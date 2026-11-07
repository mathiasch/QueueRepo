//! Light/dark: follows the system unless the user picked a theme.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    System,
    Light,
    Dark,
}

impl Theme {
    pub fn key(self) -> &'static str {
        match self {
            Theme::System => "system",
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }

    pub fn parse(s: &str) -> Theme {
        match s {
            "light" => Theme::Light,
            "dark" => Theme::Dark,
            _ => Theme::System,
        }
    }
}

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

pub fn stored() -> Theme {
    storage()
        .and_then(|s| s.get_item("theme").ok().flatten())
        .map(|v| Theme::parse(&v))
        .unwrap_or(Theme::System)
}

pub fn apply(t: Theme) {
    let Some(root) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.document_element())
    else {
        return;
    };
    let _ = match t {
        Theme::System => root.remove_attribute("data-theme"),
        other => root.set_attribute("data-theme", other.key()),
    };
}

pub fn save(t: Theme) {
    if let Some(s) = storage() {
        let _ = match t {
            Theme::System => s.remove_item("theme"),
            other => s.set_item("theme", other.key()),
        };
    }
    apply(t);
}

/// Small localStorage helpers for per-browser preferences.
pub fn get_pref(key: &str) -> Option<String> {
    storage()?.get_item(key).ok()?
}

pub fn set_pref(key: &str, value: &str) {
    if let Some(s) = storage() {
        let _ = if value.is_empty() {
            s.remove_item(key)
        } else {
            s.set_item(key, value)
        };
    }
}

#[cfg(test)]
mod tests {
    use super::Theme;

    #[test]
    fn theme_keys_roundtrip() {
        for t in [Theme::System, Theme::Light, Theme::Dark] {
            assert_eq!(Theme::parse(t.key()), t);
        }
        assert_eq!(Theme::parse("bogus"), Theme::System);
    }
}
