import { useEffect, useMemo, useState } from "react";
import { api, ApiError, fmtTime, STATUS_LABEL, type Project, type Task } from "./api";
import { useStore } from "./store";

export function ProjectsView() {
  const { projects, reloadProjects, toast } = useStore();
  const [selected, setSelected] = useState<string | null>(null);
  const [name, setName] = useState("");
  const [goal, setGoal] = useState("");

  const create = async (e: React.FormEvent) => {
    e.preventDefault();
    try {
      const p = await api.post<Project>("/projects", { name, goal });
      setName("");
      setGoal("");
      await reloadProjects();
      setSelected(p.id);
    } catch (err) {
      toast({ title: "Fehler", body: err instanceof ApiError ? err.message : String(err), tone: "error" });
    }
  };

  if (selected && projects.some((p) => p.id === selected)) return <ProjectDetail id={selected} onBack={() => setSelected(null)} />;

  return (
    <div className="projects">
      <form className="card stack" onSubmit={create}>
        <h3>Neues Projekt</h3>
        <input placeholder="Name" value={name} onChange={(e) => setName(e.target.value)} required />
        <textarea rows={2} placeholder="Ziel des Projekts (steuert u. a. die OnEmptyQueue-Vorschläge)" value={goal} onChange={(e) => setGoal(e.target.value)} />
        <div className="row">
          <button className="primary">Anlegen</button>
        </div>
      </form>
      <div className="grid">
        {projects.map((p) => (
          <div key={p.id} className="card project-card clickable" onClick={() => setSelected(p.id)}>
            <div className="row">
              <strong className="grow">{p.name}</strong>
              <span className="tag">{p.role}</span>
            </div>
            <div className="muted small">{p.description || `von ${p.owner_name}`}</div>
          </div>
        ))}
      </div>
    </div>
  );
}

type Tab = "settings" | "files" | "members" | "tasks";

function ProjectDetail({ id, onBack }: { id: string; onBack: () => void }) {
  const { reloadProjects, toast } = useStore();
  const [p, setP] = useState<Project | null>(null);
  const [tab, setTab] = useState<Tab>("files");

  const load = () => api.get<Project>(`/projects/${id}`).then(setP);
  useEffect(() => {
    load();
  }, [id]); // eslint-disable-line react-hooks/exhaustive-deps

  if (!p) return <div className="muted">Lade…</div>;
  const canEdit = p.role !== "viewer";

  const remove = async () => {
    if (!confirm(`Projekt "${p.name}" inkl. Workspace und Aufträgen endgültig löschen?`)) return;
    try {
      await api.del(`/projects/${id}`);
      await reloadProjects();
      onBack();
    } catch (e) {
      toast({ title: "Fehler", body: e instanceof ApiError ? e.message : String(e), tone: "error" });
    }
  };

  return (
    <div className="project-detail">
      <div className="row">
        <button className="ghost small" onClick={onBack}>
          ← Projekte
        </button>
        <h2 className="grow">{p.name}</h2>
        <a className="btn small" href={`/api/projects/${id}/zip`}>
          ZIP herunterladen
        </a>
      </div>
      <div className="seg">
        {(["files", "tasks", "settings", "members"] as Tab[]).map((t) => (
          <button key={t} className={tab === t ? "active" : ""} onClick={() => setTab(t)}>
            {{ files: "Dateien", tasks: "Aufträge", settings: "Einstellungen", members: "Mitglieder" }[t]}
          </button>
        ))}
      </div>
      {tab === "files" && <Files id={id} canEdit={canEdit} />}
      {tab === "tasks" && <ProjectTasks id={id} />}
      {tab === "settings" && <ProjectSettings p={p} canEdit={canEdit} onSaved={load} onDelete={p.role === "owner" ? remove : undefined} />}
      {tab === "members" && <Members id={id} isOwner={p.role === "owner"} />}
    </div>
  );
}

function ProjectSettings({ p, canEdit, onSaved, onDelete }: { p: Project; canEdit: boolean; onSaved: () => void; onDelete?: () => void }) {
  const { meta, toast, reloadProjects } = useStore();
  const [f, setF] = useState({
    name: p.name,
    description: p.description,
    goal: p.goal,
    verify_command: p.verify_command ?? "",
    review_enabled: p.review_enabled,
    review_provider: p.review_provider ?? "",
    review_model: p.review_model ?? "",
  });
  const save = async () => {
    try {
      await api.patch(`/projects/${p.id}`, f);
      toast({ title: "Gespeichert" });
      onSaved();
      reloadProjects();
    } catch (e) {
      toast({ title: "Fehler", body: e instanceof ApiError ? e.message : String(e), tone: "error" });
    }
  };
  return (
    <div className="card stack">
      <label>
        Name
        <input disabled={!canEdit} value={f.name} onChange={(e) => setF({ ...f, name: e.target.value })} />
      </label>
      <label>
        Beschreibung
        <input disabled={!canEdit} value={f.description} onChange={(e) => setF({ ...f, description: e.target.value })} />
      </label>
      <label>
        Projektziel <span className="muted">– wird jedem Agenten mitgegeben und steuert OnEmptyQueue</span>
        <textarea disabled={!canEdit} rows={3} value={f.goal} onChange={(e) => setF({ ...f, goal: e.target.value })} />
      </label>
      <h4>Definition of Done</h4>
      <label>
        Verifikationsbefehl <span className="muted">– läuft nach jedem Auftrag in der Sandbox, muss Exit 0 liefern</span>
        <input disabled={!canEdit} placeholder="z. B. npm ci && npm test  ·  cargo test  ·  pytest -q" value={f.verify_command} onChange={(e) => setF({ ...f, verify_command: e.target.value })} />
      </label>
      <label className="check">
        <input type="checkbox" disabled={!canEdit} checked={f.review_enabled} onChange={(e) => setF({ ...f, review_enabled: e.target.checked })} />
        Review durch zweiten Agenten (frischer Kontext, sieht nur Auftrag + Diff)
      </label>
      {f.review_enabled && (
        <div className="row wrap">
          <label>
            Review-Provider
            <select disabled={!canEdit} value={f.review_provider} onChange={(e) => setF({ ...f, review_provider: e.target.value })}>
              <option value="">wie Auftrag</option>
              {meta.providers.map((x) => (
                <option key={x}>{x}</option>
              ))}
            </select>
          </label>
          <label>
            Review-Modell <span className="muted">(z. B. günstigeres Modell)</span>
            <input disabled={!canEdit} value={f.review_model} onChange={(e) => setF({ ...f, review_model: e.target.value })} placeholder="Standard" />
          </label>
        </div>
      )}
      <div className="row">
        {canEdit && (
          <button className="primary" onClick={save}>
            Speichern
          </button>
        )}
        <span className="grow" />
        {onDelete && (
          <button className="danger" onClick={onDelete}>
            Projekt löschen
          </button>
        )}
      </div>
    </div>
  );
}

function Files({ id, canEdit }: { id: string; canEdit: boolean }) {
  const { toast } = useStore();
  const [files, setFiles] = useState<string[]>([]);
  const [filter, setFilter] = useState("");
  const [open, setOpen] = useState<{ path: string; content: string } | null>(null);
  const [busy, setBusy] = useState(false);

  const load = () => api.get<string[]>(`/projects/${id}/files`).then(setFiles);
  useEffect(() => {
    load();
  }, [id]); // eslint-disable-line react-hooks/exhaustive-deps

  const shown = useMemo(() => files.filter((f) => f.toLowerCase().includes(filter.toLowerCase())).slice(0, 500), [files, filter]);

  const upload = async (file: File) => {
    setBusy(true);
    try {
      const r = await api.upload<{ files: number }>(`/projects/${id}/import`, file);
      toast({ title: `${r.files} Dateien importiert` });
      load();
    } catch (e) {
      toast({ title: "Import fehlgeschlagen", body: e instanceof ApiError ? e.message : String(e), tone: "error" });
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="files">
      <div className="row wrap">
        <input className="grow" placeholder={`${files.length} Dateien filtern…`} value={filter} onChange={(e) => setFilter(e.target.value)} />
        {canEdit && (
          <label className="btn small">
            {busy ? "Importiere…" : "ZIP importieren"}
            <input type="file" accept=".zip" hidden onChange={(e) => e.target.files?.[0] && upload(e.target.files[0])} />
          </label>
        )}
      </div>
      <div className="files-split">
        <div className="file-list card">
          {shown.length === 0 && <div className="muted pad">Workspace ist leer. Importiere ein ZIP oder lass einen Agenten loslegen.</div>}
          {shown.map((f) => (
            <div key={f} className={open?.path === f ? "file active" : "file"} onClick={() => api.get<{ path: string; content: string }>(`/projects/${id}/file?path=${encodeURIComponent(f)}`).then(setOpen).catch((e) => toast({ title: String(e.message), tone: "error" }))}>
              {f}
            </div>
          ))}
        </div>
        {open && (
          <div className="file-view card">
            <div className="row">
              <strong className="grow mono small">{open.path}</strong>
              <button className="icon" onClick={() => setOpen(null)}>
                ✕
              </button>
            </div>
            <pre className="code">{open.content}</pre>
          </div>
        )}
      </div>
    </div>
  );
}

function ProjectTasks({ id }: { id: string }) {
  const { setOpenTask } = useStore();
  const [tasks, setTasks] = useState<Task[]>([]);
  useEffect(() => {
    api.get<Task[]>(`/projects/${id}/tasks`).then(setTasks);
  }, [id]);
  return (
    <div className="list">
      {tasks.length === 0 && <div className="muted pad">Noch keine Aufträge.</div>}
      {tasks.map((t) => (
        <div key={t.id} className="item" onClick={() => setOpenTask(t.id)}>
          <div className="item-main">
            <span className={`dot ${t.status}`} />
            <div className="grow min0">
              <div className="title">{t.title}</div>
              <div className="meta muted">
                {STATUS_LABEL[t.status]} · {t.provider} · {fmtTime(t.created_at)}
              </div>
            </div>
          </div>
        </div>
      ))}
    </div>
  );
}

function Members({ id, isOwner }: { id: string; isOwner: boolean }) {
  const { toast, me } = useStore();
  const [members, setMembers] = useState<{ user_id: string; email: string; name: string; role: string }[]>([]);
  const [email, setEmail] = useState("");
  const [role, setRole] = useState("editor");
  const load = () => api.get<typeof members>(`/projects/${id}/members`).then(setMembers);
  useEffect(() => {
    load();
  }, [id]); // eslint-disable-line react-hooks/exhaustive-deps

  const add = async (e: React.FormEvent) => {
    e.preventDefault();
    try {
      await api.post(`/projects/${id}/members`, { email, role });
      setEmail("");
      load();
    } catch (err) {
      toast({ title: "Fehler", body: err instanceof ApiError ? err.message : String(err), tone: "error" });
    }
  };

  return (
    <div className="card stack">
      <p className="muted small">Jeder User bleibt ein eigener Tenant mit eigener Queue und eigenem Abo – geteilt wird nur der Workspace. Pro Projekt läuft immer nur ein Auftrag gleichzeitig.</p>
      {members.map((m) => (
        <div key={m.user_id} className="row">
          <div className="grow">
            {m.name} <span className="muted small">{m.email}</span>
          </div>
          <span className="tag">{m.role}</span>
          {m.role !== "owner" && (isOwner || m.user_id === me.id) && (
            <button className="icon" onClick={() => api.del(`/projects/${id}/members/${m.user_id}`).then(load)}>
              ✕
            </button>
          )}
        </div>
      ))}
      {isOwner && (
        <form className="row wrap" onSubmit={add}>
          <input className="grow" type="email" placeholder="E-Mail eines registrierten Users" value={email} onChange={(e) => setEmail(e.target.value)} required />
          <select value={role} onChange={(e) => setRole(e.target.value)}>
            <option value="editor">Editor</option>
            <option value="viewer">Viewer</option>
          </select>
          <button className="primary small">Hinzufügen</button>
        </form>
      )}
    </div>
  );
}
