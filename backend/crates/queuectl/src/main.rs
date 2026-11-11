use std::path::PathBuf;

use clap::{Parser, Subcommand};
use queuectl::{commands as c, exit, Api, EXIT_USAGE};

const AFTER: &str = "\
Konfiguration (Umgebung vor Datei):
  QUEUE_URL, QUEUE_TOKEN, QUEUE_PROJECT
  Datei: $QUEUECTL_CONFIG, sonst $XDG_CONFIG_HOME/queuectl/config bzw. ~/.config/queuectl/config
         Zeilen „url = https://…“, „token = qk_…“, „project = Name“; mit Token nur mit chmod 600.
  Das Token steht nie auf der Kommandozeile.

Exit-Codes: 0 ok · 1 Auftrag nicht erfolgreich bzw. Server lehnt ab · 2 Bedienung/Konfiguration · 3 Token";

#[derive(Parser)]
#[command(name = "queuectl", version, about = "Aufträge in Queue anlegen, verfolgen und steuern", after_help = AFTER)]
struct Cli {
    /// Konfigurationsdatei (statt des Standardpfads)
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Ausgabe als JSON
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Zeigt, wem das Token gehört und was es darf
    Whoami,
    /// Listet deine Projekte
    Projects,
    /// Legt einen Auftrag an (Text als Argumente oder über stdin mit „-“)
    Add {
        #[arg(short, long)]
        project: Option<String>,
        #[arg(short, long)]
        title: Option<String>,
        /// Vorlage (Name oder ID); weitere Wörter werden an ihre Beschreibung angehängt
        #[arg(long)]
        template: Option<String>,
        /// Erst planen, dann umsetzen
        #[arg(long, conflicts_with = "no_plan")]
        plan: bool,
        #[arg(long)]
        no_plan: bool,
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        model: Option<String>,
        /// Kostenobergrenze in USD
        #[arg(long)]
        max_cost: Option<f64>,
        /// Danach verfolgen wie „watch“
        #[arg(short, long)]
        watch: bool,
        text: Vec<String>,
    },
    /// Listet Aufträge
    List {
        #[arg(short, long)]
        project: Option<String>,
        /// z. B. running, queued, done, failed
        #[arg(short, long)]
        status: Option<String>,
    },
    /// Zeigt einen Auftrag (ID oder eindeutiger Anfang)
    Status { id: String },
    /// Verfolgt einen Auftrag bis zum Ende (Live-Ereignisse, sonst Abfrage)
    Watch {
        id: String,
        /// Abfragen statt Live-Verbindung
        #[arg(long)]
        poll: bool,
        /// Sekunden zwischen Abfragen
        #[arg(long, default_value_t = 3)]
        interval: u64,
        /// Höchstens so viele Sekunden warten
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Bricht einen Auftrag ab
    Cancel { id: String },
    /// Schickt dem Agenten eine Nachricht (zurückziehbar bis zur Zustellung)
    Steer {
        id: String,
        /// Laufenden Agenten unterbrechen und sofort zustellen
        #[arg(long)]
        now: bool,
        text: Vec<String>,
    },
    /// „Weiter mit …“: neuer Auftrag, der dort weitermacht
    Continue {
        id: String,
        #[arg(long)]
        plan: bool,
        #[arg(short, long)]
        watch: bool,
        text: Vec<String>,
    },
    /// Plan genehmigen, überarbeiten lassen oder verwerfen
    Plan {
        id: String,
        #[command(subcommand)]
        action: PlanCmd,
    },
    /// Startet einen eigenen Auslöser (optional mit Text als Information)
    Fire { trigger: String, text: Vec<String> },
}

#[derive(Subcommand)]
enum PlanCmd {
    Approve,
    Revise { text: Vec<String> },
    Reject,
}

async fn run(cli: Cli) -> queuectl::Result<i32> {
    let env = |k: &str| std::env::var(k).ok();
    let cfg = queuectl::load(cli.config.as_deref(), &env)?;
    let api = Api::new(&cfg)?;
    let json = cli.json;
    match cli.cmd {
        Cmd::Whoami => c::whoami(&api, json).await,
        Cmd::Projects => c::projects(&api, json).await,
        Cmd::Add {
            project,
            title,
            template,
            plan,
            no_plan,
            provider,
            model,
            max_cost,
            watch,
            text,
        } => {
            let t = c::add(
                &api,
                c::AddArgs {
                    project: project.or(cfg.project.clone()),
                    title,
                    template,
                    plan: if plan {
                        Some(true)
                    } else if no_plan {
                        Some(false)
                    } else {
                        None
                    },
                    provider,
                    model,
                    max_cost,
                    text,
                    json,
                },
            )
            .await?;
            if watch {
                c::watch(&api, t.id, false, 3, None).await
            } else {
                Ok(0)
            }
        }
        Cmd::List { project, status } => {
            c::list(
                &api,
                project.or(cfg.project.clone()).as_deref(),
                status.as_deref(),
                json,
            )
            .await
        }
        Cmd::Status { id } => c::status(&api, c::resolve_task(&api, &id).await?, json).await,
        Cmd::Watch {
            id,
            poll,
            interval,
            timeout,
        } => {
            c::watch(
                &api,
                c::resolve_task(&api, &id).await?,
                poll,
                interval,
                timeout,
            )
            .await
        }
        Cmd::Cancel { id } => c::cancel(&api, c::resolve_task(&api, &id).await?).await,
        Cmd::Steer { id, now, text } => {
            let id = c::resolve_task(&api, &id).await?;
            c::steer(&api, id, c::text_arg(&text, "Nachricht")?, now).await
        }
        Cmd::Continue {
            id,
            plan,
            watch,
            text,
        } => {
            let id = c::resolve_task(&api, &id).await?;
            let t = c::continue_task(
                &api,
                id,
                c::text_arg(&text, "Nachricht")?,
                plan.then_some(true),
            )
            .await?;
            if watch {
                c::watch(&api, t.id, false, 3, None).await
            } else {
                Ok(0)
            }
        }
        Cmd::Plan { id, action } => {
            let id = c::resolve_task(&api, &id).await?;
            let a = match action {
                PlanCmd::Approve => c::PlanAction::Approve,
                PlanCmd::Revise { text } => {
                    c::PlanAction::Revise(c::text_arg(&text, "Änderungswunsch")?)
                }
                PlanCmd::Reject => c::PlanAction::Reject,
            };
            c::plan(&api, id, a).await
        }
        Cmd::Fire { trigger, text } => {
            let id = uuid::Uuid::parse_str(trigger.trim())
                .map_err(|_| exit(EXIT_USAGE, "Auslöser-ID muss eine vollständige UUID sein"))?;
            let text = if text.is_empty() {
                None
            } else {
                Some(c::text_arg(&text, "Text")?)
            };
            c::fire(&api, id, text, json).await
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let cli = Cli::parse();
    let code = match run(cli).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("queuectl: {e}");
            e.code
        }
    };
    std::process::exit(code);
}
