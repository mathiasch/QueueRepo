import { useEffect, useMemo, useRef, useState } from "react";
import { api, ApiError, duration, fmtTime, STATUS_LABEL, type Task } from "./api";
import { useStore } from "./store";

export function QueueView({ goProjects }: { goProjects: () => void }) {
  const { tasks, projects } = useStore();
  const [historyFilter, setHistoryFilter] = useState<"all" | "done" | "problems">("all");

  const by = (s: Task["status"][]) => tasks.filter((t) => s.includes(t.status));
  const running = by(["running"]);
  const attention = by(["waiting_input", "pending_approval"]);
  const queued = by(["queued"]).sort((a, b) => a.position - b.position);
  const history = by(["done", "failed", "blocked", "cancelled"])
    .filter((t) => historyFilter === "all" || (historyFilter === "done" ? t.status === "done" : t.status !== "done"))
    .sort((a, b) => (b.finished_at ?? b.updated_at).localeCompare(a.finished_at ?? a.updated_at));

  if (projects.length === 0)
    return (
      <div className="empty card">
        <h2>Willkommen 👋</h2>
        <p>Lege zuerst ein Projekt an. Jedes Projekt hat einen eigenen Workspace, in dem die Agenten arbeiten.</p>
        <button className="primary" onClick={goProjects}>
          Projekt anlegen
        </button>
      </div>
    );

  return (
    <div className="queue">
      <Composer />
      {running.length > 0 && (
        <Section title="Läuft">
          {running.map((t) => <TaskRow key={t.id} t={t} />)}
        </Section>
      )}
      {attention.length > 0 && <Attention tasks={attention} />}
      <Section title={`Warteschlange (${queued.length})`}>
        {queued.length === 0 ? <div className="muted pad">Keine wartenden Aufträge. Ist die Queue leer, starten die OnEmptyQueue-Aktionen.</div> : queued.map((t, i) => <TaskRow key={t.id} t={t} index={i} count={queued.length} />)}
      </Section>
      <Section
        title="Verlauf"
        right={
          <div className="seg">
            {(["all", "done", "problems"] as const).map((f) => (
              <button key={f} className={historyFilter === f ? "active" : ""} onClick={() => setHistoryFilter(f)}>
                {{ all: "Alle", done: "Erledigt", problems: "Probleme" }[f]}
              </button>
            ))}
          </div>
        }
      >
        {history.length === 0 ? <div className="muted pad">Noch nichts erledigt.</div> : history.slice(0, 100).map((t) => <TaskRow key={t.id} t={t} />)}
      </Section>
    </div>
  );
}

function Section({ title, right, children }: { title: string; right?: React.ReactNode; children: React.ReactNode }) {
  return (
    <section className="section">
      <div className="section-head">
        <h3>{title}</h3>
        {right}
      </div>
      <div className="list">{children}</div>
    </section>
  );
}

function Composer() {
  const { projects, settings, meta, tasks, toast } = useStore();
  const [projectId, setProjectId] = useState(() => localStorage.getItem("project") ?? "");
  const [prompt, setPrompt] = useState(() => localStorage.getItem("draft") ?? "");
  const [context, setContext] = useState("");
  const [provider, setProvider] = useState("");
  const [model, setModel] = useState("");
  const [parentId, setParentId] = useState("");
  const [front, setFront] = useState(false);
  const [showOpts, setShowOpts] = useState(false);
  const [busy, setBusy] = useState(false);
  const ref = useRef<HTMLTextAreaElement>(null);

  const editable = projects.filter((p) => p.role !== "viewer");
  const pid = editable.some((p) => p.id === projectId) ? projectId : editable[0]?.id ?? "";
  useEffect(() => localStorage.setItem("draft", prompt), [prompt]);

  const parents = useMemo(
    () => tasks.filter((t) => t.project_id === pid && t.status === "done").sort((a, b) => (b.finished_at ?? "").localeCompare(a.finished_at ?? "")).slice(0, 20),
    [tasks, pid],
  );

  const submit = async () => {
    if (!prompt.trim() || !pid) return;
    setBusy(true);
    try {
      await api.post("/tasks", { project_id: pid, prompt, context, provider: provider || undefined, model: model || undefined, parent_id: parentId || undefined, front });
      setPrompt("");
      setContext("");
      setParentId("");
      setFront(false);
      ref.current?.focus();
    } catch (e) {
      toast({ title: "Konnte Auftrag nicht anlegen", body: e instanceof ApiError ? e.message : String(e), tone: "error" });
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="card composer">
      <textarea
        ref={ref}
        autoFocus
        rows={3}
        placeholder="Was soll erledigt werden? (⌘/Ctrl + Enter zum Einreihen)"
        value={prompt}
        onChange={(e) => setPrompt(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
            e.preventDefault();
            submit();
          }
        }}
      />
      {showOpts && (
        <div className="opts">
          <label>
            Kontext für den Agenten (Anforderungen, Links, Hinweise)
            <textarea rows={3} value={context} onChange={(e) => setContext(e.target.value)} placeholder="Optional – wird dem Agenten mitgegeben" />
          </label>
          <div className="row wrap">
            <label>
              Fortsetzung von (Handoff)
              <select value={parentId} onChange={(e) => setParentId(e.target.value)}>
                <option value="">— keiner —</option>
                {parents.map((t) => (
                  <option key={t.id} value={t.id}>
                    {t.title.slice(0, 60)}
                  </option>
                ))}
              </select>
            </label>
            <label>
              Provider
              <select value={provider} onChange={(e) => setProvider(e.target.value)}>
                <option value="">Standard ({settings?.default_provider})</option>
                {meta.providers.map((p) => (
                  <option key={p}>{p}</option>
                ))}
              </select>
            </label>
            <label>
              Modell
              <input value={model} onChange={(e) => setModel(e.target.value)} placeholder="Standard" />
            </label>
          </div>
        </div>
      )}
      <div className="row wrap composer-bar">
        <select
          value={pid}
          onChange={(e) => {
            setProjectId(e.target.value);
            localStorage.setItem("project", e.target.value);
          }}
        >
          {editable.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </select>
        <label className="check">
          <input type="checkbox" checked={front} onChange={(e) => setFront(e.target.checked)} /> als Nächstes
        </label>
        <button className="ghost small" onClick={() => setShowOpts(!showOpts)}>
          {showOpts ? "Weniger" : "Optionen"}
        </button>
        <span className="grow" />
        <button className="primary" disabled={busy || !prompt.trim() || !pid} onClick={submit}>
          Einreihen
        </button>
      </div>
    </div>
  );
}

function Attention({ tasks }: { tasks: Task[] }) {
  const { toast } = useStore();
  const pending = tasks.filter((t) => t.status === "pending_approval");
  return (
    <Section
      title="Braucht dich"
      right={
        pending.length > 1 ? (
          <button className="small" onClick={() => api.post("/tasks/approve-all").catch((e) => toast({ title: String(e), tone: "error" }))}>
            Alle {pending.length} freigeben
          </button>
        ) : null
      }
    >
      {tasks.map((t) => (t.status === "waiting_input" ? <Question key={t.id} t={t} /> : <TaskRow key={t.id} t={t} />))}
    </Section>
  );
}

function Question({ t }: { t: Task }) {
  const { setOpenTask } = useStore();
  const [answer, setAnswer] = useState("");
  return (
    <div className="item attention">
      <div className="item-main" onClick={() => setOpenTask(t.id)}>
        <StatusDot s={t.status} />
        <div className="grow">
          <div className="title">{t.title}</div>
          <ul className="questions">
            {(t.questions ?? []).map((q, i) => (
              <li key={i}>{q}</li>
            ))}
          </ul>
        </div>
      </div>
      <div className="row">
        <input
          className="grow"
          placeholder="Antwort…"
          value={answer}
          onChange={(e) => setAnswer(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && answer.trim() && api.post(`/tasks/${t.id}/answer`, { answer })}
        />
        <button className="primary small" disabled={!answer.trim()} onClick={() => api.post(`/tasks/${t.id}/answer`, { answer })}>
          Antworten
        </button>
      </div>
    </div>
  );
}

export function StatusDot({ s }: { s: Task["status"] }) {
  return <span className={`dot ${s}`} title={STATUS_LABEL[s]} />;
}

function TaskRow({ t, index, count }: { t: Task; index?: number; count?: number }) {
  const { setOpenTask, projects, lastEvent } = useStore();
  const project = projects.find((p) => p.id === t.project_id);
  const ev = lastEvent[t.id];
  const act = (path: string, body?: unknown) => (e: React.MouseEvent) => {
    e.stopPropagation();
    api.post(`/tasks/${t.id}/${path}`, body);
  };

  return (
    <div className={`item ${t.status}`} onClick={() => setOpenTask(t.id)}>
      <div className="item-main">
        <StatusDot s={t.status} />
        <div className="grow min0">
          <div className="title">
            {t.kind === "fix" && <span className="tag fix">Fix</span>}
            {t.kind === "onempty" && <span className="tag auto">Auto</span>}
            {t.created_by_agent && t.kind === "task" && <span className="tag agent">Agent</span>}
            {t.title}
          </div>
          <div className="meta muted">
            {project?.name} · {t.provider}
            {t.model ? `/${t.model}` : ""}
            {t.status === "running" && ` · ${duration(t.started_at, null)}`}
            {t.finished_at && ` · ${fmtTime(t.finished_at)}`}
            {t.status === "running" && ev && <span className="live"> · {ev.content.split("\n")[0].slice(0, 120)}</span>}
            {(t.status === "done" || t.status === "failed" || t.status === "blocked") && t.summary && <span> · {t.summary.split("\n")[0].slice(0, 120)}</span>}
          </div>
        </div>
        <div className="actions" onClick={(e) => e.stopPropagation()}>
          {t.status === "queued" && count !== undefined && index !== undefined && (
            <>
              <button className="icon" disabled={index === 0} onClick={act("move", { direction: "top" })} title="Ganz nach oben">
                ⤒
              </button>
              <button className="icon" disabled={index === 0} onClick={act("move", { direction: "up" })} title="Hoch">
                ↑
              </button>
              <button className="icon" disabled={index === count - 1} onClick={act("move", { direction: "down" })} title="Runter">
                ↓
              </button>
            </>
          )}
          {t.status === "pending_approval" && (
            <button className="small primary" onClick={act("approve")}>
              Freigeben
            </button>
          )}
          {["queued", "pending_approval", "running"].includes(t.status) && (
            <button className="icon" onClick={act("cancel")} title={t.status === "running" ? "Abbrechen" : "Verwerfen"}>
              ✕
            </button>
          )}
          {["failed", "blocked", "cancelled"].includes(t.status) && (
            <button className="icon" onClick={act("retry")} title="Erneut einreihen">
              ↻
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
