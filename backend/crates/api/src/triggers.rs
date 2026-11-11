//! WP07: task templates, triggers (schedule, one-off, API, webhook, GitHub)
//! and their history – wire types, validation, the cron dialect and the
//! German texts. Shared by the app, the web UI and `queuectl`.
//!
//! The cron evaluation is generic over [`chrono::TimeZone`]; the app passes
//! `chrono_tz::Tz` (IANA zones), the tests here a fixed offset.
use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, TimeZone, Timelike, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{double_option, TaskLimitFields};

pub const MAX_NAME_CHARS: usize = 80;
pub const MAX_TITLE_CHARS: usize = 200;
pub const MAX_PROMPT_CHARS: usize = 20_000;
/// Free text of `POST /api/triggers/{id}/fire`.
pub const MAX_FIRE_TEXT_CHARS: usize = 20_000;
/// Default and bounds of the per-trigger rate limit (tasks per hour).
pub const DEFAULT_PER_HOUR: i32 = 10;
pub const MAX_PER_HOUR: i32 = 100;
/// Templates / triggers per project.
pub const MAX_TEMPLATES: i64 = 100;
pub const MAX_TRIGGERS: i64 = 50;
/// GitHub filter lists.
pub const MAX_FILTER_ITEMS: usize = 20;
/// A slot is "missed" when the scheduler sees it later than this.
pub const MISFIRE_GRACE_MINS: i64 = 10;
/// Signed timestamp of a generic webhook: at most this far off.
pub const WEBHOOK_TOLERANCE_SECS: i64 = 300;
/// Largest webhook body (bytes).
pub const WEBHOOK_MAX_BODY: usize = 1024 * 1024;
/// Header names of generic webhooks.
pub const HEADER_TIMESTAMP: &str = "x-queue-timestamp";
pub const HEADER_DELIVERY: &str = "x-queue-delivery";
pub const HEADER_SIGNATURE: &str = "x-queue-signature";

// ---------------------------------------------------------------------------
// Kinds, sources, outcomes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerKind {
    /// Cron with time zone (at most hourly).
    Schedule,
    /// Once at a point in time.
    Once,
    /// Only `POST /api/triggers/{id}/fire` (and the button in the UI).
    Api,
    /// Generic HMAC-signed webhook.
    Webhook,
    /// GitHub repository webhook with filters.
    Github,
}

impl TriggerKind {
    pub const ALL: [TriggerKind; 5] = [
        Self::Schedule,
        Self::Once,
        Self::Webhook,
        Self::Github,
        Self::Api,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Schedule => "schedule",
            Self::Once => "once",
            Self::Api => "api",
            Self::Webhook => "webhook",
            Self::Github => "github",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Schedule => "Zeitplan",
            Self::Once => "Einmalig",
            Self::Api => "Nur per API",
            Self::Webhook => "Webhook",
            Self::Github => "GitHub",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            Self::Schedule => "Startet regelmäßig einen Auftrag, z. B. jeden Morgen um 7 Uhr. Höchstens einmal pro Stunde.",
            Self::Once => "Startet einmal zu einem festen Zeitpunkt einen Auftrag.",
            Self::Api => "Startet nur, wenn ein Skript oder CI-System ihn mit einem API-Token aufruft.",
            Self::Webhook => "Ein anderes System ruft eine geheime Adresse auf und signiert die Anfrage. Der Inhalt geht als Information an den Agenten.",
            Self::Github => "Reagiert auf Ereignisse in einem GitHub-Repository: Push, neue Issues, Pull Requests oder fehlgeschlagene Checks.",
        }
    }

    /// Triggers that receive requests on `/api/hooks/{id}` (and have a secret).
    pub fn has_secret(self) -> bool {
        matches!(self, Self::Webhook | Self::Github)
    }

    pub fn is_timed(self) -> bool {
        matches!(self, Self::Schedule | Self::Once)
    }
}

/// What started a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunSource {
    Schedule,
    /// A missed slot that was made up after a downtime.
    CatchUp,
    /// "Jetzt auslösen" in the UI (cookie session).
    Manual,
    /// `POST /api/triggers/{id}/fire` with a token.
    Api,
    Webhook,
    Github,
}

impl RunSource {
    pub const ALL: [RunSource; 6] = [
        Self::Schedule,
        Self::CatchUp,
        Self::Manual,
        Self::Api,
        Self::Webhook,
        Self::Github,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Schedule => "schedule",
            Self::CatchUp => "catch_up",
            Self::Manual => "manual",
            Self::Api => "api",
            Self::Webhook => "webhook",
            Self::Github => "github",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Schedule => "Zeitplan",
            Self::CatchUp => "Nachgeholt",
            Self::Manual => "Von Hand",
            Self::Api => "API",
            Self::Webhook => "Webhook",
            Self::Github => "GitHub",
        }
    }
}

/// Result of one firing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    /// A task was created.
    Created,
    /// The event did not match the filters (or was a ping).
    Ignored,
    /// Deliberately not run: missed slot, a task of this trigger still open.
    Skipped,
    /// Refused: rate limit, owner lost the rights, trigger off.
    Rejected,
    /// The task could not be created (e.g. no access to the provider).
    Failed,
}

impl RunOutcome {
    pub const ALL: [RunOutcome; 5] = [
        Self::Created,
        Self::Ignored,
        Self::Skipped,
        Self::Rejected,
        Self::Failed,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Ignored => "ignored",
            Self::Skipped => "skipped",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Created => "Auftrag erstellt",
            Self::Ignored => "Ignoriert",
            Self::Skipped => "Übersprungen",
            Self::Rejected => "Abgelehnt",
            Self::Failed => "Fehlgeschlagen",
        }
    }
    /// Tone in the UI (`success`, `info`, `warning`, `danger`).
    pub fn tone(self) -> &'static str {
        match self {
            Self::Created => "success",
            Self::Ignored => "info",
            Self::Skipped => "info",
            Self::Rejected => "warning",
            Self::Failed => "danger",
        }
    }
}

// ---------------------------------------------------------------------------
// Cron (5 fields, minute fixed ⇒ at most hourly)
// ---------------------------------------------------------------------------

/// Parsed schedule. The minute is a single value, so a schedule fires at
/// most once per local hour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cron {
    pub minute: u32,
    hours: u32,
    days: u32,
    months: u16,
    weekdays: u8,
    day_star: bool,
    weekday_star: bool,
}

const MONTHS: [&str; 12] = [
    "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
];
const WEEKDAYS: [&str; 7] = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];
const WEEKDAY_DE: [&str; 7] = ["So", "Mo", "Di", "Mi", "Do", "Fr", "Sa"];
const MONTH_DE: [&str; 12] = [
    "Jan", "Feb", "Mär", "Apr", "Mai", "Jun", "Jul", "Aug", "Sep", "Okt", "Nov", "Dez",
];

fn value(s: &str, names: &[&str], offset: u32, what: &str) -> Result<u32, String> {
    if let Ok(n) = s.parse::<u32>() {
        return Ok(n);
    }
    names
        .iter()
        .position(|n| n.eq_ignore_ascii_case(s))
        .map(|i| i as u32 + offset)
        .ok_or_else(|| format!("{what}: „{s}“ ist keine gültige Angabe"))
}

/// One field as a bit set over `lo..=hi`.
fn field(
    s: &str,
    lo: u32,
    hi: u32,
    names: &[&str],
    offset: u32,
    what: &str,
) -> Result<u64, String> {
    let mut set = 0u64;
    for part in s.split(',') {
        if part.is_empty() {
            return Err(format!("{what}: leerer Listeneintrag"));
        }
        let (range, step) = match part.split_once('/') {
            Some((r, st)) => {
                let st: u32 = st.parse().ok().filter(|v| *v >= 1).ok_or_else(|| {
                    format!("{what}: Schrittweite „{st}“ muss eine Zahl ≥ 1 sein")
                })?;
                (r, st)
            }
            None => (part, 1),
        };
        let (a, b) = if range == "*" {
            (lo, hi)
        } else if let Some((a, b)) = range.split_once('-') {
            (
                value(a, names, offset, what)?,
                value(b, names, offset, what)?,
            )
        } else {
            let a = value(range, names, offset, what)?;
            // `a/n` means `a-hi/n`.
            (a, if part.contains('/') { hi } else { a })
        };
        if a < lo || b > hi || a > b {
            return Err(format!("{what}: {a}–{b} liegt nicht in {lo}–{hi}"));
        }
        let mut v = a;
        while v <= b {
            set |= 1 << v;
            v += step;
        }
    }
    Ok(set)
}

fn days_in_month(month: u32) -> u32 {
    match month {
        2 => 29,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

impl Cron {
    /// Parses `minute hour day month weekday` (or `@hourly`, `@daily`,
    /// `@weekly`, `@monthly`, `@yearly`). German error messages.
    pub fn parse(s: &str) -> Result<Cron, String> {
        let s = s.trim();
        let expanded = match s.to_ascii_lowercase().as_str() {
            "@hourly" => "0 * * * *".to_string(),
            "@daily" | "@midnight" => "0 0 * * *".into(),
            "@weekly" => "0 0 * * 0".into(),
            "@monthly" => "0 0 1 * *".into(),
            "@yearly" | "@annually" => "0 0 1 1 *".into(),
            _ if s.starts_with('@') => return Err(format!("Unbekannte Abkürzung „{s}“")),
            _ => s.to_string(),
        };
        let f: Vec<&str> = expanded.split_whitespace().collect();
        if f.len() != 5 {
            return Err(
                "Fünf Felder nötig: Minute Stunde Tag Monat Wochentag (z. B. „15 7 * * 1-5“)"
                    .into(),
            );
        }
        let minute: u32 = f[0].parse().ok().filter(|m| *m < 60).ok_or_else(|| {
            "Minute muss eine feste Zahl von 0 bis 59 sein – Queue startet höchstens einmal pro Stunde".to_string()
        })?;
        let hours = field(f[1], 0, 23, &[], 0, "Stunde")? as u32;
        let days = field(f[2], 1, 31, &[], 0, "Tag")? as u32;
        let months = field(f[3], 1, 12, &MONTHS, 1, "Monat")? as u16;
        let mut wd = field(f[4], 0, 7, &WEEKDAYS, 0, "Wochentag")?;
        if wd & (1 << 7) != 0 {
            wd = (wd | 1) & 0x7f; // 7 = Sunday
        }
        let c = Cron {
            minute,
            hours,
            days,
            months,
            weekdays: wd as u8,
            day_star: f[2].starts_with('*'),
            weekday_star: f[4].starts_with('*'),
        };
        // Day-of-month only (e.g. 31 February): must exist in a chosen month.
        if !c.day_star && c.weekday_star {
            let possible = (1..=12u32)
                .filter(|m| c.months & (1 << m) != 0)
                .any(|m| (1..=days_in_month(m)).any(|d| c.days & (1 << d) != 0));
            if !possible {
                return Err("Dieser Zeitplan trifft nie zu (den Tag gibt es in den gewählten Monaten nicht)".into());
            }
        }
        Ok(c)
    }

    fn day_matches(&self, d: NaiveDate) -> bool {
        let dom = self.days & (1 << d.day()) != 0;
        let dow = self.weekdays & (1 << d.weekday().num_days_from_sunday()) != 0;
        match (self.day_star, self.weekday_star) {
            (true, true) => true,
            (true, false) => dow,
            (false, true) => dom,
            // Classic cron: day-of-month OR weekday when both are restricted.
            (false, false) => dom || dow,
        }
    }

    fn date_matches(&self, d: NaiveDate) -> bool {
        self.months & (1 << d.month()) != 0 && self.day_matches(d)
    }

    /// First slot strictly after `after`, evaluated in local time of `tz`.
    /// A local time that does not exist (clocks go forward) runs one hour
    /// later; a local time that exists twice (clocks go back) runs once, at
    /// its first occurrence. `None` if nothing matches within 8 years.
    pub fn next_after<Tz: TimeZone>(&self, tz: &Tz, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let local = after.with_timezone(tz).naive_local();
        let mut date = local.date();
        let mut hour = local.hour();
        let end = date + Duration::days(366 * 8);
        while date <= end {
            if !self.date_matches(date) {
                date = date.succ_opt()?;
                hour = 0;
                continue;
            }
            for h in hour..24 {
                if self.hours & (1 << h) == 0 {
                    continue;
                }
                let naive =
                    NaiveDateTime::new(date, chrono::NaiveTime::from_hms_opt(h, self.minute, 0)?);
                let at = match tz.from_local_datetime(&naive) {
                    chrono::LocalResult::Single(t) => t,
                    chrono::LocalResult::Ambiguous(first, _) => first,
                    chrono::LocalResult::None => {
                        match tz
                            .from_local_datetime(&(naive + Duration::hours(1)))
                            .earliest()
                        {
                            Some(t) => t,
                            None => continue,
                        }
                    }
                }
                .with_timezone(&Utc);
                if at > after {
                    return Some(at);
                }
            }
            date = date.succ_opt()?;
            hour = 0;
        }
        None
    }

    /// The next `n` slots after `after`, at least one hour apart.
    pub fn upcoming<Tz: TimeZone>(
        &self,
        tz: &Tz,
        after: DateTime<Utc>,
        n: usize,
    ) -> Vec<DateTime<Utc>> {
        let mut out: Vec<DateTime<Utc>> = vec![];
        let mut cur = after;
        while out.len() < n {
            let floor = out
                .last()
                .map(|l| *l + Duration::minutes(59) + Duration::seconds(59));
            let from = match floor {
                Some(f) if f > cur => f,
                _ => cur,
            };
            match self.next_after(tz, from) {
                Some(t) => {
                    out.push(t);
                    cur = t;
                }
                None => break,
            }
        }
        out
    }

    /// German description, e.g. „Mo–Fr um 07:15“.
    pub fn describe(&self) -> String {
        let hours: Vec<u32> = (0..24).filter(|h| self.hours & (1 << h) != 0).collect();
        let time = if hours.len() == 24 {
            format!("stündlich um :{:02}", self.minute)
        } else if hours.len() <= 6 {
            let ts: Vec<String> = hours
                .iter()
                .map(|h| format!("{h:02}:{:02}", self.minute))
                .collect();
            format!("um {}", join_de(&ts))
        } else {
            format!("{}-mal am Tag, jeweils um :{:02}", hours.len(), self.minute)
        };
        let wds: Vec<u32> = (0..7).filter(|d| self.weekdays & (1 << d) != 0).collect();
        let wd_text = || -> String {
            // Monday-first order; contiguous Mon..x as a range.
            let order: Vec<u32> = [1, 2, 3, 4, 5, 6, 0]
                .into_iter()
                .filter(|d| wds.contains(d))
                .collect();
            let pos = |d: u32| {
                [1, 2, 3, 4, 5, 6, 0]
                    .iter()
                    .position(|x| *x == d)
                    .unwrap_or(0)
            };
            let contiguous =
                order.len() > 2 && order.windows(2).all(|w| pos(w[1]) == pos(w[0]) + 1);
            if contiguous {
                format!(
                    "{}–{}",
                    WEEKDAY_DE[order[0] as usize],
                    WEEKDAY_DE[*order.last().unwrap_or(&0) as usize]
                )
            } else {
                let names: Vec<String> = order
                    .iter()
                    .map(|d| WEEKDAY_DE[*d as usize].to_string())
                    .collect();
                join_de(&names)
            }
        };
        let days: Vec<String> = (1..=31)
            .filter(|d| self.days & (1 << d) != 0)
            .map(|d| format!("{d}."))
            .collect();
        let day = match (self.day_star, self.weekday_star) {
            (true, true) => "täglich".to_string(),
            (true, false) => wd_text(),
            (false, true) => format!("am {} des Monats", join_de(&days)),
            (false, false) => format!("am {} des Monats oder {}", join_de(&days), wd_text()),
        };
        let months: Vec<String> = (1..=12)
            .filter(|m| self.months & (1 << m) != 0)
            .map(|m| MONTH_DE[(m - 1) as usize].to_string())
            .collect();
        let month = if months.len() == 12 {
            String::new()
        } else {
            format!(" im {}", join_de(&months))
        };
        if hours.len() == 24 && day == "täglich" && month.is_empty() {
            return time;
        }
        format!("{day}{month} {time}")
    }
}

fn join_de(items: &[String]) -> String {
    match items.len() {
        0 => String::new(),
        1 => items[0].clone(),
        n => format!("{} und {}", items[..n - 1].join(", "), items[n - 1]),
    }
}

/// IANA zone name syntax (the app checks existence with chrono-tz).
pub fn plausible_timezone(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '+'))
}

// ---------------------------------------------------------------------------
// GitHub filters
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GithubEvent {
    Push,
    Issues,
    PullRequest,
    /// `check_run` completed with `failure`, `timed_out` or `startup_failure`.
    CheckFailure,
}

impl GithubEvent {
    pub const ALL: [GithubEvent; 4] = [
        Self::Push,
        Self::Issues,
        Self::PullRequest,
        Self::CheckFailure,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Push => "push",
            Self::Issues => "issues",
            Self::PullRequest => "pull_request",
            Self::CheckFailure => "check_failure",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Push => "Push",
            Self::Issues => "Issues",
            Self::PullRequest => "Pull Requests",
            Self::CheckFailure => "Fehlgeschlagene Checks",
        }
    }
    pub fn help(self) -> &'static str {
        match self {
            Self::Push => "Neue Commits auf einem passenden Branch (nicht: gelöschte Branches, Queue-eigene Commits, Branches „queue/…“).",
            Self::Issues => "Issue geöffnet, wieder geöffnet oder mit einem passenden Label versehen.",
            Self::PullRequest => "Pull Request geöffnet, wieder geöffnet, neue Commits, bereit zur Prüfung oder mit passendem Label versehen. Der Branch-Filter gilt für den Ziel-Branch.",
            Self::CheckFailure => "Ein Check (z. B. GitHub Actions) ist fehlgeschlagen. Höchstens ein Auftrag pro Commit; Queues eigene Pull Requests behandelt schon der Auto-Fix.",
        }
    }
    /// The `X-GitHub-Event` header that carries this kind.
    pub fn header(self) -> &'static str {
        match self {
            Self::Push => "push",
            Self::Issues => "issues",
            Self::PullRequest => "pull_request",
            Self::CheckFailure => "check_run",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct GithubFilter {
    pub events: Vec<GithubEvent>,
    /// Branch patterns (`*` = any characters except `/`, `**` = any), empty = all.
    #[serde(default)]
    pub branches: Vec<String>,
    /// Issues/pull requests need one of these labels; empty = no condition.
    #[serde(default)]
    pub labels: Vec<String>,
    /// `owner/repo`; `None` = the repository of the project (WP05 setting).
    #[serde(default)]
    pub repository: Option<String>,
    /// Events caused by bots count too (default: ignored).
    #[serde(default)]
    pub include_bots: bool,
    /// Draft pull requests count too (default: ignored).
    #[serde(default)]
    pub include_drafts: bool,
}

/// Branch pattern match: `*` = any characters except `/`, `**` = any
/// characters, `?` = one character except `/`; the whole name must match.
pub fn branch_matches(pattern: &str, branch: &str) -> bool {
    fn go(p: &[u8], s: &[u8]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some(b'*') if p.get(1) == Some(&b'*') => (0..=s.len()).any(|i| go(&p[2..], &s[i..])),
            Some(b'*') => {
                let max = s.iter().position(|c| *c == b'/').unwrap_or(s.len());
                (0..=max).any(|i| go(&p[1..], &s[i..]))
            }
            Some(b'?') => !s.is_empty() && s[0] != b'/' && go(&p[1..], &s[1..]),
            Some(c) => s.first() == Some(c) && go(&p[1..], &s[1..]),
        }
    }
    go(pattern.as_bytes(), branch.as_bytes())
}

fn clean_list(what: &str, items: &[String], max_len: usize) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = vec![];
    for i in items {
        let t = i.trim();
        if t.is_empty() {
            continue;
        }
        if t.chars().count() > max_len || t.chars().any(|c| c.is_control()) {
            return Err(format!(
                "{what}: „{t}“ ist ungültig (höchstens {max_len} Zeichen)"
            ));
        }
        if !out.iter().any(|o| o == t) {
            out.push(t.to_string());
        }
    }
    if out.len() > MAX_FILTER_ITEMS {
        return Err(format!("{what}: höchstens {MAX_FILTER_ITEMS} Einträge"));
    }
    Ok(out)
}

/// `owner/repo` with GitHub's characters.
pub fn valid_repository(s: &str) -> bool {
    let ok = |p: &str| {
        !p.is_empty()
            && p.len() <= 100
            && p != "."
            && p != ".."
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    matches!(s.split_once('/'), Some((o, r)) if ok(o) && ok(r))
}

/// Normalised filter or a German error.
pub fn validate_github(f: &GithubFilter) -> Result<GithubFilter, String> {
    let mut events = f.events.clone();
    events.sort();
    events.dedup();
    if events.is_empty() {
        return Err("Mindestens ein GitHub-Ereignis wählen".into());
    }
    let branches = clean_list("Branch-Muster", &f.branches, 200)?;
    for b in &branches {
        if b.contains(char::is_whitespace) || b.starts_with("refs/") || b.contains("..") {
            return Err(format!(
                "Branch-Muster „{b}“: nur den Branch-Namen angeben (ohne refs/heads/, ohne Leerzeichen)"
            ));
        }
    }
    let labels = clean_list("Labels", &f.labels, 50)?;
    let repository = match f
        .repository
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
    {
        Some(r) if valid_repository(r) => Some(r.to_string()),
        Some(r) => return Err(format!("Repository „{r}“: Form besitzer/repository")),
        None => None,
    };
    Ok(GithubFilter {
        events,
        branches,
        labels,
        repository,
        include_bots: f.include_bots,
        include_drafts: f.include_drafts,
    })
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// A task template of a project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateView {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub title: String,
    pub prompt: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    /// `None` = the project's default.
    pub plan_mode: Option<bool>,
    pub max_runtime_secs: Option<i32>,
    pub max_cost_usd: Option<f64>,
    pub max_tokens: Option<i64>,
    pub max_turns: Option<i32>,
    pub created_by_name: Option<String>,
    /// Triggers that use it (a used template cannot be deleted).
    pub used_by: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `POST /api/projects/{id}/templates`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewTemplate {
    pub name: String,
    pub title: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_mode: Option<bool>,
    #[serde(flatten)]
    pub limits: TaskLimitFields,
}

/// `PATCH /api/templates/{id}`: absent = unchanged, `null` = default.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UpdateTemplate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub provider: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub model: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub plan_mode: Option<Option<bool>>,
    #[serde(flatten)]
    pub limits: TaskLimitFields,
}

/// Bounded, non-empty single-line text.
pub fn validate_line(what: &str, s: &str, max: usize) -> Result<String, String> {
    let t = s.trim();
    if t.is_empty() {
        return Err(format!("{what} darf nicht leer sein"));
    }
    if t.chars().count() > max {
        return Err(format!("{what} ist zu lang (höchstens {max} Zeichen)"));
    }
    if t.chars().any(char::is_control) {
        return Err(format!("{what} enthält ungültige Zeichen"));
    }
    Ok(t.to_string())
}

/// Bounded, non-empty multi-line text.
pub fn validate_text(what: &str, s: &str, max: usize) -> Result<String, String> {
    let t = s.trim();
    if t.is_empty() {
        return Err(format!("{what} darf nicht leer sein"));
    }
    if t.chars().count() > max {
        return Err(format!("{what} ist zu lang (höchstens {max} Zeichen)"));
    }
    if t.contains('\0') {
        return Err(format!("{what} enthält ungültige Zeichen"));
    }
    Ok(t.to_string())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriggerView {
    pub id: Uuid,
    pub project_id: Uuid,
    pub owner_id: Uuid,
    pub owner_name: String,
    pub template_id: Uuid,
    pub template_name: String,
    pub kind: TriggerKind,
    pub name: String,
    pub enabled: bool,
    pub cron: Option<String>,
    /// German description of `cron`.
    pub cron_text: Option<String>,
    pub timezone: Option<String>,
    pub run_at: Option<DateTime<Utc>>,
    pub next_run_at: Option<DateTime<Utc>>,
    /// Further slots after `next_run_at` (schedules only).
    pub upcoming: Vec<DateTime<Utc>>,
    pub catch_up: bool,
    pub github: Option<GithubFilter>,
    pub max_per_hour: i32,
    pub skip_if_active: bool,
    /// Address for webhook senders (webhook and GitHub triggers).
    pub hook_url: Option<String>,
    pub disabled_reason: Option<String>,
    pub last_fired_at: Option<DateTime<Utc>>,
    pub last_run: Option<TriggerRunView>,
    /// The caller owns it (edit, fire, new secret).
    pub can_manage: bool,
    /// The caller may switch it off or delete it (owner or project owner).
    pub can_disable: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `POST /api/projects/{id}/triggers`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewTrigger {
    pub template_id: Uuid,
    pub kind: TriggerKind,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_at: Option<DateTime<Utc>>,
    #[serde(default = "yes")]
    pub catch_up: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<GithubFilter>,
    #[serde(default = "default_per_hour")]
    pub max_per_hour: i32,
    #[serde(default = "yes")]
    pub skip_if_active: bool,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}
fn default_per_hour() -> i32 {
    DEFAULT_PER_HOUR
}

/// `PATCH /api/triggers/{id}`: absent = unchanged (the kind is fixed).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UpdateTrigger {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catch_up: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<GithubFilter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_per_hour: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_if_active: Option<bool>,
}

/// Answer of creating a trigger or renewing its secret: the secret is shown
/// exactly once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreatedTrigger {
    pub trigger: TriggerView,
    pub secret: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriggerRunView {
    pub id: Uuid,
    pub trigger_id: Uuid,
    pub source: RunSource,
    pub outcome: RunOutcome,
    pub reason: Option<String>,
    pub task_id: Option<Uuid>,
    pub task_title: Option<String>,
    pub task_status: Option<String>,
    /// GitHub event and action, e.g. `push` or `issues.opened`.
    pub event: Option<String>,
    pub scheduled_for: Option<DateTime<Utc>>,
    pub delivery_id: Option<String>,
    pub actor_name: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// `POST /api/triggers/{id}/fire`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FireRequest {
    /// Extra information for the agent (handed over as data, not as instruction).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FireResult {
    pub run_id: Option<Uuid>,
    pub outcome: RunOutcome,
    pub reason: Option<String>,
    pub task_id: Option<Uuid>,
}

/// `POST /api/triggers/preview`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CronPreviewRequest {
    pub cron: String,
    pub timezone: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CronPreview {
    pub description: Option<String>,
    pub next_runs: Vec<DateTime<Utc>>,
    pub error: Option<String>,
}

pub const TEMPLATES_HELP: &str = "Eine Vorlage ist ein gespeicherter Auftrag (Titel, Beschreibung, Optionen). Auslöser starten Aufträge aus Vorlagen; im Auftragsformular kannst du sie auch von Hand verwenden.";
pub const TRIGGERS_HELP: &str = "Ein Auslöser startet automatisch einen Auftrag aus einer Vorlage – nach Zeitplan, einmalig, per Webhook, bei GitHub-Ereignissen oder per API. Er läuft immer mit deinem Zugang zum Anbieter, auch wenn andere im Projekt ihn sehen.";
pub const CATCH_UP_HELP: &str = "War Queue zum geplanten Zeitpunkt nicht erreichbar (z. B. Neustart), wird der Lauf genau einmal nachgeholt – auch wenn mehrere Termine verpasst wurden. Ausgeschaltet wird er übersprungen und im Verlauf vermerkt.";
pub const SKIP_ACTIVE_HELP: &str = "Solange ein Auftrag dieses Auslösers noch wartet oder läuft, startet kein weiterer. Schützt vor Endlosschleifen und doppelter Arbeit.";
pub const RATE_HELP: &str = "Höchstens so viele Aufträge pro Stunde. Weitere Anfragen werden abgelehnt und im Verlauf vermerkt.";
pub const PAYLOAD_HELP: &str = "Was ein Webhook oder GitHub mitschickt, bekommt der Agent nur als markierte Information – nie als Anweisung. Im Titel des Auftrags steht es nie.";
pub const SECRET_ONCE: &str = "Kopiere das Secret jetzt und trage es beim Absender ein. Queue zeigt es nicht noch einmal; bei Verlust erzeugst du ein neues.";

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn cron_parses_fields_names_steps_and_macros() {
        let c = Cron::parse("15 7 * * MON-FRI").unwrap();
        assert_eq!(c.minute, 15);
        assert_eq!(c.describe(), "Mo–Fr um 07:15");
        assert_eq!(Cron::parse("15 7 * * 1-5").unwrap(), c);
        assert_eq!(
            Cron::parse("@hourly").unwrap().describe(),
            "stündlich um :00"
        );
        assert_eq!(
            Cron::parse("0 * * * 1-5").unwrap().describe(),
            "Mo–Fr stündlich um :00"
        );
        assert_eq!(
            Cron::parse("@daily").unwrap().describe(),
            "täglich um 00:00"
        );
        assert_eq!(
            Cron::parse("0 */6 * * *").unwrap().describe(),
            "täglich um 00:00, 06:00, 12:00 und 18:00"
        );
        assert_eq!(
            Cron::parse("30 9 1,15 * *").unwrap().describe(),
            "am 1. und 15. des Monats um 09:30"
        );
        assert_eq!(
            Cron::parse("0 8 * jan,jul 0").unwrap().describe(),
            "So im Jan und Jul um 08:00"
        );
        assert_eq!(
            Cron::parse("0 8 * * 7").unwrap(),
            Cron::parse("0 8 * * 0").unwrap()
        );
        assert_eq!(
            Cron::parse("0 12 * * 1,3,5").unwrap().describe(),
            "Mo, Mi und Fr um 12:00"
        );
        assert_eq!(
            Cron::parse("5 9-17/2 * * *").unwrap().describe(),
            "täglich um 09:05, 11:05, 13:05, 15:05 und 17:05"
        );
        assert_eq!(
            Cron::parse("0 1/12 * * *").unwrap().describe(),
            "täglich um 01:00 und 13:00"
        );
    }

    #[test]
    fn cron_rejects_more_than_hourly_and_nonsense() {
        for bad in [
            "* * * * *",
            "*/5 * * * *",
            "0,30 * * * *",
            "0-5 * * * *",
            "60 * * * *",
            "0 24 * * *",
            "0 * 0 * *",
            "0 * 32 * *",
            "0 * * 13 *",
            "0 * * * 8",
            "0 * * *",
            "0 * * * * *",
            "0 5-3 * * *",
            "0 */0 * * *",
            "0 1,,2 * * *",
            "0 0 31 2 *",
            "0 0 30 2 *",
            "@reboot",
            "0 0 * FOO *",
        ] {
            assert!(Cron::parse(bad).is_err(), "{bad} must be rejected");
        }
        // 31 February never exists, but with a weekday it can still fire.
        assert!(Cron::parse("0 0 31 2 1").is_ok());
        assert!(Cron::parse("0 0 29 2 *").is_ok());
    }

    #[test]
    fn next_after_in_a_fixed_offset_and_the_day_or_weekday_rule() {
        let cet = FixedOffset::east_opt(3600).unwrap();
        let c = Cron::parse("15 7 * * 1-5").unwrap();
        // Friday 2026-11-13 08:00 local → Monday 07:15 local = 06:15 UTC.
        let n = c.next_after(&cet, utc("2026-11-13T07:00:00Z")).unwrap();
        assert_eq!(n, utc("2026-11-16T06:15:00Z"));
        // Exactly at a slot: strictly after.
        assert_eq!(c.next_after(&cet, n).unwrap(), utc("2026-11-17T06:15:00Z"));
        // Day 1 OR Monday.
        let c = Cron::parse("0 0 1 * 1").unwrap();
        let mut t = utc("2026-11-30T23:30:00Z"); // = Dec 1 00:30 CET
        let mut got = vec![];
        for _ in 0..3 {
            t = c.next_after(&cet, t).unwrap();
            got.push(t.with_timezone(&cet).format("%a %d").to_string());
        }
        assert_eq!(got, ["Mon 07", "Mon 14", "Mon 21"]);
        // 29 February: next leap year.
        let c = Cron::parse("0 12 29 2 *").unwrap();
        assert_eq!(
            c.next_after(&Utc, utc("2026-03-01T00:00:00Z")).unwrap(),
            utc("2028-02-29T12:00:00Z")
        );
        // Half-hour offset (India): minute in local time.
        let ist = FixedOffset::east_opt(5 * 3600 + 1800).unwrap();
        let c = Cron::parse("0 9 * * *").unwrap();
        assert_eq!(
            c.next_after(&ist, utc("2026-11-11T00:00:00Z")).unwrap(),
            utc("2026-11-11T03:30:00Z")
        );
    }

    #[test]
    fn upcoming_slots_are_at_least_an_hour_apart() {
        let c = Cron::parse("0 * * * *").unwrap();
        let u = c.upcoming(&Utc, utc("2026-11-11T10:00:00Z"), 3);
        assert_eq!(
            u,
            vec![
                utc("2026-11-11T11:00:00Z"),
                utc("2026-11-11T12:00:00Z"),
                utc("2026-11-11T13:00:00Z")
            ]
        );
    }

    #[test]
    fn branch_patterns() {
        assert!(branch_matches("main", "main"));
        assert!(!branch_matches("main", "feature/main"));
        assert!(!branch_matches("main", "main2"));
        assert!(branch_matches("release/*", "release/1.2"));
        assert!(!branch_matches("release/*", "release/1.2/hotfix"));
        assert!(branch_matches("release/**", "release/1.2/hotfix"));
        assert!(branch_matches("**", "a/b/c"));
        assert!(branch_matches("feat-?", "feat-x"));
        assert!(!branch_matches("feat-?", "feat-/"));
        assert!(branch_matches("*", "main"));
        assert!(!branch_matches("*", "queue/abc"));
    }

    #[test]
    fn github_filter_validation() {
        let f = GithubFilter {
            events: vec![GithubEvent::Push, GithubEvent::Push, GithubEvent::Issues],
            branches: vec![" main ".into(), "".into(), "main".into()],
            labels: vec!["queue".into()],
            repository: Some(" acme/app ".into()),
            include_bots: false,
            include_drafts: false,
        };
        let v = validate_github(&f).unwrap();
        assert_eq!(v.events, vec![GithubEvent::Push, GithubEvent::Issues]);
        assert_eq!(v.branches, vec!["main"]);
        assert_eq!(v.repository.as_deref(), Some("acme/app"));
        for bad in [
            GithubFilter {
                events: vec![],
                ..f.clone()
            },
            GithubFilter {
                branches: vec!["refs/heads/main".into()],
                ..f.clone()
            },
            GithubFilter {
                branches: vec!["a b".into()],
                ..f.clone()
            },
            GithubFilter {
                repository: Some("acme".into()),
                ..f.clone()
            },
            GithubFilter {
                repository: Some("acme/../x".into()),
                ..f.clone()
            },
            GithubFilter {
                labels: (0..21).map(|i| i.to_string()).collect(),
                ..f.clone()
            },
        ] {
            assert!(validate_github(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn enums_roundtrip() {
        for k in TriggerKind::ALL {
            assert_eq!(TriggerKind::parse(k.as_str()), Some(k));
            assert_eq!(serde_json::to_value(k).unwrap(), k.as_str());
        }
        for s in RunSource::ALL {
            assert_eq!(RunSource::parse(s.as_str()), Some(s));
            assert_eq!(serde_json::to_value(s).unwrap(), s.as_str());
        }
        for o in RunOutcome::ALL {
            assert_eq!(RunOutcome::parse(o.as_str()), Some(o));
            assert_eq!(serde_json::to_value(o).unwrap(), o.as_str());
        }
        for e in GithubEvent::ALL {
            assert_eq!(serde_json::to_value(e).unwrap(), e.as_str());
        }
        let t: NewTrigger = serde_json::from_value(serde_json::json!({
            "template_id": Uuid::nil(), "kind": "api", "name": "x"
        }))
        .unwrap();
        assert!(t.catch_up && t.skip_if_active && t.enabled);
        assert_eq!(t.max_per_hour, DEFAULT_PER_HOUR);
    }
}
