//! Unified diff presentation.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    File,
    Hunk,
    Add,
    Del,
    Context,
}

impl LineKind {
    pub fn class(self) -> &'static str {
        match self {
            LineKind::File => "d-file",
            LineKind::Hunk => "d-hunk",
            LineKind::Add => "d-add",
            LineKind::Del => "d-del",
            LineKind::Context => "d-ctx",
        }
    }
}

pub fn classify(line: &str) -> LineKind {
    if line.starts_with("diff ")
        || line.starts_with("+++")
        || line.starts_with("---")
        || line.starts_with("index ")
        || line.starts_with("new file mode")
        || line.starts_with("deleted file mode")
    {
        LineKind::File
    } else if line.starts_with("@@") {
        LineKind::Hunk
    } else if line.starts_with('+') {
        LineKind::Add
    } else if line.starts_with('-') {
        LineKind::Del
    } else {
        LineKind::Context
    }
}

/// Changed file with added/removed line counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStat {
    pub path: String,
    pub added: usize,
    pub removed: usize,
}

/// Files of a `git diff` output, in order.
pub fn file_stats(diff: &str) -> Vec<FileStat> {
    let mut out: Vec<FileStat> = vec![];
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let path = rest
                .rsplit_once(" b/")
                .map(|(_, b)| b.to_string())
                .unwrap_or_else(|| rest.to_string());
            out.push(FileStat {
                path,
                added: 0,
                removed: 0,
            });
            continue;
        }
        let Some(f) = out.last_mut() else { continue };
        match classify(line) {
            LineKind::Add => f.added += 1,
            LineKind::Del => f.removed += 1,
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "diff --git a/MOCK_LOG.md b/MOCK_LOG.md\nindex 1..2 100644\n--- a/MOCK_LOG.md\n+++ b/MOCK_LOG.md\n@@ -1 +1,2 @@\n run 1\n+run 2\ndiff --git a/src/x.rs b/src/x.rs\nnew file mode 100644\n--- /dev/null\n+++ b/src/x.rs\n@@ -0,0 +1 @@\n+fn x() {}\n-old\n";

    #[test]
    fn classifies_lines() {
        assert_eq!(classify("+++ b/x"), LineKind::File);
        assert_eq!(classify("--- a/x"), LineKind::File);
        assert_eq!(classify("@@ -1 +1 @@"), LineKind::Hunk);
        assert_eq!(classify("+added"), LineKind::Add);
        assert_eq!(classify("-removed"), LineKind::Del);
        assert_eq!(classify(" same"), LineKind::Context);
        assert_eq!(classify(""), LineKind::Context);
        assert_eq!(LineKind::Add.class(), "d-add");
    }

    #[test]
    fn counts_changes_per_file() {
        assert_eq!(
            file_stats(DIFF),
            vec![
                FileStat {
                    path: "MOCK_LOG.md".into(),
                    added: 1,
                    removed: 0
                },
                FileStat {
                    path: "src/x.rs".into(),
                    added: 1,
                    removed: 1
                },
            ]
        );
        assert!(file_stats("").is_empty());
    }
}
