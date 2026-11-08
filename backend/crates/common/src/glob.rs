//! Minimal path globs for protected paths and file lists (WP03).
//!
//! Syntax (relative to the repository root, `/` separated):
//! * `*` matches any characters except `/`, `?` exactly one such character,
//! * `**` as a whole segment matches zero or more segments,
//! * a pattern without `/` matches the file name at any depth
//!   (`*.spec.ts` = `**/*.spec.ts`), like `.gitignore`,
//! * a trailing `/` means "everything below this directory"; without another
//!   `/` the directory may be at any depth (`tests/` = `**/tests/**`).
//!
//! No character classes or negation: every pattern is easy to read in the UI.

/// Longest accepted pattern (UI/API validation).
pub const MAX_PATTERN_LEN: usize = 200;

/// Validation error for the UI/API, `None` = fine.
pub fn invalid(pattern: &str) -> Option<&'static str> {
    let p = pattern.trim();
    if p.is_empty() {
        return Some("leeres Muster");
    }
    if p.len() > MAX_PATTERN_LEN {
        return Some("Muster zu lang (höchstens 200 Zeichen)");
    }
    if p.starts_with('/') || p.contains('\\') {
        return Some("Pfade relativ zum Repository mit `/` angeben");
    }
    if p.split('/').any(|s| s == "..") {
        return Some("`..` ist nicht erlaubt");
    }
    if p.chars().any(|c| c.is_control()) {
        return Some("Steuerzeichen sind nicht erlaubt");
    }
    None
}

fn normalize(pattern: &str) -> String {
    let p = pattern.trim().trim_start_matches("./");
    let (base, dir) = match p.strip_suffix('/') {
        Some(d) => (d, true),
        None => (p, false),
    };
    let mut p = base.to_string();
    if !p.contains('/') {
        p = format!("**/{p}");
    }
    if dir {
        p.push_str("/**");
    }
    p
}

/// Does `path` (repository-relative, `/` separated) match `pattern`?
pub fn matches(pattern: &str, path: &str) -> bool {
    if invalid(pattern).is_some() {
        return false;
    }
    let pat = normalize(pattern);
    let ps: Vec<&str> = pat.split('/').filter(|s| !s.is_empty()).collect();
    let xs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    match_segments(&ps, &xs)
}

/// Matches any of the patterns.
pub fn matches_any<S: AsRef<str>>(patterns: &[S], path: &str) -> bool {
    patterns.iter().any(|p| matches(p.as_ref(), path))
}

fn match_segments(ps: &[&str], xs: &[&str]) -> bool {
    match ps.split_first() {
        None => xs.is_empty(),
        Some((&"**", rest)) => (0..=xs.len()).any(|i| match_segments(rest, &xs[i..])),
        Some((p, rest)) => match xs.split_first() {
            Some((x, xrest)) => {
                match_segment(p.as_bytes(), x.as_bytes()) && match_segments(rest, xrest)
            }
            None => false,
        },
    }
}

/// `*`/`?` within one segment (iterative with backtracking on the last `*`).
fn match_segment(p: &[u8], s: &[u8]) -> bool {
    let (mut pi, mut si) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while si < s.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == s[si]) && p[pi] != b'*' {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some((pi, si));
            pi += 1;
        } else if let Some((sp, ss)) = star {
            pi = sp + 1;
            si = ss + 1;
            star = Some((sp, ss + 1));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basename_patterns_match_at_any_depth() {
        assert!(matches("*.spec.ts", "a.spec.ts"));
        assert!(matches("*.spec.ts", "web/src/a.spec.ts"));
        assert!(!matches("*.spec.ts", "web/src/a.ts"));
        assert!(matches("conftest.py", "pkg/tests/conftest.py"));
        assert!(matches("test_*.py", "x/test_core.py"));
        assert!(!matches("test_*.py", "x/contest_core.py"));
        assert!(matches("Jenkinsfile", "Jenkinsfile"));
    }

    #[test]
    fn directory_and_double_star() {
        assert!(matches("tests/", "tests/a.rs"));
        assert!(matches("tests/", "tests/deep/a.rs"));
        assert!(!matches("tests/", "src/tests.rs"));
        assert!(matches("tests/", "pkg/tests/a.rs"));
        assert!(!matches("web/tests/", "tests/a.rs"));
        assert!(matches("web/tests/", "web/tests/a.rs"));
        assert!(matches("tests/**", "tests/x/y/z.py"));
        assert!(matches(".github/workflows/**", ".github/workflows/ci.yml"));
        assert!(!matches(".github/workflows/**", ".github/dependabot.yml"));
        assert!(matches("**/fixtures/**", "a/fixtures/b/c.json"));
        assert!(matches("**/fixtures/**", "fixtures/c.json"));
        assert!(matches("src/**/*.test.ts", "src/a.test.ts"));
        assert!(matches("src/**/*.test.ts", "src/x/y/a.test.ts"));
        assert!(!matches("src/*.rs", "src/x/a.rs"));
        assert!(matches("a?c.txt", "abc.txt"));
        assert!(!matches("a?c.txt", "ac.txt"));
    }

    #[test]
    fn star_backtracking() {
        assert!(match_segment(b"*a*b*c", b"xxaxxbxxc"));
        assert!(!match_segment(b"*a*b*c", b"xxaxxcxxb"));
        assert!(match_segment(b"*", b""));
        assert!(match_segment(b"**", b"abc"));
    }

    #[test]
    fn validation() {
        assert!(invalid("tests/**").is_none());
        assert!(invalid("").is_some());
        assert!(invalid("/abs").is_some());
        assert!(invalid("a/../b").is_some());
        assert!(invalid("a\\b").is_some());
        assert!(invalid(&"a".repeat(201)).is_some());
        // Invalid patterns never match (fail-safe for the caller's own check).
        assert!(!matches("../x", "x"));
        assert!(matches_any(&["x", "y/"], "y/z"));
        assert!(!matches_any::<&str>(&[], "y/z"));
    }
}
