//! Protection settings (WP03): the text form of the protected paths.
use queued_api::gate::{default_protected, validate_protected_paths};

/// One pattern per line; blank lines and `#` comments are ignored.
pub fn parse_paths(text: &str) -> Result<Vec<String>, String> {
    let v: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect();
    validate_protected_paths(&v)?;
    Ok(v)
}

pub fn paths_text(paths: &[String]) -> String {
    paths.join("\n")
}

/// The text equals the built-in default list.
pub fn is_default(text: &str) -> bool {
    parse_paths(text).is_ok_and(|v| v == default_protected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_validates_and_detects_default() {
        assert_eq!(
            parse_paths("tests/\n\n  *.spec.ts  \n# comment\n").unwrap(),
            vec!["tests/", "*.spec.ts"]
        );
        assert!(parse_paths("../secret").unwrap_err().contains("`..`"));
        assert!(parse_paths("/abs").is_err());
        assert_eq!(parse_paths("").unwrap(), Vec::<String>::new());
        let d = paths_text(&default_protected());
        assert!(is_default(&d));
        assert!(!is_default("tests/"));
    }
}
