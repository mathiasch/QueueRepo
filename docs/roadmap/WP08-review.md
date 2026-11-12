# WP08 – Review (Selbst-Review, harte Lesart)

Gegenstand: kompletter WP08-Diff gegen `04d7457` (api, app inkl. Migration 0011, Frontend, E2E, README). Maßstab: `work-packages.md` (Gemeinsame Regeln, WP08), Auftrag des Koordinators (Signatur je Integration nach offizieller Doku, Zuordnung extern → intern restriktiv und mandantengetrennt, untrusted Input nur als Datenblock mit Limits/Dedupe/Rate-Limit, Rückmeldungen ohne Secrets und ohne ungeprüfte Diffs, idempotent mit Retry/Backoff, nicht im Scheduler, nur über den SSRF-Client, Tokens verschlüsselt; WP06-Fund `answer`; UI nach WP00), Report P2.6. Jede Feststellung ist am Code oder an einem Testlauf geprüft.

## Urteil

**Abnahmefähig mit dokumentierten Restrisiken.** Alle blockierenden Funde sind behoben und mit Tests belegt; die komplette Suite ist auf dem finalen Stand zweimal hintereinander grün (`WP08-test.md`). Die ehrlichen Grenzen: Gegen GitHub, Linear, Slack oder Jira selbst ist nichts gelaufen; Slack, Linear und GitHub sind aus offiziellen SDKs/Schemas belegt (Slack und GitHub mit Testvektor), **Jira nur über eine Sekundärquelle**; App-JWT (Jira Connect) gibt es nicht; ob ein externes Konto wirklich der Person gehört, prüft Queue nicht (die Eigentümerin entscheidet).

## Akzeptanz gegen WP08 und den Auftrag

| Anforderung | Status | Beleg |
|---|---|---|
| GitHub: Issue mit Label/Erwähnung → Auftrag, Status-Kommentare zurück | erfüllt (Label bei `opened`/`reopened`/`labeled`, Erwähnung im Issue oder Kommentar; Kommentare angenommen/Rückfrage/fertig/fehlgeschlagen/abgebrochen) | Integration `github_issues_end_to_end`, System-E2E, UI-E2E |
| Linear: Delegieren → Auftrag, Activities zurück | erfüllt (`created` ⇒ `thought` sofort über Weckruf der Outbox, `elicitation`, `response`/`error`; `prompted` ⇒ Antwort/Nachricht/„Weiter mit …“; Ablehnung ⇒ `error`) | `linear_agent_sessions_and_activities`, System-E2E |
| Slack: Mention/Slash → Auftrag, Thread-Updates, Rückfragen im Thread | erfüllt (Wurzelnachricht beim Slash-Befehl, Erwähnung im Thread = Nachricht/„Weiter mit …“) | `slack_mentions_threads_and_slash_commands`, System-E2E |
| Jira: Label → Auftrag, Kommentar zurück | erfüllt (Cloud-Webhook mit Secret; ADF) | `jira_issues_and_comments`, System-E2E |
| Gemeinsame Abstraktion, Verknüpfung Auftrag ↔ Objekt | erfüllt (`intake::{verify,parse,accept}` + Rückkanal je Art; WP07-Primitive wiederverwendet; `intake_links`, `TaskDetail.intake`, Drawer-Block mit Link) | Code; UI-E2E „Aus GitHub-Issue #9“ |
| Signatur je Integration nach Doku, Testvektoren | erfüllt: GitHub (Doku-Vektor, WP07), Slack v0 ±5 min (**Doku-/SDK-Vektor**), Linear Body-HMAC + signierter Zeitstempel 60 s, Jira `X-Hub-Signature` (Sekundärquelle, markiert); Vergleich in konstanter Zeit | Unit je Modul, Integration (401 ohne Spur) |
| Replay | erfüllt: DB-Dedupe (GitHub Delivery+Body, Slack Nachricht/Trigger-ID + Zeitfenster, Linear Sitzung/Activity + 60 s, Jira Identifier+Body) | Integration je Art („duplicate“) |
| Zuordnung restriktiv, beide Zustimmungen, Opt-in nur auf Kosten der Eigentümerin, GitHub-Opt-in mit Schreibrecht | erfüllt | `rights_identities_and_tenants`, `github_opt_in…`, UI-E2E „member maps …; owner confirms“ |
| Mandantentrennung (ein Arbeitsbereich = ein Projekt, kein Cross-Tenant) | erfüllt (`UNIQUE (kind, tenant)`, Mandant nach der Signatur geprüft ⇒ 403) | Integration je Art (fremder Workspace/Repo/Site) + 409 bei zweitem Projekt |
| Untrusted Input nur als Datenblock, Längenlimits, Dedupe, Rate-Limit | erfüllt (`EVENT_DATA` mit eigenen Regeln, ≤ 8 000 Zeichen + 16 KB Block, Titel fest; Antworten als `USER_MSG` ≤ 4 000; Limit/Stunde ⇒ 429) | Integration (Kontext/Titel/Grenzen entschärft), `github_opt_in…` (429) |
| WP06-Fund `answer` ohne Trennung/Limit | behoben (Nachrichtenpfad) | `answer_endpoint_goes_through_the_message_path`, angepasster Bestandstest |
| Rückmeldungen: keine Secrets, Diffs nur gekürzt/gescannt | erfüllt und strenger: **kein Diff**, Zusammenfassung durch den Scanner, Token-String entfernt, Slack-Escaping, `@` bei GitHub entschärft | Unit (Texte), Integration (Fake-Log ohne Token im Text) |
| Idempotenz, Retry mit Backoff, Scheduler nicht blockiert | erfüllt (Outbox-UNIQUE, Suche nach Marker/ID vor Wiederholung, Backoff/`Retry-After`/8 Versuche, eigene Schleife, eine Zeile je Lease, zwei Zusteller parallel) | „lost“-Fälle je Art, `updates_go_out_once_with_parallel_workers…` |
| Ausgehend nur über den SSRF-Client | erfüllt (`llm::guarded_builder` + `check_target`, keine Redirects, Jira nur `*.atlassian.net`) | Code; Validierungstest (`http://…` ⇒ 400) |
| Tokens/Secrets verschlüsselt, nie in Logs/argv/API | erfüllt | Integration (Antworten ohne Secret/Token, Klartext nicht in der DB), Code (`Debug` ohne Token, Fehlertexte geschwärzt) |
| UI (verbinden, Zuordnung, Verknüpfung mit Link), WP00-Designsystem, Safe Areas, axe | erfüllt | UI-E2E Desktop + iPhone hoch/quer, Tour hell/dunkel |
| README Einrichtung je Integration mit minimalen Rechten | erfüllt (Fine-grained-Rechtenamen aus der GitHub-Doku-Kenntnis, Slack-Scopes, Linear-Scopes, Jira-Rechte) | README „Anbindungen“ |

## Blockierende Funde (alle behoben)

| # | Fund | Wie gefunden | Behebung | Beleg |
|---|---|---|---|---|
| B1 | **`answer` hängte Antworten roh und unbegrenzt an den Kontext** (WP06-N8) – externe Antworten wären so als Anweisung beim Agenten gelandet. | Auftrag/WP06-Review | über `queue_message` (Limit, eigener Block); Bestandstest prüft jetzt das neue Verhalten | `answer_endpoint…`, `api::question_is_answered…` |
| B2 | **Slack-Erwähnung auf oberster Ebene konnte verloren gehen:** Slack schickt dieselbe Nachricht als `message` und `app_mention`; kam `message` zuerst, verbrauchte das Ignorieren den Dedupe-Schlüssel, die Erwähnung galt als Duplikat. | Selbst-Review | ignorierte `message`-Ereignisse behalten keinen Schlüssel | Unit + Integration (erst `message`, dann `app_mention` ⇒ 201) |
| B3 | **Zwei Aufträge pro Issue:** GitHub schickt beim Anlegen mit Label `opened` und `labeled`; war der erste Auftrag schon fertig, legte das zweite Ereignis einen neuen an (ebenso erneut gesetzte Labels). | Selbst-Review | ein Objekt = ein Auftrag; neu nur bei `reopened`, sonst per Erwähnung als „Weiter mit …“ | Integration (Label erneut ⇒ ignoriert, `reopened` ⇒ 201) |
| B4 | **Doppelversand bei mehreren Instanzen:** `deliver` lieh 20 Zeilen mit 60-s-Lease und arbeitete sie nacheinander mit bis zu 2 × 15 s ab – die Lease konnte ablaufen. | Selbst-Review (Rechnung) | eine Zeile je Claim | `updates_go_out_once_with_parallel_workers…` |
| B5 | **Dieselbe Rückfrage ein zweites Mal wurde nie gemeldet** (Schlüssel nur aus Auftrag + Fragen-Hash) – die externe Person hätte nie erfahren, dass Queue wieder wartet. | Selbst-Review | Episoden-Zähler im Schlüssel | gleicher Test (zweite Rückfrage ⇒ dritter Kommentar) |
| B6 | **Erwähnung in Thread/Issue mit fertigem Auftrag erzeugte einen zweiten, unverbundenen Auftrag** statt „Weiter mit …“. | Integrationstest-Entwurf | `reply_if_open` gilt für jede bestehende Verknüpfung | Slack-Test („continued“) |
| B7 | **Benachrichtigungs-Spam ohne Token:** jede Rückmeldung wurde eingereiht, scheiterte sofort und benachrichtigte die Eigentümerin. | Log des UI-E2E | ohne Token nichts einreihen; Drawer sagt „Rückmeldungen aus“; Slash-Antwort verspricht nichts | `rights…` (0 Outbox-Zeilen, `updates=false`) |
| B8 | **Personen-Bereich klappte nach jeder Zuordnung zu** (Listen-Reload baute die Karte neu). | UI-E2E rot | Karte aktualisiert nur ihre Zusammenfassung | UI-E2E grün |

## Nicht blockierende Funde (offen, bewusst)

| # | Fund | Bewertung |
|---|---|---|
| N1 | **Nicht gegen die echten Dienste geprüft.** Unbelegt: Linear-Fehlertext bei doppelter Activity-ID, Inhalt von Jiras `issue.self` in allen Varianten, „Labels setzen braucht Triage“ bei GitHub, Slack-Fehlercodes außerhalb der SDK. | Netz-Proxy sperrt die Dienste; Fehler zeigen sich im Verlauf bzw. als „nicht zugestellt“, nicht still. |
| N2 | **Jira nur Sekundärquelle** (`jira.js`), **kein App-JWT (Connect)**. | Variante „Cloud-Webhook mit Secret“ ist die vom Auftrag erlaubte; Connect bräuchte ein eigenes Installationsmodell. |
| N3 | **Externe Identität nicht nachgewiesen** (kein OAuth-Login beim Dienst). Ein Mitglied kann ein fremdes externes Konto beanspruchen; wirksam wird es erst mit Freigabe der Eigentümerin. | Bewusst; OAuth je Dienst wäre ein eigenes Paket. |
| N4 | **Besetzen eines Arbeitsbereichs:** Wer einen Workspace/ein Repo zuerst verbindet, blockiert alle anderen (`UNIQUE`). | Selbst gehostet, registrierte Nutzer; Folge der geforderten Eindeutigkeit. Lösen nur durch die Eigentümerin jenes Projekts oder in der DB. |
| N5 | **Slack-Wurzelnachricht** eines Slash-Befehls kann bei verlorener Antwort doppelt erscheinen (ohne Thread nichts nachzuschlagen). | Selten (Antwort verloren, nachdem Slack gespeichert hat); alle Thread-Antworten sind abgesichert. |
| N6 | **GitHub/Jira signieren keinen Zeitstempel:** Replay nach 90 Tagen (Aufräumen des Verlaufs) möglich. | Wie WP07-N2; braucht eine abgefangene, gültig signierte Zustellung. |
| N7 | **Linear-Stopp-Signal bricht nicht ab** (nur Hinweis im Verlauf). | Abbrechen bleibt in Queue; ein externes Abbrechen wäre eine neue Steuerfläche. |
| N8 | **Ungültig signierte Anfragen** kosten je einen DB-Zugriff und werden nicht gedrosselt. | Wie WP07-N4: Reverse-Proxy. |
| N9 | **Ausgeschaltete Anbindung meldet weiter** den Stand laufender Aufträge. | Bewusst: „aus“ heißt „nimmt nichts mehr an“; Token entfernen stoppt Rückmeldungen. |
| N10 | **WP07-GitHub-Auslöser und GitHub-Anbindung** auf demselben Repo reagieren beide, wenn beide passen. | Konfiguration; im README genannt. |
| N11 | Body-Limit 1 MiB (Linear-SDK erlaubt 5 MB). | Prompt-Kontext wird ohnehin auf 8 000 Zeichen gekürzt; zu große Zustellungen sieht der Absender als 413. |
| N13 | **Fremder Mandant bei Linear** ist nicht eigens per Integrationstest geprüft (GitHub, Slack, Jira schon); die Prüfung sitzt für alle Arten an derselben Stelle im Handler, `organizationId` wird per Unit-Test geparst. | Gleicher Codepfad; als Testlücke offen benannt. |
| N12 | Tests laufen mit Engine `none` und Mock-Provider; die Antwort erreicht den Agenten nachweislich (`SEEN.md`), echte CLIs sind nicht beteiligt. | Wie alle Pakete. |

## Geprüft und für gut befunden

- **Keine Eskalation:** Verwaltungsrouten sind nicht in der Token-Tabelle (Default-Deny aus WP07, bestehender Routen-Test grün); nur die Projekt-Eigentümerin verbindet; Fremde 404, Betrachter lesen ohne Secrets; Aufträge laufen ausschließlich als zugeordnete Person bzw. (Opt-in) als Eigentümerin der Anbindung mit erneuter Rollenprüfung bei jedem Ereignis.
- **Prompt-Injection:** Absendertext nie in Titel, Prompt oder Intro; Integrationsname und Hinweise stammen von der Eigentümerin; Linear-Identifier und Jira-Schlüssel werden vor der Verwendung im Titel validiert, sonst Standardbezeichnung.
- **Architektur:** keine Host-Rechte in der App, Runner unverändert, Wire-Typen/Texte in `queued-api`, nur neue Migration 0011, Rückkanal ausschließlich in der App über den SSRF-Client.
- **Alt-Verhalten:** Ein Bestandstest geändert (`api::question_is_answered_and_task_resumes`), weil er das unsichere Verhalten von B1 festschrieb; die neue Prüfung ist mindestens so streng (Nachricht zugestellt, Kontext ohne Rohtext, Auftrag fertig). Tour um den Reiter „Anbindungen“ erweitert, Stack-Skript startet `fake_intake`.
