import { useEffect, useRef, useState } from "react";
import { api, ApiError, duration, FAILURE_LABEL, fmtTime, fmtTokens, STATUS_LABEL, taskLimits, type Task, type TaskEvent } from "./api";
import { useStore } from "./store";
import { StatusDot } from "./Queue";

export function TaskDrawer({ id, onClose }: { id: string; onClose: () => void }) {
  const { tasks, onTaskEvent, toast, setOpenTask } = useStore();
  const [data, setData] = useState<{ task: Task; events: TaskEvent[] } | null>(null);
  const [diff, setDiff] = useState<string | null>(null);
  const [edit, setEdit] = useState<{ title: string; prompt: string; context: string } | null>(null);
  const logRef = useRef<HTMLDivElement>(null);
  const live = tasks.find((t) => t.id === id);
  const task = live ?? data?.task;

  useEffect(() => {
    setData(null);
    setDiff(null);
    setEdit(null);
    api.get<{ task: Task; events: TaskEvent[] }>(`/tasks/${id}`).then(setData).catch(() => onClose());
    return onTaskEvent(id, (e) => setData((d) => (d ? { ...d, events: [...d.events, e] } : d)));
  }, [id, onTaskEvent, onClose]);

  useEffect(() => {
    const el = logRef.current;
    if (el && el.scrollHeight - el.scrollTop - el.clientHeight < 120) el.scrollTop = el.scrollHeight;
  }, [data?.events.length]);

  useEffect(() => {
    const k = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", k);
    return () => window.removeEventListener("keydown", k);
  }, [onClose]);

  if (!task) return null;

  const run = async (fn: () => Promise<unknown>, okMsg?: string) => {
    try {
      await fn();
      if (okMsg) toast({ title: okMsg });
    } catch (e) {
      toast({ title: "Fehler", body: e instanceof ApiError ? e.message : String(e), tone: "error" });
    }
  };
  const canEdit = ["queued", "pending_approval", "waiting_input"].includes(task.status);
  const hasSnap = !!(task.snapshot_before && task.snapshot_after && task.snapshot_before !== task.snapshot_after);
  const u = task.usage;
  const parent = task.parent_id ? tasks.find((t) => t.id === task.parent_id) : undefined;
  const children = tasks.filter((t) => t.parent_id === task.id);

  return (
    <div className="drawer-backdrop" onClick={onClose}>
      <aside className="drawer" onClick={(e) => e.stopPropagation()}>
        <div className="drawer-head">
          <StatusDot s={task.status} />
          <div className="grow min0">
            <h2>{task.title}</h2>
            <div className="muted small">
              {STATUS_LABEL[task.status]} · {task.provider}
              {task.model ? `/${task.model}` : ""} · {task.kind}
              {task.attempt > 0 && ` · Versuch ${task.attempt}`}
              {task.started_at && ` · ${duration(task.started_at, task.finished_at)}`}
            </div>
          </div>
          <button className="icon" onClick={onClose}>
            ✕
          </button>
        </div>

        <div className="row wrap drawer-actions">
          {canEdit && !edit && (
            <button className="small" onClick={() => setEdit({ title: task.title, prompt: task.prompt, context: task.context })}>
              Bearbeiten
            </button>
          )}
          {task.status === "pending_approval" && (
            <button className="small primary" onClick={() => run(() => api.post(`/tasks/${id}/approve`))}>
              Freigeben
            </button>
          )}
          {["queued", "pending_approval", "running", "waiting_input"].includes(task.status) && (
            <button className="small danger" onClick={() => run(() => api.post(`/tasks/${id}/cancel`))}>
              {task.status === "running" ? "Abbrechen" : "Verwerfen"}
            </button>
          )}
          {["failed", "blocked", "cancelled"].includes(task.status) && (
            <button className="small" onClick={() => run(() => api.post(`/tasks/${id}/retry`), "Erneut eingereiht")}>
              Erneut versuchen
            </button>
          )}
          {hasSnap && (
            <button className="small" onClick={() => run(async () => setDiff(diff === null ? (await api.get<{ diff: string }>(`/tasks/${id}/diff`)).diff : null))}>
              {diff === null ? "Änderungen anzeigen" : "Diff ausblenden"}
            </button>
          )}
          {hasSnap && task.status !== "running" && (
            <button
              className="small danger"
              onClick={() => confirm("Änderungen dieses Auftrags im Workspace rückgängig machen?") && run(() => api.post(`/tasks/${id}/revert`), "Änderungen zurückgesetzt")}
            >
              Rückgängig
            </button>
          )}
        </div>

        <div className="drawer-body">
          {edit ? (
            <div className="stack">
              <input value={edit.title} onChange={(e) => setEdit({ ...edit, title: e.target.value })} />
              <textarea rows={8} value={edit.prompt} onChange={(e) => setEdit({ ...edit, prompt: e.target.value })} />
              <textarea rows={4} placeholder="Kontext" value={edit.context} onChange={(e) => setEdit({ ...edit, context: e.target.value })} />
              <div className="row">
                <button className="primary small" onClick={() => run(async () => { await api.patch(`/tasks/${id}`, edit); setEdit(null); })}>
                  Speichern
                </button>
                <button className="ghost small" onClick={() => setEdit(null)}>
                  Abbrechen
                </button>
              </div>
            </div>
          ) : (
            <>
              {task.failure_class && task.failure_class !== "task" && task.failure_class !== "cancelled" && task.status !== "done" && (
                <div data-testid="abort-reason">
                  <Block
                    title="Abbruchgrund"
                    tone={task.status === "blocked" ? "error" : undefined}
                    text={[
                      FAILURE_LABEL[task.failure_class],
                      task.transient_retries > 0 ? `Wiederholungen: ${task.transient_retries}` : "",
                      task.not_before && task.status === "queued" ? `Nächster Versuch: ${fmtTime(task.not_before)}` : "",
                      "Kein Fix-Versuch verbraucht.",
                    ]
                      .filter(Boolean)
                      .join("\n")}
                  />
                </div>
              )}
              {taskLimits(task).length > 0 && <Block title="Limits dieses Auftrags" text={taskLimits(task).join(" · ")} />}
              {task.error && <Block title="Fehler" tone="error" text={task.error} />}
              {task.questions && task.questions.length > 0 && <Block title="Rückfragen" text={task.questions.map((q) => `• ${q}`).join("\n")} />}
              {task.summary && <Block title="Zusammenfassung" text={task.summary} />}
              {task.review && (
                <Block title={task.review.approved ? "Review ✓" : "Review ✗"} tone={task.review.approved ? undefined : "error"} text={task.review.issues.length ? task.review.issues.map((i) => `• ${i}`).join("\n") : "Keine Beanstandungen"} />
              )}
              {task.handoff && <Block title="Handoff für Folgeaufträge" text={task.handoff} collapsible />}
              <Block title="Auftrag" text={task.prompt || "(automatisch generiert)"} collapsible={task.status !== "queued"} />
              {task.context && <Block title="Kontext" text={task.context} collapsible />}
              {(parent || children.length > 0) && (
                <div className="block">
                  <h4>Kette</h4>
                  {parent && (
                    <a className="link" onClick={() => setOpenTask(parent.id)}>
                      ← {parent.title}
                    </a>
                  )}
                  {children.map((c) => (
                    <a key={c.id} className="link" onClick={() => setOpenTask(c.id)}>
                      → {c.title} <span className="muted">({STATUS_LABEL[c.status]})</span>
                    </a>
                  ))}
                </div>
              )}
              {u && (
                <div className="block usage muted small">
                  Tokens: {fmtTokens(u.input_tokens)} in · {fmtTokens(u.output_tokens)} out · Cache {fmtTokens(u.cache_read_tokens)}
                  {u.cost_usd > 0 && ` · $${u.cost_usd.toFixed(3)}`}
                </div>
              )}
            </>
          )}

          {diff !== null && <DiffView diff={diff} />}

          <div className="block">
            <h4>Verlauf {task.status === "running" && <span className="pulse" />}</h4>
            <div className="log" ref={logRef}>
              {(data?.events ?? []).map((e) => (
                <div key={e.id} className={`ev ev-${e.kind}`}>
                  <span className="ts">{fmtTime(e.ts)}</span>
                  <span className="kind">{e.kind}</span>
                  <span className="content">{e.content}</span>
                </div>
              ))}
              {data && data.events.length === 0 && <div className="muted">Noch keine Ereignisse.</div>}
            </div>
          </div>
        </div>
      </aside>
    </div>
  );
}

function Block({ title, text, tone, collapsible }: { title: string; text: string; tone?: "error"; collapsible?: boolean }) {
  const [open, setOpen] = useState(!collapsible || text.length < 400);
  return (
    <div className={`block ${tone ?? ""}`}>
      <h4 onClick={() => collapsible && setOpen(!open)} className={collapsible ? "clickable" : ""}>
        {title} {collapsible && <span className="muted">{open ? "▾" : "▸"}</span>}
      </h4>
      {open && <div className="pre">{text}</div>}
    </div>
  );
}

function DiffView({ diff }: { diff: string }) {
  if (!diff.trim()) return <div className="block muted">Keine Änderungen.</div>;
  return (
    <div className="block">
      <h4>Änderungen</h4>
      <pre className="diff">
        {diff.split("\n").map((l, i) => (
          <div key={i} className={l.startsWith("+++") || l.startsWith("---") ? "d-file" : l.startsWith("+") ? "d-add" : l.startsWith("-") ? "d-del" : l.startsWith("@@") ? "d-hunk" : l.startsWith("diff ") ? "d-file" : ""}>
            {l || " "}
          </div>
        ))}
      </pre>
    </div>
  );
}
