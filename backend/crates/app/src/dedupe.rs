//! WP09: OnEmpty dedupe and critic (report P1.9).
//!
//! Before OnEmpty proposals become tasks, every proposal is screened:
//! 1. **Critic rules** (deterministic): too short to run without asking,
//!    very long, touches CI/tests/secrets/deployment/history ⇒ review.
//! 2. **Similarity** against the project's open tasks, finished tasks of
//!    the window (done, failed, blocked, cancelled = also rejected
//!    proposals) and reverted tasks, and against earlier proposals of the
//!    same run: normalized word tokens + word bigrams of title and prompt,
//!    and character 3-grams of the title, Jaccard each, the highest score
//!    counts. No embedding API, no network: the same input always gives
//!    the same verdict.
//! 3. Optional model critic (runner.rs, only ever stricter).
//!
//! `score ≥ dedupe_threshold` ⇒ dropped; `≥ dedupe_review_threshold` ⇒
//! needs approval; similar to a reverted task from the review threshold on
//! ⇒ dropped (the same "improvement" must not come back every day).
use std::collections::HashSet;

use queued_api::memory::{Screening, SimilarTask, Verdict};
use uuid::Uuid;

use crate::{
    models::{FollowUp, Settings},
    state::AppState,
};

/// Words without meaning for similarity (English + German).
const STOPWORDS: [&str; 64] = [
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "in", "into", "is", "it", "of",
    "on", "or", "that", "the", "this", "to", "with", "all", "any", "add", "new", "make", "use",
    "der", "die", "das", "den", "dem", "des", "ein", "eine", "einen", "einem", "und", "oder", "im",
    "in", "mit", "von", "zu", "zum", "zur", "für", "auf", "ist", "sind", "werden", "wird", "als",
    "bei", "nach", "aus", "über", "neue", "neuen", "alle", "please", "bitte", "should",
];

/// Lower-cased word tokens with light suffix stemming.
pub fn tokens(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .filter(|w| w.chars().count() >= 2 && !STOPWORDS.contains(&w.as_str()))
        .map(|w| stem(&w))
        .collect()
}

fn stem(w: &str) -> String {
    if w.chars().count() <= 4 {
        return w.to_string();
    }
    for suf in [
        "ungen", "ung", "ing", "ies", "es", "en", "er", "ed", "s", "e",
    ] {
        if let Some(base) = w.strip_suffix(suf) {
            if base.chars().count() >= 3 {
                return base.to_string();
            }
        }
    }
    w.to_string()
}

fn bigrams(t: &[String]) -> HashSet<String> {
    t.windows(2).map(|w| format!("{} {}", w[0], w[1])).collect()
}

fn char3(s: &str) -> HashSet<String> {
    let norm: String = s
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let chars: Vec<char> = format!(" {norm} ").chars().collect();
    chars.windows(3).map(|w| w.iter().collect()).collect()
}

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count();
    inter as f32 / (a.len() + b.len() - inter) as f32
}

/// Precomputed features of one task text.
#[derive(Debug, Clone)]
pub struct Doc {
    title3: HashSet<String>,
    words: HashSet<String>,
    pairs: HashSet<String>,
}

impl Doc {
    pub fn new(title: &str, prompt: &str) -> Self {
        let t = tokens(&format!("{title}\n{}", cut(prompt, 4000)));
        Doc {
            title3: char3(title),
            words: t.iter().cloned().collect(),
            pairs: bigrams(&t),
        }
    }
}

/// Similarity 0..1 (the higher of title similarity and text similarity).
pub fn similarity(a: &Doc, b: &Doc) -> f32 {
    let title = jaccard(&a.title3, &b.title3);
    let text = (jaccard(&a.words, &b.words) + jaccard(&a.pairs, &b.pairs)) / 2.0;
    title.max(text).clamp(0.0, 1.0)
}

/// An existing task compared against.
#[derive(Debug, Clone)]
pub struct Existing {
    pub id: Uuid,
    pub title: String,
    pub status: String,
    pub reverted: bool,
    pub doc: Doc,
}

/// Critic rules on the text alone: `Some((verdict, reason))`.
pub fn critic_rules(title: &str, prompt: &str) -> Vec<(Verdict, String)> {
    let mut out = vec![];
    let p = prompt.trim();
    if p.chars().count() < 30 {
        out.push((
            Verdict::Dropped,
            "Zu knapp beschrieben, um ohne Rückfrage ausgeführt zu werden".to_string(),
        ));
    }
    if p.chars().count() > 8000 {
        out.push((
            Verdict::Review,
            "Sehr umfangreicher Auftrag – besser von einem Menschen zuschneiden lassen".to_string(),
        ));
    }
    let text = format!("{title}\n{p}").to_lowercase();
    const SENSITIVE: [(&str, &str); 18] = [
        (".github/workflows", "CI-Konfiguration"),
        ("gitlab-ci", "CI-Konfiguration"),
        ("ci pipeline", "CI-Konfiguration"),
        ("secret", "Geheimnisse"),
        ("credential", "Zugangsdaten"),
        ("password", "Passwörter"),
        ("passwort", "Passwörter"),
        ("api key", "Zugangsdaten"),
        ("disable test", "Tests abschalten"),
        ("skip test", "Tests überspringen"),
        ("remove test", "Tests entfernen"),
        ("delete test", "Tests löschen"),
        ("tests entfernen", "Tests entfernen"),
        ("tests löschen", "Tests löschen"),
        ("force push", "Git-Verlauf umschreiben"),
        ("rewrite history", "Git-Verlauf umschreiben"),
        ("deploy", "Deployment"),
        ("production", "Produktion"),
    ];
    let mut hits: Vec<&str> = SENSITIVE
        .iter()
        .filter(|(k, _)| text.contains(k))
        .map(|(_, l)| *l)
        .collect();
    hits.dedup();
    if !hits.is_empty() {
        out.push((
            Verdict::Review,
            format!(
                "Berührt {} – das soll ein Mensch entscheiden",
                hits.join(", ")
            ),
        ));
    }
    out
}

/// Screens proposals (pure: everything it needs is passed in). Returns one
/// screening per proposal, in order.
pub fn screen_pure(
    items: &[FollowUp],
    existing: &[Existing],
    threshold: f32,
    review: f32,
) -> Vec<Screening> {
    let mut accepted: Vec<(String, Doc)> = vec![];
    let mut out = vec![];
    for f in items {
        let title = display_title(f);
        let doc = Doc::new(&title, &f.prompt);
        let mut verdict = Verdict::Accepted;
        let mut reasons = vec![];
        for (v, r) in critic_rules(&title, &f.prompt) {
            verdict = stricter(verdict, v);
            reasons.push(r);
        }
        // Most similar existing task.
        let best = existing
            .iter()
            .map(|e| (similarity(&doc, &e.doc), e))
            .max_by(|a, b| a.0.total_cmp(&b.0));
        let similar = best.filter(|(s, _)| *s > 0.0).map(|(s, e)| SimilarTask {
            task_id: e.id,
            title: e.title.clone(),
            status: e.status.clone(),
            score: (s * 100.0).round() / 100.0,
            reverted: e.reverted,
        });
        if let Some(sim) = &similar {
            let pct = (sim.score * 100.0).round() as i32;
            if sim.reverted && sim.score >= review {
                verdict = Verdict::Dropped;
                reasons.push(format!(
                    "Ähnlich ({pct} %) zu „{}“, dessen Änderungen rückgängig gemacht wurden",
                    sim.title
                ));
            } else if sim.score >= threshold {
                verdict = Verdict::Dropped;
                reasons.push(format!(
                    "Duplikat: {pct} % ähnlich zu „{}“ ({})",
                    sim.title,
                    status_label(&sim.status)
                ));
            } else if sim.score >= review {
                verdict = stricter(verdict, Verdict::Review);
                reasons.push(format!(
                    "Ähnlich ({pct} %) zu „{}“ ({}) – bitte prüfen, ob das schon erledigt ist",
                    sim.title,
                    status_label(&sim.status)
                ));
            }
        }
        // Earlier proposals of the same run.
        if verdict != Verdict::Dropped {
            if let Some((t, s)) = accepted
                .iter()
                .map(|(t, d)| (t, similarity(&doc, d)))
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .filter(|(_, s)| *s >= threshold)
            {
                verdict = Verdict::Dropped;
                reasons.push(format!(
                    "Doppelt im selben Vorschlag ({} % ähnlich zu „{t}“)",
                    (s * 100.0).round() as i32
                ));
            }
        }
        if verdict != Verdict::Dropped {
            accepted.push((title.clone(), doc));
        }
        if reasons.is_empty() {
            reasons.push(match &similar {
                Some(s) => format!(
                    "Keine Bedenken; ähnlichster Auftrag „{}“ mit {} %",
                    s.title,
                    (s.score * 100.0).round() as i32
                ),
                None => "Keine Bedenken; kein ähnlicher Auftrag gefunden".into(),
            });
        }
        out.push(Screening {
            title,
            verdict,
            reasons,
            similar,
            task_id: None,
            critic: "rules".into(),
        });
    }
    out
}

/// The stricter of two verdicts.
pub fn stricter(a: Verdict, b: Verdict) -> Verdict {
    let rank = |v: Verdict| match v {
        Verdict::Accepted => 0,
        Verdict::Review => 1,
        Verdict::Dropped => 2,
    };
    if rank(b) > rank(a) {
        b
    } else {
        a
    }
}

pub fn display_title(f: &FollowUp) -> String {
    if f.title.trim().is_empty() {
        f.prompt
            .lines()
            .next()
            .unwrap_or("Folgeauftrag")
            .chars()
            .take(120)
            .collect()
    } else {
        f.title.trim().chars().take(200).collect()
    }
}

fn status_label(s: &str) -> &'static str {
    match s {
        "done" => "erledigt",
        "cancelled" => "abgebrochen oder abgelehnt",
        "failed" | "blocked" => "gescheitert",
        "pending_approval" => "wartet auf Freigabe",
        "running" => "läuft",
        _ => "offen",
    }
}

fn cut(s: &str, n: usize) -> &str {
    if s.len() <= n {
        return s;
    }
    let mut e = n;
    while !s.is_char_boundary(e) {
        e -= 1;
    }
    &s[..e]
}

/// Tasks of the project to compare against: all open ones, finished ones
/// within the window, reverted ones (any age). OnEmpty runs themselves are
/// not compared (their titles are generic).
pub async fn existing(st: &AppState, project_id: Uuid, days: i32) -> sqlx::Result<Vec<Existing>> {
    let rows: Vec<(Uuid, String, String, String, bool)> = sqlx::query_as(
        "SELECT id, title, prompt, status, reverted_at IS NOT NULL FROM tasks
          WHERE project_id=$1 AND kind <> 'onempty'
            AND (status NOT IN ('done','failed','blocked','cancelled')
                 OR reverted_at IS NOT NULL
                 OR COALESCE(finished_at, updated_at) > now() - make_interval(days => $2))
          ORDER BY created_at DESC LIMIT 1000",
    )
    .bind(project_id)
    .bind(days)
    .fetch_all(&st.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, title, prompt, status, reverted)| Existing {
            doc: Doc::new(&title, &prompt),
            id,
            title,
            status,
            reverted,
        })
        .collect())
}

/// Screens with the user's thresholds.
pub async fn screen(
    st: &AppState,
    project_id: Uuid,
    settings: &Settings,
    items: &[FollowUp],
) -> sqlx::Result<Vec<Screening>> {
    let ex = existing(st, project_id, settings.dedupe_window_days).await?;
    Ok(screen_pure(
        items,
        &ex,
        settings.dedupe_threshold,
        settings
            .dedupe_review_threshold
            .min(settings.dedupe_threshold),
    ))
}

/// Reverted tasks of the project (negative examples for the OnEmpty prompt).
pub async fn reverted_titles(st: &AppState, project_id: Uuid) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT title FROM tasks WHERE project_id=$1 AND reverted_at IS NOT NULL
          ORDER BY reverted_at DESC LIMIT 10",
    )
    .bind(project_id)
    .fetch_all(&st.db)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ex(title: &str, prompt: &str, status: &str, reverted: bool) -> Existing {
        Existing {
            id: Uuid::new_v4(),
            title: title.into(),
            status: status.into(),
            reverted,
            doc: Doc::new(title, prompt),
        }
    }

    fn fu(title: &str, prompt: &str) -> FollowUp {
        FollowUp {
            title: title.into(),
            prompt: prompt.into(),
        }
    }

    #[test]
    fn jaccard_and_similarity_are_symmetric_and_bounded() {
        let a: HashSet<String> = ["x", "y"].iter().map(|s| s.to_string()).collect();
        let b: HashSet<String> = ["y", "z"].iter().map(|s| s.to_string()).collect();
        assert!((jaccard(&a, &b) - 1.0 / 3.0).abs() < 1e-6);
        assert_eq!(jaccard(&a, &a), 1.0);
        assert_eq!(jaccard(&a, &HashSet::new()), 0.0);
        let d1 = Doc::new(
            "Add unit tests for the parser",
            "Write unit tests covering the CSV parser edge cases.",
        );
        let d2 = Doc::new(
            "Write parser unit tests",
            "Cover the CSV parser edge cases with unit tests.",
        );
        let d3 = Doc::new(
            "Upgrade the logging library",
            "Bump tracing to the latest version and fix deprecations.",
        );
        let s12 = similarity(&d1, &d2);
        assert_eq!(s12, similarity(&d2, &d1));
        assert!(s12 > 0.5, "{s12}");
        assert!(similarity(&d1, &d3) < 0.15, "{}", similarity(&d1, &d3));
        assert_eq!(similarity(&d1, &d1), 1.0);
    }

    #[test]
    fn stemming_and_stopwords() {
        assert_eq!(
            tokens("Adding the Tests for parsing"),
            vec!["add", "test", "pars"]
        );
        assert_eq!(
            tokens("Die Validierung der Eingaben"),
            vec!["validier", "eingab"]
        );
        assert_eq!(tokens("a b ok"), vec!["ok"]);
    }

    #[test]
    fn duplicates_are_dropped_similar_need_review_reverted_are_negative() {
        let existing = vec![
            ex(
                "Add unit tests for the CSV parser",
                "Write unit tests covering the CSV parser edge cases (quotes, empty lines).",
                "done",
                false,
            ),
            ex(
                "Refactor the config loader",
                "Split config loading into smaller functions and add error context.",
                "queued",
                false,
            ),
            ex(
                "Switch to async logging",
                "Replace the logger with an async one to speed up requests.",
                "done",
                true,
            ),
        ];
        let items = vec![
            // Same work again.
            fu("Add unit tests for CSV parser", "Write unit tests covering the CSV parser edge cases like quotes and empty lines."),
            // Related, not identical.
            fu("Improve config loader errors", "Add error context to the config loader and document the config fields."),
            // A reverted idea comes back.
            fu("Use async logging", "Replace the logger with an asynchronous logger for faster requests."),
            // Unrelated, fine.
            fu("Document the HTTP API", "Write a short reference of all HTTP endpoints with request and response examples."),
            // Same as the previous proposal of this run.
            fu("Document the HTTP API endpoints", "Write a short reference of all HTTP endpoints with request and response examples."),
            // Too vague.
            fu("Fix stuff", "Fix it."),
            // Sensitive.
            fu("Speed up CI", "Skip tests that are slow in .github/workflows/ci.yml to make CI faster for everyone."),
        ];
        let s = screen_pure(&items, &existing, 0.6, 0.35);
        let v: Vec<Verdict> = s.iter().map(|x| x.verdict).collect();
        assert_eq!(
            v,
            vec![
                Verdict::Dropped,
                Verdict::Review,
                Verdict::Dropped,
                Verdict::Accepted,
                Verdict::Dropped,
                Verdict::Dropped,
                Verdict::Review
            ],
            "{s:#?}"
        );
        assert!(
            s[0].reasons[0].starts_with("Duplikat:"),
            "{:?}",
            s[0].reasons
        );
        assert_eq!(
            s[0].similar.as_ref().unwrap().title,
            "Add unit tests for the CSV parser"
        );
        assert!(s[1].reasons.iter().any(|r| r.contains("bitte prüfen")));
        assert!(s[2].reasons.iter().any(|r| r.contains("rückgängig")));
        assert!(s[3].reasons[0].starts_with("Keine Bedenken"));
        assert!(s[4]
            .reasons
            .iter()
            .any(|r| r.contains("Doppelt im selben Vorschlag")));
        assert!(s[5].reasons.iter().any(|r| r.contains("Zu knapp")));
        assert!(s[6].reasons.iter().any(|r| r.contains("CI-Konfiguration")));
        // Deterministic.
        assert_eq!(s, screen_pure(&items, &existing, 0.6, 0.35));
        // Thresholds are honoured: with a strict threshold nothing is a duplicate.
        let lax = screen_pure(&items[..1], &existing, 1.0, 1.0);
        assert_eq!(lax[0].verdict, Verdict::Accepted);
    }
}
