//! Backend error messages (partly English, written for developers) in
//! plain German for the UI. Unknown messages are shown unchanged.

/// Message shown when the request never reached the server.
pub const NETWORK: &str = "Keine Verbindung zum Server. Bitte prüfe deine Internetverbindung.";

pub fn translate(status: u16, message: &str) -> String {
    let m = message.trim();
    let known = match m {
        "invalid credentials" => Some("E-Mail oder Passwort ist falsch."),
        "valid email and password (min. 8 chars) required" => {
            Some("Bitte eine gültige E-Mail-Adresse und ein Passwort mit mindestens 8 Zeichen angeben.")
        }
        "already exists" => Some("Das gibt es schon (z. B. ein Konto mit dieser E-Mail-Adresse)."),
        "registration disabled" => Some("Neue Konten können hier nicht angelegt werden."),
        "not authenticated" => Some("Du bist nicht mehr angemeldet. Bitte melde dich erneut an."),
        "forbidden" => Some("Dafür fehlen dir die Rechte."),
        "not found" => Some("Nicht gefunden – vielleicht wurde es inzwischen gelöscht."),
        "prompt required" => Some("Bitte beschreibe, was erledigt werden soll."),
        "name required" => Some("Bitte gib einen Namen ein."),
        "unknown provider" => Some("Dieser Anbieter ist hier nicht verfügbar."),
        "parent task not in project" => {
            Some("Der gewählte Vorgänger-Auftrag gehört zu einem anderen Projekt.")
        }
        "task is not in a suitable state" | "task is not waiting for input" => {
            Some("Der Auftrag hat sich inzwischen geändert. Die Ansicht ist jetzt aktuell.")
        }
        "only queued tasks can be moved" => Some("Nur wartende Aufträge lassen sich verschieben."),
        "only waiting tasks can be edited" => {
            Some("Nur Aufträge, die noch nicht laufen, lassen sich bearbeiten.")
        }
        "a task is running in this project" => {
            Some("In diesem Projekt läuft gerade ein Auftrag. Bitte warte, bis er fertig ist.")
        }
        "task has no commits" => Some("Dieser Auftrag hat keine Änderungen gespeichert."),
        "git remote must be an https:// or ssh URL" => {
            Some("Die Repository-Adresse muss mit https:// oder ssh:// beginnen (oder git@…).")
        }
        "invalid branch name" => Some("Der Branch-Name ist ungültig."),
        "no git remote configured" => Some("Für dieses Projekt ist kein Remote-Repository eingetragen."),
        "no user with this email – they must sign in once first" => Some(
            "Unter dieser E-Mail-Adresse gibt es kein Konto. Die Person muss sich zuerst einmal anmelden.",
        ),
        "role must be editor or viewer" => Some("Bitte wähle „Bearbeiten“ oder „Nur ansehen“."),
        "secret required" => Some("Bitte füge den Zugang ein."),
        "expected the JSON file content" => {
            Some("Bitte füge den kompletten Inhalt der JSON-Datei ein.")
        }
        _ => None,
    };
    if let Some(k) = known {
        return k.to_string();
    }
    if let Some(p) = m.strip_prefix("push failed: ") {
        return format!("Hochladen (Push) fehlgeschlagen: {p}");
    }
    if m.is_empty() {
        return match status {
            401 => "Du bist nicht mehr angemeldet.".into(),
            403 => "Dafür fehlen dir die Rechte.".into(),
            404 => "Nicht gefunden.".into(),
            s if s >= 500 => "Auf dem Server ist ein Fehler aufgetreten.".into(),
            s => format!("Anfrage fehlgeschlagen ({s})."),
        };
    }
    if status >= 500 && m.eq_ignore_ascii_case("internal server error") {
        return "Auf dem Server ist ein Fehler aufgetreten.".into();
    }
    m.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_messages_are_german() {
        assert_eq!(
            translate(401, "invalid credentials"),
            "E-Mail oder Passwort ist falsch."
        );
        assert!(
            translate(409, "a task is running in this project").starts_with("In diesem Projekt")
        );
        assert!(translate(400, "push failed: rejected").ends_with("rejected"));
    }

    #[test]
    fn unknown_and_empty_messages() {
        // Already German backend messages pass through.
        assert_eq!(
            translate(400, "Max. Tokens muss zwischen 1 und 10 liegen"),
            "Max. Tokens muss zwischen 1 und 10 liegen"
        );
        assert_eq!(translate(404, ""), "Nicht gefunden.");
        assert_eq!(
            translate(502, ""),
            "Auf dem Server ist ein Fehler aufgetreten."
        );
        assert_eq!(
            translate(500, "Internal Server Error"),
            "Auf dem Server ist ein Fehler aufgetreten."
        );
        assert_eq!(translate(418, ""), "Anfrage fehlgeschlagen (418).");
    }
}
