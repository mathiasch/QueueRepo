import { useCallback, useEffect, useState } from "react";
import { api, ApiError, fmtTime, fmtTokens, type Meta, type User } from "./api";
import { StoreProvider, useStore } from "./store";
import { QueueView } from "./Queue";
import { ProjectsView } from "./Projects";
import { InboxView } from "./Inbox";
import { SettingsView } from "./SettingsView";
import { TaskDrawer } from "./TaskDrawer";

type Tab = "queue" | "projects" | "inbox" | "settings";

export default function App() {
  const [meta, setMeta] = useState<Meta | null>(null);
  const [me, setMe] = useState<User | null | undefined>(undefined);

  useEffect(() => {
    api.get<Meta>("/meta").then(setMeta);
    api.get<User>("/auth/me").then(setMe).catch(() => setMe(null));
  }, []);

  if (!meta || me === undefined) return <div className="center muted">Lade…</div>;
  if (!me) return <Login meta={meta} onLogin={() => api.get<User>("/auth/me").then(setMe)} />;
  return (
    <StoreProvider me={me} meta={meta}>
      <Shell />
    </StoreProvider>
  );
}

function Shell() {
  const { tasks, notifications, limits, toasts, dismissToast, setOpenTask, openTask } = useStore();
  const [tab, setTab] = useState<Tab>(() => (localStorage.getItem("tab") as Tab) || "queue");
  const closeTask = useCallback(() => setOpenTask(null), [setOpenTask]);
  useEffect(() => localStorage.setItem("tab", tab), [tab]);

  const attention = tasks.filter((t) => t.status === "waiting_input" || t.status === "pending_approval").length;
  const unread = notifications.filter((n) => !n.read_at).length;
  const running = tasks.filter((t) => t.status === "running").length;
  const queued = tasks.filter((t) => t.status === "queued").length;

  const nav: [Tab, string, number?][] = [
    ["queue", "Queue", running + queued || undefined],
    ["inbox", "Inbox", attention + unread || undefined],
    ["projects", "Projekte"],
    ["settings", "Einstellungen"],
  ];

  return (
    <div className="shell">
      <header className="topbar">
        <div className="brand">
          <span className="logo">▤</span> Queue
          {running > 0 && <span className="pulse" title={`${running} läuft`} />}
        </div>
        <nav className="tabs">
          {nav.map(([id, label, badge]) => (
            <button key={id} className={tab === id ? "tab active" : "tab"} onClick={() => setTab(id)}>
              {label}
              {badge ? <span className={id === "inbox" && attention ? "badge warn" : "badge"}>{badge}</span> : null}
            </button>
          ))}
        </nav>
        <div className="limits">
          {limits.map((l) => (
            <span key={l.provider} className={l.limited_until ? "chip warn" : "chip"} title={`5h-Fenster: ${l.runs} Runs, ${fmtTokens(l.input_tokens)} in / ${fmtTokens(l.output_tokens)} out, Cache ${fmtTokens(l.cache_read_tokens)}${l.cost_usd ? `, $${l.cost_usd.toFixed(2)}` : ""}`}>
              {l.provider} {l.limited_until ? `⏸ bis ${fmtTime(l.limited_until)}` : fmtTokens(l.input_tokens + l.output_tokens)}
            </span>
          ))}
        </div>
      </header>

      <main className="main">
        {tab === "queue" && <QueueView goProjects={() => setTab("projects")} />}
        {tab === "inbox" && <InboxView />}
        {tab === "projects" && <ProjectsView />}
        {tab === "settings" && <SettingsView />}
      </main>

      {openTask && <TaskDrawer id={openTask} onClose={closeTask} />}

      <div className="toasts">
        {toasts.map((t) => (
          <div key={t.id} className={`toast ${t.tone ?? ""}`} onClick={() => { if (t.taskId) setOpenTask(t.taskId); dismissToast(t.id); }}>
            <strong>{t.title}</strong>
            {t.body && <div className="muted pre clamp">{t.body}</div>}
          </div>
        ))}
      </div>
    </div>
  );
}

function Login({ meta, onLogin }: { meta: Meta; onLogin: () => void }) {
  const [mode, setMode] = useState<"login" | "register">("login");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [name, setName] = useState("");
  const [error, setError] = useState(() => new URLSearchParams(location.search).get("login_error") ?? "");
  const [busy, setBusy] = useState(false);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError("");
    try {
      await api.post(`/auth/${mode}`, mode === "login" ? { email, password } : { email, password, name });
      onLogin();
    } catch (err) {
      setError(err instanceof ApiError ? err.message : "Fehler");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="center">
      <form className="card login" onSubmit={submit}>
        <h1>
          <span className="logo">▤</span> Queue
        </h1>
        <p className="muted">Agentische Coding-Queue</p>
        {meta.oidc.length > 0 && (
          <div className="stack">
            {meta.oidc.map((p) => (
              <a key={p.id} className={`btn idp idp-${p.id}`} href={`/api/auth/oidc/${p.id}/start`}>
                Mit {p.name} anmelden
              </a>
            ))}
            <div className="divider">oder</div>
          </div>
        )}
        {mode === "register" && <input placeholder="Name" value={name} onChange={(e) => setName(e.target.value)} />}
        <input type="email" placeholder="E-Mail" autoComplete="email" value={email} onChange={(e) => setEmail(e.target.value)} required />
        <input type="password" placeholder="Passwort" autoComplete={mode === "login" ? "current-password" : "new-password"} value={password} onChange={(e) => setPassword(e.target.value)} required minLength={mode === "register" ? 8 : undefined} />
        {error && <div className="error">{error}</div>}
        <button className="primary" disabled={busy}>
          {mode === "login" ? "Anmelden" : "Konto erstellen"}
        </button>
        {meta.allow_registration && (
          <button type="button" className="ghost" onClick={() => setMode(mode === "login" ? "register" : "login")}>
            {mode === "login" ? "Neues Konto erstellen" : "Ich habe schon ein Konto"}
          </button>
        )}
      </form>
    </div>
  );
}
