//! Unified diff parsing with line numbers (WP06 inline comments). Used by the
//! backend (validation, anchors, "veraltet") and the frontend (rendering,
//! line selection) – one parser, identical results.
use crate::steer::{CommentLocation, Side};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Add,
    Del,
    Context,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub kind: Kind,
    /// Line number in the old version (removed and unchanged lines).
    pub old: Option<i32>,
    /// Line number in the new version (added and unchanged lines).
    pub new: Option<i32>,
    pub text: String,
    /// Index of the hunk the line belongs to.
    pub hunk: usize,
}

impl Line {
    pub fn number(&self, side: Side) -> Option<i32> {
        match side {
            Side::New => self.new,
            Side::Old => self.old,
        }
    }

    /// The side a click on this line comments on.
    pub fn side(&self) -> Side {
        if self.kind == Kind::Del {
            Side::Old
        } else {
            Side::New
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub header: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct File {
    /// Path of the new version; of the old one for deleted files.
    pub path: String,
    /// Old path when it differs (rename) or the file was deleted.
    pub old_path: Option<String>,
    pub created: bool,
    pub deleted: bool,
    pub binary: bool,
    pub hunks: Vec<Hunk>,
    pub lines: Vec<Line>,
}

impl File {
    pub fn added(&self) -> usize {
        self.lines.iter().filter(|l| l.kind == Kind::Add).count()
    }

    pub fn removed(&self) -> usize {
        self.lines.iter().filter(|l| l.kind == Kind::Del).count()
    }
}

/// Git's C-style quoting of unusual paths (`"a/\303\244.txt"`).
fn unquote(s: &str) -> String {
    let Some(inner) = s.strip_prefix('"').and_then(|x| x.strip_suffix('"')) else {
        return s.to_string();
    };
    let mut out: Vec<u8> = vec![];
    let b = inner.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() {
            let c = b[i + 1];
            match c {
                b'n' => out.push(b'\n'),
                b't' => out.push(b'\t'),
                b'"' => out.push(b'"'),
                b'\\' => out.push(b'\\'),
                b'0'..=b'3'
                    if i + 3 < b.len()
                        && (b'0'..=b'7').contains(&b[i + 2])
                        && (b'0'..=b'7').contains(&b[i + 3]) =>
                {
                    let oct = std::str::from_utf8(&b[i + 1..i + 4]).unwrap_or("0");
                    out.push(u8::from_str_radix(oct, 8).unwrap_or(b'?'));
                    i += 4;
                    continue;
                }
                o => out.push(o),
            }
            i += 2;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn strip_side(p: &str) -> Option<String> {
    let p = unquote(p.trim_end_matches('\t'));
    if p == "/dev/null" {
        return None;
    }
    Some(
        p.strip_prefix("a/")
            .or_else(|| p.strip_prefix("b/"))
            .unwrap_or(&p)
            .to_string(),
    )
}

/// `@@ -a[,b] +c[,d] @@` → (a, c).
fn hunk_start(h: &str) -> Option<(i32, i32)> {
    let mut it = h.split_whitespace();
    if it.next()? != "@@" {
        return None;
    }
    let num = |s: &str| s.split(',').next().and_then(|n| n.parse::<i32>().ok());
    let old = num(it.next()?.strip_prefix('-')?)?;
    let new = num(it.next()?.strip_prefix('+')?)?;
    Some((old, new))
}

/// Parses `git diff` output (any context size).
pub fn parse(diff: &str) -> Vec<File> {
    let mut files: Vec<File> = vec![];
    let (mut old_no, mut new_no) = (0, 0);
    let mut in_hunk = false;
    for raw in diff.lines() {
        if let Some(rest) = raw.strip_prefix("diff --git ") {
            // Path from the header; `---`/`+++` refine it.
            let path = rest
                .rsplit_once(" b/")
                .map(|(_, b)| b.to_string())
                .unwrap_or_else(|| rest.to_string());
            files.push(File {
                path: unquote(&path),
                ..Default::default()
            });
            in_hunk = false;
            continue;
        }
        let Some(f) = files.last_mut() else { continue };
        if !in_hunk || raw.starts_with("@@") {
            if let Some(p) = raw.strip_prefix("--- ") {
                match strip_side(p) {
                    Some(o) => f.old_path = Some(o),
                    None => f.created = true,
                }
                continue;
            }
            if let Some(p) = raw.strip_prefix("+++ ") {
                match strip_side(p) {
                    Some(n) => f.path = n,
                    None => {
                        f.deleted = true;
                        if let Some(o) = &f.old_path {
                            f.path = o.clone();
                        }
                    }
                }
                continue;
            }
            if raw.starts_with("Binary files ") || raw.starts_with("GIT binary patch") {
                f.binary = true;
                continue;
            }
            if raw.starts_with("new file mode") {
                f.created = true;
                continue;
            }
            if raw.starts_with("deleted file mode") {
                f.deleted = true;
                continue;
            }
            if let Some((o, n)) = raw.starts_with("@@").then(|| hunk_start(raw)).flatten() {
                old_no = o;
                new_no = n;
                in_hunk = true;
                f.hunks.push(Hunk {
                    header: raw.to_string(),
                });
                continue;
            }
            if !in_hunk {
                continue;
            }
        }
        let hunk = f.hunks.len().saturating_sub(1);
        match raw.as_bytes().first() {
            Some(b'+') => {
                f.lines.push(Line {
                    kind: Kind::Add,
                    old: None,
                    new: Some(new_no),
                    text: raw[1..].to_string(),
                    hunk,
                });
                new_no += 1;
            }
            Some(b'-') => {
                f.lines.push(Line {
                    kind: Kind::Del,
                    old: Some(old_no),
                    new: None,
                    text: raw[1..].to_string(),
                    hunk,
                });
                old_no += 1;
            }
            Some(b' ') | None => {
                f.lines.push(Line {
                    kind: Kind::Context,
                    old: Some(old_no),
                    new: Some(new_no),
                    text: raw.get(1..).unwrap_or("").to_string(),
                    hunk,
                });
                old_no += 1;
                new_no += 1;
            }
            // "\ No newline at end of file"
            Some(b'\\') => {}
            _ => in_hunk = false,
        }
    }
    if let Some(f) = files.last_mut() {
        if f.path.is_empty() {
            files.pop();
        }
    }
    files
}

pub fn file<'a>(files: &'a [File], path: &str) -> Option<&'a File> {
    files.iter().find(|f| f.path == path)
}

/// The lines `start..=end` of one side, if all of them are in the diff and
/// belong to one hunk.
pub fn lines_of(f: &File, side: Side, start: i32, end: i32) -> Option<Vec<String>> {
    let mut out = vec![];
    let mut hunk = None;
    for n in start..=end {
        let l = f.lines.iter().find(|l| l.number(side) == Some(n))?;
        if *hunk.get_or_insert(l.hunk) != l.hunk {
            return None;
        }
        out.push(l.text.clone());
    }
    Some(out)
}

/// Where the commented lines (`anchor`, originally at `start`) are in the
/// diff now: unchanged at the same place, the same lines elsewhere (the
/// nearest unique match), or gone.
pub fn locate(
    files: &[File],
    path: &str,
    side: Side,
    anchor: &[String],
    start: i32,
) -> CommentLocation {
    let Some(f) = file(files, path) else {
        return CommentLocation::Stale;
    };
    if anchor.is_empty() {
        return CommentLocation::Stale;
    }
    let len = anchor.len() as i32;
    if lines_of(f, side, start, start + len - 1).as_deref() == Some(anchor) {
        return CommentLocation::Current;
    }
    let mut best: Option<(i32, i32)> = None; // (distance, start)
    let mut tie = false;
    for l in &f.lines {
        let Some(n) = l.number(side) else { continue };
        if l.text != anchor[0] {
            continue;
        }
        if lines_of(f, side, n, n + len - 1).as_deref() != Some(anchor) {
            continue;
        }
        let d = (n - start).abs();
        match best {
            Some((bd, _)) if d > bd => {}
            Some((bd, _)) if d == bd => tie = true,
            _ => {
                best = Some((d, n));
                tie = false;
            }
        }
    }
    match best {
        Some((_, n)) if !tie => CommentLocation::Moved {
            line_start: n,
            line_end: n + len - 1,
        },
        _ => CommentLocation::Stale,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const D1: &str = "diff --git a/src/a.rs b/src/a.rs
index 1..2 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,4 +1,5 @@
 fn main() {
-    old();
+    new();
+    more();
     done();
 }
@@ -10,2 +11,2 @@ fn x() {
 a
-b
+c
diff --git a/new.txt b/new.txt
new file mode 100644
index 0000000..3
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,2 @@
+hello
+world
\\ No newline at end of file
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
--- a/gone.txt
+++ /dev/null
@@ -1 +0,0 @@
-bye
diff --git a/img.png b/img.png
Binary files a/img.png and b/img.png differ
diff --git \"a/\\303\\244 b.txt\" \"b/\\303\\244 b.txt\"
--- \"a/\\303\\244 b.txt\"
+++ \"b/\\303\\244 b.txt\"
@@ -1 +1 @@
-x
+y
";

    #[test]
    fn parses_files_numbers_and_special_cases() {
        let f = parse(D1);
        let paths: Vec<&str> = f.iter().map(|x| x.path.as_str()).collect();
        assert_eq!(
            paths,
            ["src/a.rs", "new.txt", "gone.txt", "img.png", "ä b.txt"]
        );
        let a = &f[0];
        assert_eq!((a.added(), a.removed(), a.hunks.len()), (3, 2, 2));
        let new: Vec<(Option<i32>, Option<i32>, &str)> = a
            .lines
            .iter()
            .map(|l| (l.old, l.new, l.text.as_str()))
            .collect();
        assert_eq!(new[1], (Some(2), None, "    old();"));
        assert_eq!(new[2], (None, Some(2), "    new();"));
        assert_eq!(new[4], (Some(3), Some(4), "    done();"));
        assert_eq!(new[6], (Some(10), Some(11), "a"));
        assert_eq!(new[8], (None, Some(12), "c"));
        assert!(f[1].created && f[1].lines.len() == 2);
        assert!(f[2].deleted && f[2].lines[0].old == Some(1));
        assert!(f[3].binary && f[3].lines.is_empty());
        assert_eq!(f[4].lines[1].text, "y");
    }

    #[test]
    fn ranges_must_exist_and_stay_in_one_hunk() {
        let f = parse(D1);
        let a = &f[0];
        assert_eq!(
            lines_of(a, Side::New, 2, 3).unwrap(),
            ["    new();", "    more();"]
        );
        assert_eq!(lines_of(a, Side::Old, 2, 2).unwrap(), ["    old();"]);
        // Not in the diff, across hunks, wrong side.
        assert!(lines_of(a, Side::New, 6, 7).is_none());
        assert!(lines_of(a, Side::New, 5, 11).is_none());
        assert!(lines_of(a, Side::Old, 12, 12).is_none());
        assert_eq!(a.lines[1].side(), Side::Old);
        assert_eq!(a.lines[0].side(), Side::New);
    }

    #[test]
    fn comments_stay_move_or_go_stale() {
        let before = parse(D1);
        let anchor = lines_of(&before[0], Side::New, 2, 3).unwrap();
        assert_eq!(
            locate(&before, "src/a.rs", Side::New, &anchor, 2),
            CommentLocation::Current
        );
        // Two lines inserted above: same lines, new place.
        let moved = D1.replace(
            "@@ -1,4 +1,5 @@\n fn main() {\n",
            "@@ -1,4 +1,7 @@\n fn main() {\n+    // a\n+    // b\n",
        );
        let after = parse(&moved);
        assert_eq!(
            locate(&after, "src/a.rs", Side::New, &anchor, 2),
            CommentLocation::Moved {
                line_start: 4,
                line_end: 5
            }
        );
        // The commented line itself changed.
        let changed = parse(&D1.replace("+    more();", "+    other();"));
        assert_eq!(
            locate(&changed, "src/a.rs", Side::New, &anchor, 2),
            CommentLocation::Stale
        );
        // File no longer in the diff.
        assert_eq!(
            locate(&parse(""), "src/a.rs", Side::New, &anchor, 2),
            CommentLocation::Stale
        );
        // Two equally near copies: ambiguous → stale.
        let dup = "diff --git a/d b/d\n--- a/d\n+++ b/d\n@@ -1,0 +1,3 @@\n+x\n+y\n+x\n";
        let files = parse(dup);
        assert_eq!(
            locate(&files, "d", Side::New, &["x".to_string()], 1),
            CommentLocation::Current
        );
        assert_eq!(
            locate(&files, "d", Side::New, &["x".to_string()], 2),
            CommentLocation::Stale
        );
    }
}
