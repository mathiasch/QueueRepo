//! WP09 UI logic (native tests): line diff for rule proposals, percent
//! inputs of the dedupe thresholds, upload pre-checks, state edits.
use queued_api::memory::{Feature, FeatureStatus, ProjectState, MAX_ATTACHMENT_BYTES};

/// One line of a diff between the current rules and a proposal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLine {
    Same(String),
    Added(String),
    Removed(String),
}

/// Line diff (longest common subsequence; inputs are at most 32 KiB, so
/// the quadratic table is bounded – larger inputs fall back to
/// "everything removed, everything added").
pub fn line_diff(old: &str, new: &str) -> Vec<DiffLine> {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    if a.len() * b.len() > 4_000_000 {
        return a
            .iter()
            .map(|l| DiffLine::Removed(l.to_string()))
            .chain(b.iter().map(|l| DiffLine::Added(l.to_string())))
            .collect();
    }
    let (n, m) = (a.len(), b.len());
    let mut t = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            t[i][j] = if a[i] == b[j] {
                t[i + 1][j + 1] + 1
            } else {
                t[i + 1][j].max(t[i][j + 1])
            };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, vec![]);
    while i < n && j < m {
        if a[i] == b[j] {
            out.push(DiffLine::Same(a[i].to_string()));
            i += 1;
            j += 1;
        } else if t[i + 1][j] >= t[i][j + 1] {
            out.push(DiffLine::Removed(a[i].to_string()));
            i += 1;
        } else {
            out.push(DiffLine::Added(b[j].to_string()));
            j += 1;
        }
    }
    out.extend(a[i..].iter().map(|l| DiffLine::Removed(l.to_string())));
    out.extend(b[j..].iter().map(|l| DiffLine::Added(l.to_string())));
    out
}

/// (added, removed) lines.
pub fn diff_counts(d: &[DiffLine]) -> (usize, usize) {
    d.iter().fold((0, 0), |(a, r), l| match l {
        DiffLine::Added(_) => (a + 1, r),
        DiffLine::Removed(_) => (a, r + 1),
        DiffLine::Same(_) => (a, r),
    })
}

/// "60" or "60 %" → 0.6 (5–100 %).
pub fn parse_percent(s: &str, label: &str) -> Result<f32, String> {
    let t = s.trim().trim_end_matches('%').trim().replace(',', ".");
    let v: f32 = t
        .parse()
        .map_err(|_| format!("{label}: bitte eine Zahl zwischen 5 und 100 eingeben"))?;
    if !(5.0..=100.0).contains(&v) {
        return Err(format!(
            "{label}: bitte eine Zahl zwischen 5 und 100 eingeben"
        ));
    }
    Ok((v / 100.0 * 1000.0).round() / 1000.0)
}

/// 0.6 → "60".
pub fn percent(v: f32) -> String {
    format!("{}", (v * 100.0).round() as i32)
}

/// Checks before an upload starts (the server checks again).
pub fn precheck(name: &str, size: f64, already: usize) -> Result<(), String> {
    if already >= queued_api::memory::MAX_ATTACHMENTS_PER_TASK {
        return Err(format!(
            "Höchstens {} Anhänge pro Auftrag.",
            queued_api::memory::MAX_ATTACHMENTS_PER_TASK
        ));
    }
    if size > MAX_ATTACHMENT_BYTES as f64 {
        return Err(format!(
            "„{name}“ ist zu groß ({}, höchstens {}).",
            queued_api::memory::file_size(size as i64),
            queued_api::memory::file_size(MAX_ATTACHMENT_BYTES as i64)
        ));
    }
    if size <= 0.0 {
        return Err(format!("„{name}“ ist leer."));
    }
    Ok(())
}

/// Feature id from a title ("Login per Passkey" → "login-per-passkey"),
/// unique in `state`.
pub fn new_feature_id(title: &str, state: &ProjectState) -> String {
    let mut id = String::new();
    for c in title.to_lowercase().chars() {
        let rep = match c {
            'ä' => "ae",
            'ö' => "oe",
            'ü' => "ue",
            'ß' => "ss",
            c if c.is_ascii_alphanumeric() => {
                id.push(c);
                continue;
            }
            _ => "-",
        };
        if rep == "-" {
            if !id.ends_with('-') && !id.is_empty() {
                id.push('-');
            }
        } else {
            id.push_str(rep);
        }
    }
    let base: String = id.trim_matches('-').chars().take(48).collect();
    let base = if base.is_empty() {
        "eintrag".to_string()
    } else {
        base.trim_end_matches('-').to_string()
    };
    let mut cand = base.clone();
    let mut n = 2;
    while state.feature(&cand).is_some() {
        cand = format!("{base}-{n}");
        n += 1;
    }
    cand
}

/// Adds a feature (human edit). `Err` = German message.
pub fn add_feature(state: &mut ProjectState, title: &str) -> Result<(), String> {
    let t = title.trim();
    if t.is_empty() {
        return Err("Bitte einen Namen eingeben.".into());
    }
    if t.chars().count() > queued_api::memory::MAX_TITLE {
        return Err("Der Name ist zu lang (höchstens 200 Zeichen).".into());
    }
    let id = new_feature_id(t, state);
    state.features.push(Feature {
        id,
        title: t.to_string(),
        status: FeatureStatus::Todo,
        passes: false,
        files: vec![],
        steps: vec![],
        notes: String::new(),
    });
    Ok(())
}

/// Sets a status; leaving "done" clears `passes`.
pub fn set_status(state: &mut ProjectState, id: &str, status: FeatureStatus) {
    if let Some(f) = state.features.iter_mut().find(|f| f.id == id) {
        f.status = status;
        if status != FeatureStatus::Done {
            f.passes = false;
        }
    }
}

/// Counts per status for the summary line.
pub fn status_counts(state: &ProjectState) -> Vec<(FeatureStatus, usize)> {
    FeatureStatus::ALL
        .iter()
        .map(|s| (*s, state.features.iter().filter(|f| f.status == *s).count()))
        .filter(|(_, n)| *n > 0)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_marks_added_and_removed_lines() {
        let d = line_diff("a\nb\nc", "a\nB\nc\nd");
        assert_eq!(
            d,
            vec![
                DiffLine::Same("a".into()),
                DiffLine::Removed("b".into()),
                DiffLine::Added("B".into()),
                DiffLine::Same("c".into()),
                DiffLine::Added("d".into()),
            ]
        );
        assert_eq!(diff_counts(&d), (2, 1));
        assert_eq!(line_diff("", "x"), vec![DiffLine::Added("x".into())]);
        assert_eq!(diff_counts(&line_diff("same", "same")), (0, 0));
    }

    #[test]
    fn percent_inputs() {
        assert_eq!(parse_percent("60", "x").unwrap(), 0.6);
        assert_eq!(parse_percent(" 35 % ", "x").unwrap(), 0.35);
        assert_eq!(parse_percent("12,5", "x").unwrap(), 0.125);
        assert!(parse_percent("4", "Duplikat")
            .unwrap_err()
            .starts_with("Duplikat:"));
        assert!(parse_percent("101", "x").is_err());
        assert!(parse_percent("viel", "x").is_err());
        assert_eq!(percent(0.6), "60");
        assert_eq!(percent(0.35), "35");
    }

    #[test]
    fn upload_prechecks() {
        assert!(precheck("a.png", 1000.0, 0).is_ok());
        assert!(precheck("a.png", (MAX_ATTACHMENT_BYTES + 1) as f64, 0)
            .unwrap_err()
            .contains("zu groß"));
        assert!(precheck("a.png", 10.0, 10)
            .unwrap_err()
            .contains("Höchstens 10"));
        assert!(precheck("a.png", 0.0, 0).unwrap_err().contains("leer"));
    }

    #[test]
    fn state_edits() {
        let mut s = ProjectState::default();
        add_feature(&mut s, "Login per Passkey").unwrap();
        add_feature(&mut s, "Login per Passkey").unwrap();
        assert_eq!(s.features[0].id, "login-per-passkey");
        assert_eq!(s.features[1].id, "login-per-passkey-2");
        add_feature(&mut s, "Größe ändern!").unwrap();
        assert_eq!(s.features[2].id, "groesse-aendern");
        add_feature(&mut s, "日本").unwrap();
        assert_eq!(s.features[3].id, "eintrag");
        assert!(add_feature(&mut s, "  ").is_err());
        assert!(add_feature(&mut s, &"x".repeat(201)).is_err());
        for f in &s.features {
            assert!(queued_api::memory::valid_feature_id(&f.id), "{}", f.id);
        }
        s.features[0].passes = true;
        set_status(&mut s, "login-per-passkey", FeatureStatus::Done);
        assert!(s.features[0].passes);
        set_status(&mut s, "login-per-passkey", FeatureStatus::Blocked);
        assert!(!s.features[0].passes);
        assert_eq!(
            status_counts(&s),
            vec![(FeatureStatus::Todo, 3), (FeatureStatus::Blocked, 1)]
        );
    }
}
