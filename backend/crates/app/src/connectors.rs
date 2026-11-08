//! Connector registry (WP13): which accesses exist, how they are configured,
//! where they process data, and which harness/wire format they use.
//!
//! Secrets are AES-256-GCM encrypted (`crypto.rs`) and write-only. Facts that
//! could not be verified in this environment are listed per type in
//! `unverified` and shown in the UI.
use queued_api::{
    ConnectorField, ConnectorTypeInfo, ConnectorView, DataLocation, FieldOption, Residency,
};
use serde_json::Value;
use uuid::Uuid;

use crate::{crypto, state::AppState};

/// Wire format of direct single-shot calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    /// `POST {base}/v1/messages` (Anthropic Messages API).
    Anthropic,
    /// `POST {base}/chat/completions` (OpenAI-compatible).
    OpenAiChat,
}

/// Provider batch API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchApi {
    Anthropic,
    OpenAi,
    Mistral,
}

impl BatchApi {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
            Self::Mistral => "mistral",
        }
    }
}

pub const ANTHROPIC_BASE: &str = "https://api.anthropic.com";
pub const OPENAI_BASE: &str = "https://api.openai.com/v1";
pub const MISTRAL_BASE: &str = "https://api.mistral.ai/v1";
pub const MISTRAL_EU_BASE: &str = "https://api.eu.mistral.ai/v1";
pub const OPENROUTER_BASE: &str = "https://openrouter.ai/api/v1";
pub const OPENROUTER_EU_BASE: &str = "https://eu.openrouter.ai/api/v1";

fn opt(value: &str, label: &str) -> FieldOption {
    FieldOption {
        value: value.into(),
        label: label.into(),
    }
}

fn field(key: &str, label: &str, help: &str, input: &str, required: bool) -> ConnectorField {
    ConnectorField {
        key: key.into(),
        label: label.into(),
        help: help.into(),
        input: input.into(),
        options: vec![],
        required,
        default: None,
    }
}

fn select(key: &str, label: &str, help: &str, options: Vec<FieldOption>) -> ConnectorField {
    ConnectorField {
        default: options.first().map(|o| o.value.clone()),
        options,
        ..field(key, label, help, "select", true)
    }
}

fn location_field() -> ConnectorField {
    select(
        "location",
        "Wo steht der Server?",
        "Selbst angegeben – entscheidet, ob der Zugang in Projekten mit EU/CH-Regel erlaubt ist.",
        vec![
            opt("unknown", "Weiß ich nicht"),
            opt("ch", "Schweiz"),
            opt("eu", "EU"),
            opt("us", "USA"),
        ],
    )
}

fn base_url_override() -> ConnectorField {
    field(
        "base_url",
        "Eigene Basis-URL (optional)",
        "Nur nötig für einen Proxy oder ein Gateway vor dem Anbieter.",
        "url",
        false,
    )
}

const SUBSCRIPTION_NOTE: &str = "Ob ein Abo in einer selbst gehosteten Queue genutzt werden darf, ist rechtlich ungeklärt (Nutzungsbedingungen des Anbieters prüfen). Abos haben Wochen-Limits und kein Batch.";

/// All connector types. `mock` only with `ENABLE_MOCK_PROVIDER`.
pub fn types(with_mock: bool) -> Vec<ConnectorTypeInfo> {
    let mut v = vec![
        ConnectorTypeInfo {
            kind: "claude_subscription".into(),
            label: "Claude-Abo (Claude Code)".into(),
            description: "Dein Claude-Pro/Max/Ultra-Abo über die offizielle CLI.".into(),
            group: "claude".into(),
            harness: Some("claude".into()),
            secret_label: Some("OAuth-Token".into()),
            secret_help: "Auf deinem Rechner `claude setup-token` ausführen und das Token einfügen.".into(),
            secret_json: false,
            secret_optional: false,
            fields: vec![],
            location: DataLocation::Us,
            single_shot: false,
            batch: false,
            flex: false,
            models: vec!["claude-sonnet-5-5".into(), "claude-opus-5-5".into(), "claude-haiku-5-5".into()],
            unverified: vec![],
            note: SUBSCRIPTION_NOTE.into(),
        },
        ConnectorTypeInfo {
            kind: "anthropic_api".into(),
            label: "Anthropic API".into(),
            description: "Claude per API-Schlüssel – mit Batch (−50 %) für Planung und Review.".into(),
            group: "claude".into(),
            harness: Some("claude".into()),
            secret_label: Some("API-Schlüssel".into()),
            secret_help: "Beginnt mit `sk-ant-`. Erstellen in der Anthropic Console.".into(),
            secret_json: false,
            secret_optional: false,
            fields: vec![base_url_override()],
            location: DataLocation::Us,
            single_shot: true,
            batch: true,
            flex: false,
            models: vec!["claude-sonnet-5-5".into(), "claude-opus-5-5".into(), "claude-haiku-5-5".into()],
            unverified: vec!["Message-Batches-Format (POST /v1/messages/batches) aus Vorwissen, nur gegen Fake-Server getestet".into()],
            note: "Die First-Party-API bietet keine EU-Datenhaltung (nur global/USA).".into(),
        },
        ConnectorTypeInfo {
            kind: "bedrock".into(),
            label: "Claude über AWS Bedrock".into(),
            description: "Claude Code im Bedrock-Modus, z. B. mit dem EU-Geo-Profil (Frankfurt, Stockholm, Mailand, Spanien, Irland, Paris).".into(),
            group: "eu_ch".into(),
            harness: Some("claude".into()),
            secret_label: Some("Bedrock-API-Schlüssel oder Zugangsschlüssel (JSON)".into()),
            secret_help: "Entweder ein Bedrock-API-Schlüssel (Bearer-Token) oder JSON {\"access_key_id\":…,\"secret_access_key\":…,\"session_token\":… (optional)}.".into(),
            secret_json: false,
            secret_optional: false,
            fields: vec![
                select("geo", "Datenraum", "Bestimmt das Inferenz-Profil (eu.anthropic.… / us.anthropic.…).", vec![opt("eu", "EU"), opt("us", "USA"), opt("global", "Weltweit")]),
                ConnectorField { default: Some("eu-central-1".into()), ..field("region", "AWS-Region", "Region, über die Bedrock aufgerufen wird, z. B. eu-central-1 (Frankfurt). Zürich (eu-central-2) ist für In-Region-Betrieb laut AWS nicht unterstützt.", "text", true) },
            ],
            location: DataLocation::Eu,
            single_shot: false,
            batch: false,
            flex: false,
            models: vec!["claude-sonnet-5-5".into(), "claude-opus-5-5".into()],
            unverified: vec![
                "Env-Variablen CLAUDE_CODE_USE_BEDROCK/AWS_REGION/AWS_BEARER_TOKEN_BEDROCK sind im Claude-Code-Binary 2.1.319 vorhanden; Verhalten nicht live geprüft".into(),
                "Dass Bedrock dieselben Basispreise wie die Anthropic-API hat, ist nicht verifiziert (+10 % EU-Aufschlag ist belegt)".into(),
                "Planung/Review per Batch über Bedrock wird nicht angeboten (eigene AWS-Signatur wäre ungeprüft)".into(),
            ],
            note: String::new(),
        },
        ConnectorTypeInfo {
            kind: "vertex".into(),
            label: "Claude über Google Vertex AI".into(),
            description: "Claude Code im Vertex-Modus, z. B. über den EU-Multi-Region-Endpunkt.".into(),
            group: "eu_ch".into(),
            harness: Some("claude".into()),
            secret_label: Some("Dienstkonto-Schlüssel (JSON)".into()),
            secret_help: "JSON-Schlüssel eines Google-Cloud-Dienstkontos mit Zugriff auf Vertex AI.".into(),
            secret_json: true,
            secret_optional: false,
            fields: vec![
                field("project_id", "Google-Cloud-Projekt-ID", "", "text", true),
                select("region", "Region", "„eu“ = EU-Multi-Region (aiplatform.eu.rep.googleapis.com).", vec![
                    opt("eu", "EU (Multi-Region)"),
                    opt("europe-west1", "Belgien (europe-west1)"),
                    opt("europe-west4", "Niederlande (europe-west4)"),
                    opt("europe-west6", "Zürich (europe-west6) – Verfügbarkeit nicht bestätigt"),
                    opt("us-east5", "USA (us-east5)"),
                    opt("global", "Weltweit"),
                ]),
            ],
            location: DataLocation::Eu,
            single_shot: false,
            batch: false,
            flex: false,
            models: vec!["claude-sonnet-5-5".into(), "claude-opus-5-5".into()],
            unverified: vec![
                "CLAUDE_CODE_USE_VERTEX/CLOUD_ML_REGION/ANTHROPIC_VERTEX_PROJECT_ID im Binary 2.1.319 vorhanden; Verhalten nicht live geprüft".into(),
                "Schlüssel wird als Application-Default-Credentials-Datei (~/.config/gcloud/application_default_credentials.json) abgelegt – Annahme aus Vorwissen".into(),
                "Claude 5.5 in europe-west6 (Zürich) nicht bestätigt".into(),
            ],
            note: String::new(),
        },
        ConnectorTypeInfo {
            kind: "anthropic_gateway".into(),
            label: "Claude über eigenes Gateway".into(),
            description: "Claude Code gegen eine eigene Anthropic-kompatible Basis-URL (z. B. ein Firmen-Gateway).".into(),
            group: "open".into(),
            harness: Some("claude".into()),
            secret_label: Some("Token".into()),
            secret_help: "Wird als ANTHROPIC_AUTH_TOKEN übergeben.".into(),
            secret_json: false,
            secret_optional: false,
            fields: vec![field("base_url", "Basis-URL", "z. B. https://gateway.example.ch", "url", true), location_field()],
            location: DataLocation::Unknown,
            single_shot: true,
            batch: false,
            flex: false,
            models: vec!["claude-sonnet-5-5".into()],
            unverified: vec!["Claude Code schaltet die Tool-Suche bei fremden Basis-URLs ab (MCP-Schemas kosten dann mehr Tokens) – sekundär".into()],
            note: String::new(),
        },
        ConnectorTypeInfo {
            kind: "codex_subscription".into(),
            label: "ChatGPT-Abo (Codex)".into(),
            description: "Dein ChatGPT-Plus/Pro-Abo über die offizielle Codex-CLI.".into(),
            group: "openai".into(),
            harness: Some("codex".into()),
            secret_label: Some("Inhalt von ~/.codex/auth.json".into()),
            secret_help: "Auf deinem Rechner `codex login` ausführen und den Dateiinhalt einfügen.".into(),
            secret_json: true,
            secret_optional: false,
            fields: vec![],
            location: DataLocation::Us,
            single_shot: false,
            batch: false,
            flex: false,
            models: vec!["gpt-6.1-sol".into(), "gpt-6.1-terra".into()],
            unverified: vec![],
            note: SUBSCRIPTION_NOTE.into(),
        },
        ConnectorTypeInfo {
            kind: "openai_api".into(),
            label: "OpenAI API".into(),
            description: "Codex per API-Schlüssel – mit Flex (−50 %, langsamer) im Agenten und Batch für Planung/Review.".into(),
            group: "openai".into(),
            harness: Some("codex".into()),
            secret_label: Some("API-Schlüssel".into()),
            secret_help: "Beginnt mit `sk-`. Erstellen unter platform.openai.com.".into(),
            secret_json: false,
            secret_optional: false,
            fields: vec![base_url_override()],
            location: DataLocation::Us,
            single_shot: true,
            batch: true,
            flex: true,
            models: vec!["gpt-6.1-terra".into(), "gpt-6.1-sol".into(), "gpt-6-astra".into(), "gpt-6-luna".into()],
            unverified: vec![
                "Codex 0.167.0 kennt `service_tier` (Wert „flex“ im Binary); dass Flex damit wirklich greift, ist nicht live geprüft".into(),
                "OpenAI-Batch-Format (Dateien + /v1/batches) aus Vorwissen, nur gegen Fake-Server getestet".into(),
                "EU-Datenhaltung der direkten OpenAI-API nicht geprüft".into(),
            ],
            note: String::new(),
        },
        ConnectorTypeInfo {
            kind: "azure_openai".into(),
            label: "Azure OpenAI (EU / Schweiz)".into(),
            description: "GPT-Modelle über Microsoft Foundry in der EU Data Zone oder in Switzerland North.".into(),
            group: "eu_ch".into(),
            harness: Some("codex".into()),
            secret_label: Some("API-Schlüssel".into()),
            secret_help: "Schlüssel der Azure-OpenAI-Ressource.".into(),
            secret_json: false,
            secret_optional: false,
            fields: vec![
                field("base_url", "Ressourcen-URL", "z. B. https://meine-ressource.openai.azure.com/openai/v1", "url", true),
                select("deployment", "Bereitstellung", "EU Data Zone +20 %, Switzerland North +30 % gegenüber global.", vec![
                    opt("eu_data_zone", "EU Data Zone"),
                    opt("switzerland_north", "Switzerland North"),
                    opt("global", "Global"),
                ]),
            ],
            location: DataLocation::Eu,
            single_shot: true,
            batch: false,
            flex: false,
            models: vec!["gpt-6.1-sol".into(), "gpt-6.1-terra".into()],
            unverified: vec![
                "Pfad /openai/v1 und Header `api-key` aus Vorwissen; nicht live geprüft".into(),
                "Name der Bereitstellung muss als Modell eingetragen werden; Preis wird nur erkannt, wenn er dem Modellnamen entspricht".into(),
            ],
            note: String::new(),
        },
        ConnectorTypeInfo {
            kind: "gemini_subscription".into(),
            label: "Google-Abo (Gemini CLI)".into(),
            description: "Google-AI-Abo über die Gemini CLI (eingefroren auf 0.67.0).".into(),
            group: "google".into(),
            harness: Some("gemini".into()),
            secret_label: Some("Inhalt von ~/.gemini/oauth_creds.json".into()),
            secret_help: "Nach der Anmeldung in der Gemini CLI auf deinem Rechner.".into(),
            secret_json: true,
            secret_optional: false,
            fields: vec![],
            location: DataLocation::Us,
            single_shot: false,
            batch: false,
            flex: false,
            models: vec!["gemini-3.8-flash".into(), "gemini-4-argon".into()],
            unverified: vec!["Sekundärquellen melden, dass der Abo-/OAuth-Pfad seit 18.6.2026 nicht mehr funktioniert und die CLI durch Antigravity ersetzt wird; das npm-Paket wird aber weiter veröffentlicht (0.67.0 vom 4.11.2026)".into()],
            note: SUBSCRIPTION_NOTE.into(),
        },
        ConnectorTypeInfo {
            kind: "gemini_api".into(),
            label: "Gemini API (über opencode)".into(),
            description: "Gemini-Modelle per API-Schlüssel, ausgeführt mit dem offenen Agenten opencode.".into(),
            group: "google".into(),
            harness: Some("opencode".into()),
            secret_label: Some("API-Schlüssel".into()),
            secret_help: "Aus Google AI Studio.".into(),
            secret_json: false,
            secret_optional: false,
            fields: vec![],
            location: DataLocation::Global,
            single_shot: false,
            batch: false,
            flex: false,
            models: vec!["gemini-3.8-flash".into(), "gemini-4-argon".into()],
            unverified: vec!["opencode-Provider `google` (@ai-sdk/google, im Binary 1.19.5 gebündelt); Lauf nicht live geprüft".into()],
            note: "Achtung: Gemini 4 Argon verdoppelt den Preis am 1.12.2026, 3.8 Flash am 1.1.2027.".into(),
        },
        ConnectorTypeInfo {
            kind: "mistral".into(),
            label: "Mistral (EU)".into(),
            description: "Mistral Large 4 & Co. über opencode, optional über den EU-Endpunkt (+10 %).".into(),
            group: "eu_ch".into(),
            harness: Some("opencode".into()),
            secret_label: Some("API-Schlüssel".into()),
            secret_help: "Aus der Mistral-Konsole (La Plateforme).".into(),
            secret_json: false,
            secret_optional: false,
            fields: vec![
                select("endpoint", "Endpunkt", "EU = api.eu.mistral.ai (+10 %), sonst globaler Endpunkt.", vec![opt("eu", "EU"), opt("global", "Global")]),
                base_url_override(),
            ],
            location: DataLocation::Eu,
            single_shot: true,
            batch: true,
            flex: false,
            models: vec!["mistral-large-4".into(), "mistral-small-4".into(), "devstral-2".into()],
            unverified: vec![
                "Modellabdeckung des EU-Endpunkts unklar".into(),
                "Mistral-Batch-Format (/v1/batch/jobs) aus Vorwissen, nur gegen Fake-Server getestet".into(),
            ],
            note: String::new(),
        },
        ConnectorTypeInfo {
            kind: "infomaniak".into(),
            label: "Infomaniak (Schweiz)".into(),
            description: "Offene Modelle bei Infomaniak in der Schweiz (Preise in CHF), über opencode.".into(),
            group: "eu_ch".into(),
            harness: Some("opencode".into()),
            secret_label: Some("API-Token".into()),
            secret_help: "Aus dem Infomaniak-Manager (AI Tools).".into(),
            secret_json: false,
            secret_optional: false,
            fields: vec![field("base_url", "OpenAI-kompatible Basis-URL", "Steht im Infomaniak-Manager bei deinem AI-Produkt.", "url", true)],
            location: DataLocation::Ch,
            single_shot: true,
            batch: false,
            flex: false,
            models: vec!["gemma-4-31B-it".into(), "Apertus-v1.5-70B".into()],
            unverified: vec![
                "Aufbau der Basis-URL nicht verifiziert (deshalb Pflichtfeld)".into(),
                "Tool-Calling-Qualität der Modelle für Agenten nicht verifiziert".into(),
            ],
            note: String::new(),
        },
        ConnectorTypeInfo {
            kind: "openai_compatible".into(),
            label: "Eigener Server (OpenAI-kompatibel)".into(),
            description: "vLLM, Ollama, SGLang oder ein anderer OpenAI-kompatibler Endpunkt, über opencode.".into(),
            group: "open".into(),
            harness: Some("opencode".into()),
            secret_label: Some("API-Schlüssel (falls nötig)".into()),
            secret_help: String::new(),
            secret_json: false,
            secret_optional: true,
            fields: vec![field("base_url", "Basis-URL", "z. B. http://gpu-server:8000/v1", "url", true), location_field()],
            location: DataLocation::Unknown,
            single_shot: true,
            batch: false,
            flex: false,
            models: vec![],
            unverified: vec!["Kosten nur, wenn ein Preis für den Modellnamen in der Preistabelle steht (Admin)".into()],
            note: String::new(),
        },
        ConnectorTypeInfo {
            kind: "openrouter".into(),
            label: "OpenRouter".into(),
            description: "Sehr viele Modelle mit einem Schlüssel, über opencode.".into(),
            group: "open".into(),
            harness: Some("opencode".into()),
            secret_label: Some("API-Schlüssel".into()),
            secret_help: String::new(),
            secret_json: false,
            secret_optional: false,
            fields: vec![select("region", "Region", "EU (eu.openrouter.ai) verarbeitet die Anfragen in der EU; Konto- und Abrechnungsdaten können trotzdem in den USA liegen.", vec![opt("global", "Global"), opt("eu", "EU")])],
            location: DataLocation::Global,
            single_shot: true,
            batch: false,
            flex: false,
            models: vec!["mistralai/mistral-large-4".into(), "anthropic/claude-sonnet-5-5".into()],
            unverified: vec![
                "EU-Routing laut Quellen nur im Business-Plan".into(),
                "Preise = Listenpreis des Modells (Annahme) + 5.5 % Gebühr (sekundär)".into(),
            ],
            note: String::new(),
        },
        ConnectorTypeInfo {
            kind: "litellm".into(),
            label: "LiteLLM-Gateway".into(),
            description: "Eigenes LiteLLM-Gateway (z. B. in CH/EU) – optional mit eigenem Budget-Schlüssel pro Auftrag.".into(),
            group: "open".into(),
            harness: None,
            secret_label: Some("Master- oder virtueller Schlüssel".into()),
            secret_help: "Mit „Budget-Schlüssel pro Auftrag“ wird ein Master-Schlüssel gebraucht.".into(),
            secret_json: false,
            secret_optional: false,
            fields: vec![
                field("base_url", "Gateway-URL", "z. B. https://llm.example.ch", "url", true),
                select("harness", "Agent", "Welche CLI über das Gateway arbeitet.", vec![opt("claude", "Claude Code (Anthropic-Format)"), opt("codex", "Codex (Responses-API)"), opt("opencode", "opencode (OpenAI-Format)")]),
                location_field(),
                select("per_task_key", "Budget-Schlüssel pro Auftrag", "Erzeugt pro Lauf einen virtuellen Schlüssel mit dem Kostenlimit des Auftrags und löscht ihn danach.", vec![opt("off", "Aus"), opt("on", "An")]),
            ],
            location: DataLocation::Unknown,
            single_shot: true,
            batch: false,
            flex: false,
            models: vec![],
            unverified: vec![
                "LiteLLM-Admin-API (/key/generate, /key/delete) aus Vorwissen, nur gegen Fake-Server getestet".into(),
                "Gateway muss cache_control und Cache-Usage-Felder durchreichen, sonst entfällt der Cache-Rabatt".into(),
            ],
            note: String::new(),
        },
    ];
    if with_mock {
        v.push(ConnectorTypeInfo {
            kind: "mock".into(),
            label: "Test-Anbieter".into(),
            description: "Deterministischer Test-Agent (nur Entwicklung/Tests).".into(),
            group: "open".into(),
            harness: Some("mock".into()),
            secret_label: Some("Test-Schlüssel (optional)".into()),
            secret_help: String::new(),
            secret_json: false,
            secret_optional: true,
            fields: vec![
                field(
                    "base_url",
                    "Fake-Server für Einzelschritte (optional)",
                    "",
                    "url",
                    false,
                ),
                select(
                    "api",
                    "API des Fake-Servers",
                    "",
                    vec![
                        opt("anthropic", "Anthropic"),
                        opt("openai", "OpenAI"),
                        opt("mistral", "Mistral"),
                    ],
                ),
                select(
                    "location",
                    "Datenstandort (Test)",
                    "",
                    vec![
                        opt("local", "Lokal"),
                        opt("ch", "Schweiz"),
                        opt("eu", "EU"),
                        opt("us", "USA"),
                        opt("unknown", "Unbekannt"),
                    ],
                ),
            ],
            location: DataLocation::Local,
            single_shot: true,
            batch: true,
            flex: true,
            models: vec!["claude-sonnet-5-5".into(), "claude-haiku-5-5".into()],
            unverified: vec![],
            note: String::new(),
        });
    }
    v
}

pub fn type_info(kind: &str, with_mock: bool) -> Option<ConnectorTypeInfo> {
    types(with_mock).into_iter().find(|t| t.kind == kind)
}

fn cfg_str<'a>(config: &'a Value, key: &str) -> Option<&'a str> {
    config
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn valid_url(u: &str, allow_http: bool) -> bool {
    let Ok(p) = url::Url::parse(u) else {
        return false;
    };
    let scheme_ok = p.scheme() == "https" || (allow_http && p.scheme() == "http");
    scheme_ok && p.host_str().is_some() && p.username().is_empty() && p.password().is_none()
}

/// Validates and normalises the (non-secret) configuration of a connector.
pub fn normalize_config(t: &ConnectorTypeInfo, config: &Value) -> Result<Value, String> {
    let obj = match config {
        Value::Null => serde_json::Map::new(),
        Value::Object(o) => o.clone(),
        _ => return Err("Konfiguration muss ein Objekt sein".into()),
    };
    let mut out = serde_json::Map::new();
    for f in &t.fields {
        let raw = obj
            .get(&f.key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .or_else(|| f.default.clone());
        let Some(v) = raw else {
            if f.required {
                return Err(format!("„{}“ fehlt", f.label));
            }
            continue;
        };
        if v.len() > 500 {
            return Err(format!("„{}“ ist zu lang", f.label));
        }
        if f.input == "select" && !f.options.iter().any(|o| o.value == v) {
            return Err(format!("„{}“: ungültige Auswahl", f.label));
        }
        if f.input == "url" {
            // Self-hosted endpoints in a LAN often have no TLS.
            let http_ok = matches!(
                t.kind.as_str(),
                "openai_compatible" | "litellm" | "anthropic_gateway" | "mock"
            ) || f.key == "base_url" && !f.required;
            if !valid_url(&v, http_ok) {
                return Err(format!(
                    "„{}“: gültige URL mit https:// angeben (ohne Zugangsdaten in der URL)",
                    f.label
                ));
            }
        }
        if f.key == "region"
            && t.kind == "bedrock"
            && !v
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err("AWS-Region ungültig".into());
        }
        if f.key == "project_id" && !v.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err("Projekt-ID ungültig".into());
        }
        out.insert(f.key.clone(), Value::String(v));
    }
    if t.kind == "bedrock" {
        let geo = cfg_str(&Value::Object(out.clone()), "geo")
            .unwrap_or("eu")
            .to_string();
        let region = cfg_str(&Value::Object(out.clone()), "region")
            .unwrap_or("")
            .to_string();
        if geo == "eu" && !region.starts_with("eu-") {
            return Err("Für den EU-Datenraum muss die AWS-Region in der EU liegen (eu-…)".into());
        }
    }
    Ok(Value::Object(out))
}

/// Validates a secret for a type (`None` = keep / none).
pub fn check_secret(
    t: &ConnectorTypeInfo,
    secret: Option<&str>,
    creating: bool,
) -> Result<(), String> {
    let s = secret.map(str::trim).filter(|s| !s.is_empty());
    match (t.secret_label.is_some(), s) {
        (false, Some(_)) => Err("Dieser Zugang braucht keinen Schlüssel".into()),
        (true, None) if creating && !t.secret_optional => Err("Schlüssel fehlt".into()),
        (_, Some(v)) if v.len() > 64 * 1024 => Err("Schlüssel ist zu lang".into()),
        (_, Some(v))
            if t.secret_json
                && serde_json::from_str::<Value>(v)
                    .map(|j| !j.is_object())
                    .unwrap_or(true) =>
        {
            Err("Erwartet wird der Inhalt der JSON-Datei".into())
        }
        _ => Ok(()),
    }
}

/// Harness of a connector (LiteLLM: from its config).
pub fn harness(kind: &str, config: &Value) -> String {
    if kind == "litellm" {
        return cfg_str(config, "harness").unwrap_or("opencode").to_string();
    }
    type_info(kind, true)
        .and_then(|t| t.harness)
        .unwrap_or_else(|| "opencode".into())
}

/// Data location and whether it was declared by the user.
pub fn location(kind: &str, config: &Value) -> (DataLocation, bool) {
    let declared = |c: &Value| {
        (
            cfg_str(c, "location")
                .and_then(DataLocation::parse)
                .unwrap_or(DataLocation::Unknown),
            true,
        )
    };
    match kind {
        "claude_subscription"
        | "anthropic_api"
        | "codex_subscription"
        | "openai_api"
        | "gemini_subscription" => (DataLocation::Us, false),
        "gemini_api" => (DataLocation::Global, false),
        "bedrock" => (
            match cfg_str(config, "geo").unwrap_or("eu") {
                "eu" => DataLocation::Eu,
                "us" => DataLocation::Us,
                _ => DataLocation::Global,
            },
            false,
        ),
        "vertex" => (
            match cfg_str(config, "region").unwrap_or("eu") {
                "europe-west6" => DataLocation::Ch,
                r if r == "eu" || r.starts_with("europe-") => DataLocation::Eu,
                r if r.starts_with("us") => DataLocation::Us,
                _ => DataLocation::Global,
            },
            false,
        ),
        "azure_openai" => (
            match cfg_str(config, "deployment").unwrap_or("eu_data_zone") {
                "eu_data_zone" => DataLocation::Eu,
                "switzerland_north" => DataLocation::Ch,
                _ => DataLocation::Global,
            },
            false,
        ),
        "mistral" => (
            if cfg_str(config, "endpoint").unwrap_or("eu") == "eu" {
                DataLocation::Eu
            } else {
                DataLocation::Global
            },
            false,
        ),
        "infomaniak" => (DataLocation::Ch, false),
        "openrouter" => (
            if cfg_str(config, "region") == Some("eu") {
                DataLocation::Eu
            } else {
                DataLocation::Global
            },
            false,
        ),
        "mock" => match cfg_str(config, "location") {
            Some(_) => declared(config),
            None => (DataLocation::Local, false),
        },
        _ => declared(config),
    }
}

/// Regional surcharge rule of the price catalog for this connector.
pub fn region_rule(kind: &str, config: &Value) -> Option<&'static str> {
    match kind {
        "bedrock" if cfg_str(config, "geo").unwrap_or("eu") != "global" => Some("bedrock_eu"),
        "vertex" if cfg_str(config, "region").unwrap_or("eu") != "global" => Some("vertex_eu"),
        "mistral" if cfg_str(config, "endpoint").unwrap_or("eu") == "eu" => Some("mistral_eu"),
        "azure_openai" => match cfg_str(config, "deployment").unwrap_or("eu_data_zone") {
            "eu_data_zone" => Some("azure_eu_data_zone"),
            "switzerland_north" => Some("azure_switzerland_north"),
            _ => None,
        },
        "openrouter" => Some("openrouter_fee"),
        _ => None,
    }
}

/// Direct single-shot wire and base URL (`None` = not possible).
pub fn wire(kind: &str, config: &Value) -> Option<(Wire, String)> {
    let base = cfg_str(config, "base_url").map(|s| s.trim_end_matches('/').to_string());
    match kind {
        "anthropic_api" => Some((
            Wire::Anthropic,
            base.unwrap_or_else(|| ANTHROPIC_BASE.into()),
        )),
        "anthropic_gateway" => Some((Wire::Anthropic, base?)),
        "openai_api" => Some((Wire::OpenAiChat, base.unwrap_or_else(|| OPENAI_BASE.into()))),
        "azure_openai" | "infomaniak" | "openai_compatible" => Some((Wire::OpenAiChat, base?)),
        "mistral" => Some((
            Wire::OpenAiChat,
            base.unwrap_or_else(|| {
                if cfg_str(config, "endpoint").unwrap_or("eu") == "eu" {
                    MISTRAL_EU_BASE.into()
                } else {
                    MISTRAL_BASE.into()
                }
            }),
        )),
        "openrouter" => Some((
            Wire::OpenAiChat,
            if cfg_str(config, "region") == Some("eu") {
                OPENROUTER_EU_BASE.into()
            } else {
                OPENROUTER_BASE.into()
            },
        )),
        "litellm" => {
            let b = base?;
            if harness(kind, config) == "claude" {
                Some((Wire::Anthropic, b))
            } else {
                Some((Wire::OpenAiChat, format!("{b}/v1")))
            }
        }
        "mock" => {
            let b = base?;
            match cfg_str(config, "api").unwrap_or("anthropic") {
                "anthropic" => Some((Wire::Anthropic, b)),
                _ => Some((Wire::OpenAiChat, format!("{b}/v1"))),
            }
        }
        _ => None,
    }
}

/// Batch API (only where the provider offers one and it is wired).
pub fn batch_api(kind: &str, config: &Value) -> Option<BatchApi> {
    match kind {
        "anthropic_api" => Some(BatchApi::Anthropic),
        "openai_api" => Some(BatchApi::OpenAi),
        "mistral" => Some(BatchApi::Mistral),
        "mock" if cfg_str(config, "base_url").is_some() => {
            Some(match cfg_str(config, "api").unwrap_or("anthropic") {
                "openai" => BatchApi::OpenAi,
                "mistral" => BatchApi::Mistral,
                _ => BatchApi::Anthropic,
            })
        }
        _ => None,
    }
}

/// Header used for the API key of direct calls.
pub fn auth_header(kind: &str) -> &'static str {
    match kind {
        "azure_openai" => "api-key",
        _ => "authorization",
    }
}

/// A connector as loaded for a run (secret decrypted in memory only).
#[derive(Clone)]
pub struct Resolved {
    pub id: Uuid,
    pub user_id: Uuid,
    pub project_id: Option<Uuid>,
    pub kind: String,
    pub label: String,
    pub config: Value,
    pub secret: Option<String>,
    pub harness: String,
    pub location: DataLocation,
    pub declared: bool,
}

impl std::fmt::Debug for Resolved {
    // Never print the secret.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Resolved")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("label", &self.label)
            .field("harness", &self.harness)
            .field("location", &self.location)
            .finish_non_exhaustive()
    }
}

impl Resolved {
    pub fn limit_key(&self) -> String {
        limit_key(self.id)
    }

    pub fn wire(&self) -> Option<(Wire, String)> {
        wire(&self.kind, &self.config)
    }

    pub fn batch_api(&self) -> Option<BatchApi> {
        batch_api(&self.kind, &self.config)
    }

    pub fn region_rule(&self) -> Option<&'static str> {
        region_rule(&self.kind, &self.config)
    }

    pub fn cfg(&self, key: &str) -> Option<&str> {
        cfg_str(&self.config, key)
    }
}

/// Key in `provider_limits` for pauses of one connector.
pub fn limit_key(id: Uuid) -> String {
    format!("connector:{id}")
}

pub type Row = (
    Uuid,
    Uuid,
    Option<Uuid>,
    String,
    String,
    Value,
    Option<Vec<u8>>,
    chrono::DateTime<chrono::Utc>,
    chrono::DateTime<chrono::Utc>,
);

pub const SELECT: &str =
    "SELECT id, user_id, project_id, kind, label, config, secret_enc, created_at, updated_at FROM connectors";

/// May `user` use connector `row` for a task in `project`?
/// User-wide connectors: only their owner. Project connectors: editors of that project.
pub async fn can_use(
    st: &AppState,
    row: &Row,
    user: Uuid,
    project: Option<Uuid>,
) -> sqlx::Result<bool> {
    match row.2 {
        None => Ok(row.1 == user),
        Some(pid) => {
            if project != Some(pid) {
                return Ok(false);
            }
            sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM project_members WHERE project_id=$1 AND user_id=$2 AND role IN ('owner','editor'))",
            )
            .bind(pid)
            .bind(user)
            .fetch_one(&st.db)
            .await
        }
    }
}

pub async fn load_row(st: &AppState, id: Uuid) -> sqlx::Result<Option<Row>> {
    sqlx::query_as(&format!("{SELECT} WHERE id=$1"))
        .bind(id)
        .fetch_optional(&st.db)
        .await
}

/// Loads a connector for use by `user` in `project` (access checked).
/// `Ok(None)` = does not exist or not accessible.
pub async fn load_for(
    st: &AppState,
    id: Uuid,
    user: Uuid,
    project: Option<Uuid>,
) -> anyhow::Result<Option<Resolved>> {
    let Some(row) = load_row(st, id).await? else {
        return Ok(None);
    };
    if !can_use(st, &row, user, project).await? {
        return Ok(None);
    }
    Ok(Some(resolve(st, row)?))
}

pub fn resolve(st: &AppState, row: Row) -> anyhow::Result<Resolved> {
    let (id, user_id, project_id, kind, label, config, enc, _, _) = row;
    let secret = match enc {
        Some(e) => Some(String::from_utf8(crypto::decrypt(&st.cfg.master_key, &e)?)?),
        None => None,
    };
    let (location, declared) = location(&kind, &config);
    Ok(Resolved {
        id,
        user_id,
        project_id,
        harness: harness(&kind, &config),
        kind,
        label,
        config,
        secret,
        location,
        declared,
    })
}

pub fn view(row: &Row, can_manage: bool) -> ConnectorView {
    let (location, declared) = location(&row.3, &row.5);
    ConnectorView {
        id: row.0,
        user_id: row.1,
        project_id: row.2,
        kind: row.3.clone(),
        label: row.4.clone(),
        config: row.5.clone(),
        has_secret: row.6.is_some(),
        harness: harness(&row.3, &row.5),
        location,
        location_declared: declared,
        single_shot: wire(&row.3, &row.5).is_some(),
        batch: batch_api(&row.3, &row.5).is_some(),
        can_manage,
        paused_reason: None,
        limited_until: None,
        created_at: row.7,
        updated_at: row.8,
    }
}

/// German explanation when a location violates a residency rule.
pub fn residency_error(r: Residency, loc: DataLocation, what: &str) -> String {
    format!(
        "{what} verarbeitet Daten in „{}“ – das Projekt erlaubt nur „{}“.",
        loc.label(),
        r.label()
    )
}

/// Legacy providers (credential under Einstellungen → Zugänge).
pub fn legacy_location(provider: &str) -> DataLocation {
    match provider {
        "mock" => DataLocation::Local,
        _ => DataLocation::Us,
    }
}

/// Redacts every occurrence of `secret` (error texts of providers).
pub fn redact(text: &str, secret: Option<&str>) -> String {
    match secret.map(str::trim).filter(|s| s.len() >= 4) {
        Some(s) => text.replace(s, "***"),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_type_is_consistent() {
        let all = types(true);
        let mut kinds: Vec<&str> = all.iter().map(|t| t.kind.as_str()).collect();
        kinds.sort();
        kinds.dedup();
        assert_eq!(kinds.len(), all.len());
        for t in &all {
            // Default config validates (required fields aside) and the
            // computed capabilities match the description.
            let mut cfg = serde_json::Map::new();
            for f in &t.fields {
                let v = match (f.key.as_str(), f.input.as_str()) {
                    (_, "select") => f.default.clone().unwrap(),
                    ("base_url", _) => "https://example.ch/v1".into(),
                    ("project_id", _) => "my-proj".into(),
                    ("region", _) => "eu-central-1".into(),
                    _ => continue,
                };
                cfg.insert(f.key.clone(), Value::String(v));
            }
            let cfg = normalize_config(t, &Value::Object(cfg))
                .unwrap_or_else(|e| panic!("{}: {e}", t.kind));
            assert_eq!(
                wire(&t.kind, &cfg).is_some(),
                t.single_shot,
                "{} single_shot",
                t.kind
            );
            assert_eq!(
                batch_api(&t.kind, &cfg).is_some(),
                t.batch,
                "{} batch",
                t.kind
            );
            let h = harness(&t.kind, &cfg);
            assert!(
                ["claude", "codex", "gemini", "opencode", "mock"].contains(&h.as_str()),
                "{}",
                t.kind
            );
            if let Some(th) = &t.harness {
                assert_eq!(&h, th);
            }
            assert!(!t.label.is_empty() && !t.description.is_empty());
        }
        assert!(types(false).iter().all(|t| t.kind != "mock"));
    }

    #[test]
    fn locations_follow_configuration() {
        let c = |v: Value| v;
        assert_eq!(
            location("anthropic_api", &json!({})),
            (DataLocation::Us, false)
        );
        assert_eq!(
            location("bedrock", &c(json!({"geo":"eu"}))).0,
            DataLocation::Eu
        );
        assert_eq!(
            location("bedrock", &c(json!({"geo":"us"}))).0,
            DataLocation::Us
        );
        assert_eq!(
            location("vertex", &c(json!({"region":"europe-west6"}))).0,
            DataLocation::Ch
        );
        assert_eq!(
            location("vertex", &c(json!({"region":"eu"}))).0,
            DataLocation::Eu
        );
        assert_eq!(
            location("vertex", &c(json!({"region":"global"}))).0,
            DataLocation::Global
        );
        assert_eq!(
            location(
                "azure_openai",
                &c(json!({"deployment":"switzerland_north"}))
            )
            .0,
            DataLocation::Ch
        );
        assert_eq!(
            location("mistral", &c(json!({"endpoint":"global"}))).0,
            DataLocation::Global
        );
        assert_eq!(location("infomaniak", &json!({})).0, DataLocation::Ch);
        assert_eq!(
            location("openai_compatible", &c(json!({"location":"ch"}))),
            (DataLocation::Ch, true)
        );
        assert_eq!(
            location("openai_compatible", &json!({})),
            (DataLocation::Unknown, true)
        );
        assert_eq!(location("mock", &json!({})).0, DataLocation::Local);
        assert_eq!(
            region_rule("mistral", &json!({"endpoint":"eu"})),
            Some("mistral_eu")
        );
        assert_eq!(region_rule("mistral", &json!({"endpoint":"global"})), None);
        assert_eq!(
            region_rule("bedrock", &json!({"geo":"eu"})),
            Some("bedrock_eu")
        );
        assert_eq!(
            wire("mistral", &json!({"endpoint":"eu"})).unwrap().1,
            MISTRAL_EU_BASE
        );
        assert_eq!(
            wire(
                "litellm",
                &json!({"base_url":"https://g.ch","harness":"claude"})
            )
            .unwrap()
            .0,
            Wire::Anthropic
        );
        assert_eq!(
            wire(
                "litellm",
                &json!({"base_url":"https://g.ch","harness":"codex"})
            )
            .unwrap()
            .1,
            "https://g.ch/v1"
        );
        assert!(wire("bedrock", &json!({})).is_none());
    }

    #[test]
    fn config_validation() {
        let t = type_info("bedrock", false).unwrap();
        assert!(
            normalize_config(&t, &json!({"geo":"eu","region":"us-east-1"}))
                .unwrap_err()
                .contains("EU")
        );
        assert!(normalize_config(&t, &json!({"geo":"eu","region":"eu central"})).is_err());
        let ok = normalize_config(&t, &json!({"region":"eu-central-1","junk":"x"})).unwrap();
        assert_eq!(
            ok,
            json!({"geo":"eu","region":"eu-central-1"}),
            "unknown keys dropped, defaults filled"
        );
        let t = type_info("infomaniak", false).unwrap();
        assert!(normalize_config(&t, &json!({}))
            .unwrap_err()
            .contains("fehlt"));
        assert!(
            normalize_config(&t, &json!({"base_url":"http://x.ch"})).is_err(),
            "https required"
        );
        assert!(
            normalize_config(&t, &json!({"base_url":"https://u:p@x.ch"})).is_err(),
            "no credentials in URLs"
        );
        let t = type_info("openai_compatible", false).unwrap();
        assert!(normalize_config(&t, &json!({"base_url":"http://gpu:8000/v1"})).is_ok());
        let t = type_info("mistral", false).unwrap();
        assert!(normalize_config(&t, &json!({"endpoint":"mars"})).is_err());
        assert!(normalize_config(&t, &json!([1])).is_err());
    }

    #[test]
    fn secret_validation_and_redaction() {
        let vertex = type_info("vertex", false).unwrap();
        assert!(check_secret(&vertex, Some("not json"), true).is_err());
        assert!(check_secret(&vertex, Some("[1]"), true).is_err());
        assert!(check_secret(&vertex, Some("{\"type\":\"service_account\"}"), true).is_ok());
        assert!(check_secret(&vertex, None, true).is_err());
        assert!(
            check_secret(&vertex, None, false).is_ok(),
            "update keeps the secret"
        );
        let oc = type_info("openai_compatible", false).unwrap();
        assert!(check_secret(&oc, None, true).is_ok());
        assert_eq!(
            redact("bad key sk-123456 here", Some("sk-123456")),
            "bad key *** here"
        );
        assert_eq!(
            redact("abc", Some("ab")),
            "abc",
            "too short to redact safely"
        );
    }
}
