//! WP12 Preview: dev server in the task's sandbox, reached only through the
//! app's authenticated proxy on an own origin; screenshots after UI changes.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Viewports of the screenshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Viewport {
    Desktop,
    Mobile,
}

impl Viewport {
    pub const ALL: [Viewport; 2] = [Viewport::Desktop, Viewport::Mobile];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Mobile => "mobile",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }

    /// CSS pixels of the browser window.
    pub fn size(self) -> (u32, u32) {
        match self {
            Self::Desktop => (1280, 800),
            Self::Mobile => (390, 844),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Desktop => "Desktop",
            Self::Mobile => "Mobil",
        }
    }
}

/// Defaults of a new project.
pub const DEFAULT_UI_GLOBS: [&str; 9] = [
    "**/*.html",
    "**/*.css",
    "**/*.scss",
    "**/*.js",
    "**/*.jsx",
    "**/*.ts",
    "**/*.tsx",
    "**/*.vue",
    "**/*.svelte",
];
pub const MAX_SCREENSHOT_PATHS: usize = 5;
pub const MAX_UI_GLOBS: usize = 30;

/// `GET/PUT /api/projects/{id}/preview`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewSettings {
    pub enabled: bool,
    /// Shell command that starts the dev server in the task's working tree
    /// (it must listen on `$PORT`, which equals `port`).
    pub command: String,
    pub port: u16,
    /// Path that answers once the server is ready (status < 500).
    pub ready_path: String,
    pub ready_timeout_secs: u32,
    /// The server is stopped at the latest after this many seconds.
    pub max_lifetime_secs: u32,
    /// Screenshots after a run that changed UI files.
    pub screenshots: bool,
    pub screenshot_paths: Vec<String>,
    /// Files that count as UI changes.
    pub ui_globs: Vec<String>,
}

impl Default for PreviewSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            command: String::new(),
            port: 3000,
            ready_path: "/".into(),
            ready_timeout_secs: 60,
            max_lifetime_secs: 900,
            screenshots: true,
            screenshot_paths: vec!["/".into()],
            ui_globs: DEFAULT_UI_GLOBS.iter().map(|s| s.to_string()).collect(),
        }
    }
}

fn path_ok(p: &str) -> bool {
    p.starts_with('/')
        && p.len() <= 300
        && !p.contains("//")
        && p.chars().all(|c| {
            c.is_ascii_graphic() && c != '\\' && c != '"' && c != '\'' && c != '<' && c != '>'
        })
}

impl PreviewSettings {
    /// German error for the UI (also checked by the server).
    pub fn validate(&self) -> Result<(), String> {
        if self.enabled && self.command.trim().is_empty() {
            return Err("Bitte ein Startkommando für den Dev-Server angeben.".into());
        }
        if self.command.len() > 2000 || self.command.contains('\0') {
            return Err("Das Startkommando ist zu lang.".into());
        }
        if self.port < 1024 {
            return Err("Der Port muss zwischen 1024 und 65535 liegen.".into());
        }
        if !path_ok(&self.ready_path) {
            return Err("Der Bereit-Pfad muss mit / beginnen (z. B. /).".into());
        }
        if !(5..=600).contains(&self.ready_timeout_secs) {
            return Err("Die Startzeit muss zwischen 5 und 600 Sekunden liegen.".into());
        }
        if !(60..=3600).contains(&self.max_lifetime_secs) {
            return Err("Die Laufzeit muss zwischen 60 und 3600 Sekunden liegen.".into());
        }
        if self.screenshot_paths.len() > MAX_SCREENSHOT_PATHS {
            return Err(format!(
                "Höchstens {MAX_SCREENSHOT_PATHS} Seiten für Screenshots."
            ));
        }
        if let Some(p) = self.screenshot_paths.iter().find(|p| !path_ok(p)) {
            return Err(format!(
                "Ungültiger Seitenpfad „{p}“ (muss mit / beginnen)."
            ));
        }
        if self.screenshots && self.enabled && self.screenshot_paths.is_empty() {
            return Err("Für Screenshots mindestens eine Seite angeben (z. B. /).".into());
        }
        if self.ui_globs.len() > MAX_UI_GLOBS
            || self
                .ui_globs
                .iter()
                .any(|g| g.trim().is_empty() || g.len() > 200 || g.starts_with('/'))
        {
            return Err(format!(
                "Dateimuster: höchstens {MAX_UI_GLOBS}, relativ, nicht leer (z. B. **/*.css)."
            ));
        }
        Ok(())
    }
}

/// `GET /api/projects/{id}/preview`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewConfig {
    pub settings: PreviewSettings,
    /// Live preview possible (the operator configured `PREVIEW_ORIGIN`).
    pub live_available: bool,
}

/// State of a live preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewStatus {
    Starting,
    Ready,
    Stopped,
    Failed,
}

impl PreviewStatus {
    pub const ALL: [PreviewStatus; 4] = [
        PreviewStatus::Starting,
        PreviewStatus::Ready,
        PreviewStatus::Stopped,
        PreviewStatus::Failed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Ready => "ready",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }
}

/// `GET/POST /api/tasks/{id}/preview`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreviewView {
    pub id: Uuid,
    pub task_id: Uuid,
    pub status: PreviewStatus,
    /// Own origin of this preview (only for project members, only while it runs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub expires_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A screenshot of a task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScreenshotView {
    pub id: Uuid,
    pub viewport: Viewport,
    pub path: String,
    pub width: u32,
    pub height: u32,
    /// API path of the PNG (`/api/tasks/{task}/screenshots/{id}`).
    pub url: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_validation() {
        let mut s = PreviewSettings {
            enabled: true,
            command: "npm run dev".into(),
            ..Default::default()
        };
        assert!(s.validate().is_ok());
        s.port = 80;
        assert!(s.validate().unwrap_err().contains("Port"));
        s.port = 5173;
        s.ready_path = "health".into();
        assert!(s.validate().is_err());
        s.ready_path = "/health".into();
        s.screenshot_paths = vec!["/".into(), "javascript:alert(1)".into()];
        assert!(s.validate().unwrap_err().contains("Seitenpfad"));
        s.screenshot_paths = vec!["/a\"><script>".into()];
        assert!(s.validate().is_err());
        s.screenshot_paths = vec!["/".into(); 6];
        assert!(s.validate().is_err());
        s.screenshot_paths = vec!["/".into()];
        s.max_lifetime_secs = 7200;
        assert!(s.validate().is_err());
        s.max_lifetime_secs = 600;
        s.command = String::new();
        assert!(s.validate().unwrap_err().contains("Startkommando"));
        s.enabled = false;
        assert!(s.validate().is_ok(), "disabled settings may stay empty");
        s.ui_globs = vec!["/etc/*".into()];
        assert!(s.validate().is_err());
    }

    #[test]
    fn viewports() {
        assert_eq!(Viewport::parse("mobile"), Some(Viewport::Mobile));
        assert_eq!(Viewport::Mobile.size(), (390, 844));
        assert_eq!(PreviewStatus::parse("ready"), Some(PreviewStatus::Ready));
        assert_eq!(PreviewStatus::parse("x"), None);
    }
}
