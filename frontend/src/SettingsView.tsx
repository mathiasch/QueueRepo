import { useEffect, useState } from "react";
import { api, ApiError, b64ToBytes, fmtTime, fmtTokens, providerLabel, type Limit, type Settings } from "./api";
import { useStore } from "./store";

const ACTIONS: [string, string, string][] = [
  ["review", "Review", "Code auf Bugs, Sicherheitslücken, fehlende Tests prüfen"],
  ["research", "Recherche", "Im Web nach neuen Versionen, Tools und Best Practices suchen"],
  ["improve", "Verbesserungen", "Bisherige Arbeit analysieren und nächste Schritte vorschlagen"],
];

const KIND_LABEL: Record<string, string> = {
  oauth_token: "Abo-Token",
  auth_json: "Abo-Login (auth.json)",
  oauth_creds: "Abo-Login (oauth_creds.json)",
  api_key: "API-Key",
  none: "—",
};

const HELP: Record<string, Record<string, string>> = {
  claude: {
    oauth_token: "Nutzt dein Claude Pro/Max-Abo. Lokal im Terminal: `claude setup-token` ausführen und den angezeigten Token (sk-ant-oat…) hier einfügen.",
    api_key: "Anthropic API-Key (sk-ant-api…), Abrechnung pro Token.",
  },
  codex: {
    auth_json: "Nutzt dein ChatGPT Plus/Pro-Abo. Lokal `codex login` ausführen, dann den Inhalt von ~/.codex/auth.json hier einfügen. Erneuerte Tokens werden automatisch zurückgespeichert.",
    api_key: "OpenAI API-Key (sk-…), Abrechnung pro Token.",
  },
  gemini: {
    oauth_creds: "Nutzt dein Google-Konto/Abo. Lokal `gemini` starten, mit Google anmelden, dann den Inhalt von ~/.gemini/oauth_creds.json hier einfügen.",
    api_key: "Gemini API-Key aus Google AI Studio.",
  },
};

export function SettingsView() {
  const { settings, reloadSettings, meta, toast, me } = useStore();
  const [s, setS] = useState<Settings | null>(settings);
  useEffect(() => setS(settings), [settings]);
  if (!s) return <div className="muted">Lade…</div>;

  const save = async () => {
    try {
      await api.put("/settings", s);
      await reloadSettings();
      toast({ title: "Einstellungen gespeichert" });
    } catch (e) {
      toast({ title: "Fehler", body: e instanceof ApiError ? e.message : String(e), tone: "error" });
    }
  };
  const toggleAction = (a: string) =>
    setS({ ...s, onempty_actions: s.onempty_actions.includes(a) ? s.onempty_actions.filter((x) => x !== a) : [...s.onempty_actions, a] });

  return (
    <div className="settings">
      <div className="card stack">
        <h3>Ausführung</h3>
        <div className="row wrap">
          <label>
            Parallele Agenten
            <input type="number" min={1} max={16} value={s.parallelism} onChange={(e) => setS({ ...s, parallelism: +e.target.value })} />
            <span className="muted small">1 = seriell. Pro Projekt läuft immer nur ein Agent.</span>
          </label>
          <label>
            Standard-Provider
            <select value={s.default_provider} onChange={(e) => setS({ ...s, default_provider: e.target.value })}>
              {meta.providers.map((p) => (
                <option key={p}>{p}</option>
              ))}
            </select>
          </label>
          <label>
            Max. Fix-Versuche
            <input type="number" min={0} max={10} value={s.max_fix_attempts} onChange={(e) => setS({ ...s, max_fix_attempts: +e.target.value })} />
            <span className="muted small">Danach: blockiert + Benachrichtigung</span>
          </label>
          <label>
            Auto-Freigabe bis
            <input type="number" min={0} max={20} value={s.max_auto_tasks} onChange={(e) => setS({ ...s, max_auto_tasks: +e.target.value })} />
            <span className="muted small">neue Agent-Aufträge; darüber wirst du gefragt</span>
          </label>
        </div>

        <h3>OnEmptyQueue</h3>
        <label className="check">
          <input type="checkbox" checked={s.onempty_enabled} onChange={(e) => setS({ ...s, onempty_enabled: e.target.checked })} />
          Wenn die Queue leer ist, selbstständig nach sinnvoller Arbeit suchen
        </label>
        {s.onempty_enabled && (
          <>
            {ACTIONS.map(([id, label, desc]) => (
              <label key={id} className="check">
                <input type="checkbox" checked={s.onempty_actions.includes(id)} onChange={() => toggleAction(id)} />
                <strong>{label}</strong> <span className="muted">– {desc}</span>
              </label>
            ))}
            <label>
              Höchstens alle … Minuten
              <input type="number" min={5} value={s.onempty_cooldown_minutes} onChange={(e) => setS({ ...s, onempty_cooldown_minutes: +e.target.value })} />
              <span className="muted small">
                300 = einmal pro 5h-Abo-Fenster. Läuft nur, wenn nichts auf dich wartet und das Limit nicht erreicht ist.
                {s.last_onempty_at && ` Zuletzt: ${fmtTime(s.last_onempty_at)}`}
              </span>
            </label>
          </>
        )}

        <h3>Benachrichtigungen</h3>
        <label>
          ntfy-URL <span className="muted">(optional, z. B. https://ntfy.sh/mein-geheimes-topic)</span>
          <input value={s.ntfy_url ?? ""} onChange={(e) => setS({ ...s, ntfy_url: e.target.value })} placeholder="https://ntfy.sh/…" />
        </label>
        <PushControls />
        <div className="row">
          <button className="primary" onClick={save}>
            Speichern
          </button>
        </div>
      </div>
      <Budgets />
      <Credentials />
      <div className="card row">
        <div className="grow">
          Angemeldet als <strong>{me.name}</strong> <span className="muted">{me.email}</span>
        </div>
        <button className="small" onClick={() => api.post("/auth/logout").then(() => (location.href = "/"))}>
          Abmelden
        </button>
      </div>
    </div>
  );
}

function PushControls() {
  const { toast } = useStore();
  const supported = "serviceWorker" in navigator && "PushManager" in window;
  const [state, setState] = useState<string>(supported ? Notification.permission : "unsupported");

  const enable = async () => {
    try {
      const reg = await navigator.serviceWorker.register("/sw.js");
      const perm = await Notification.requestPermission();
      setState(perm);
      if (perm !== "granted") return;
      const { public_key } = await api.get<{ public_key: string }>("/push/key");
      const sub = await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: b64ToBytes(public_key) });
      await api.post("/push/subscribe", sub.toJSON());
      toast({ title: "Push aktiviert" });
    } catch (e) {
      toast({ title: "Push fehlgeschlagen", body: String(e), tone: "error" });
    }
  };

  if (!supported)
    return <p className="muted small">Web Push wird hier nicht unterstützt. Auf iPhone/iPad: Seite über „Teilen → Zum Home-Bildschirm“ als App installieren.</p>;
  return (
    <div className="row wrap">
      <button className="small" onClick={enable}>
        {state === "granted" ? "Push auf diesem Gerät erneuern" : "Push auf diesem Gerät aktivieren"}
      </button>
      <button className="small ghost" onClick={() => api.post("/push/test")}>
        Test senden
      </button>
      <span className="muted small">Status: {state}</span>
    </div>
  );
}


function Credentials() {
  const { meta, toast } = useStore();
  const [creds, setCreds] = useState<{ provider: string; kind: string; updated_at: string }[]>([]);
  const load = () => api.get<typeof creds>("/credentials").then(setCreds);
  useEffect(() => {
    load();
  }, []);

  return (
    <div className="card stack">
      <h3>Provider-Zugänge</h3>
      <p className="muted small">Jeder User nutzt sein eigenes Abo. Secrets werden verschlüsselt gespeichert (AES-256-GCM) und nie wieder angezeigt.</p>
      {meta.providers
        .filter((p) => p !== "mock")
        .map((p) => (
          <CredentialRow key={p} provider={p} kinds={meta.credential_kinds[p] ?? []} current={creds.find((c) => c.provider === p)} onSaved={load} toast={toast} />
        ))}
    </div>
  );
}

function CredentialRow({
  provider,
  kinds,
  current,
  onSaved,
  toast,
}: {
  provider: string;
  kinds: string[];
  current?: { kind: string; updated_at: string };
  onSaved: () => void;
  toast: ReturnType<typeof useStore>["toast"];
}) {
  const [open, setOpen] = useState(false);
  const [kind, setKind] = useState(kinds[0]);
  const [secret, setSecret] = useState("");
  const save = async () => {
    try {
      await api.put(`/credentials/${provider}`, { kind, secret });
      setSecret("");
      setOpen(false);
      onSaved();
      toast({ title: `${provider} verbunden` });
    } catch (e) {
      toast({ title: "Fehler", body: e instanceof ApiError ? e.message : String(e), tone: "error" });
    }
  };
  return (
    <div className="cred">
      <div className="row">
        <strong className="grow">{providerLabel(provider)}</strong>
        {current ? <span className="tag ok">{KIND_LABEL[current.kind] ?? current.kind} · {fmtTime(current.updated_at)}</span> : <span className="tag">nicht verbunden</span>}
        <button className="small" onClick={() => setOpen(!open)}>
          {current ? "Ändern" : "Verbinden"}
        </button>
        {current && (
          <button className="icon" title="Entfernen" onClick={() => confirm(`${provider}-Zugang entfernen?`) && api.del(`/credentials/${provider}`).then(onSaved)}>
            ✕
          </button>
        )}
      </div>
      {open && (
        <div className="stack">
          <div className="seg">
            {kinds.map((k) => (
              <button key={k} className={kind === k ? "active" : ""} onClick={() => setKind(k)}>
                {KIND_LABEL[k] ?? k}
              </button>
            ))}
          </div>
          <p className="muted small">{HELP[provider]?.[kind]}</p>
          <textarea rows={kind.includes("json") || kind === "oauth_creds" ? 5 : 1} className="mono" value={secret} onChange={(e) => setSecret(e.target.value)} placeholder={kind === "api_key" || kind === "oauth_token" ? "Token einfügen" : "JSON-Inhalt einfügen"} />
          <div className="row">
            <button className="primary small" disabled={!secret.trim()} onClick={save}>
              Speichern
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

function Budgets() {
  const { limits, meta } = useStore();
  return (
    <div className="card stack" data-testid="budgets">
      <h3>Token-Budgets</h3>
      <p className="muted small">
        Rollierende Fenster pro Provider. Ist ein Budget erreicht, startet die Queue für diesen Provider keine neuen Aufträge, bis genug Verbrauch aus dem Fenster
        gefallen ist. Gezählt werden Input + Output + Cache-Writes, Cache-Reads separat. Trifft ein echtes Abo-Limit, merkt sich die Queue den Verbrauch an diesem Punkt
        als <em>beobachtetes Limit</em>, das ist ein guter Startwert für dein 5h-Budget.
      </p>
      {meta.providers.map((p) => {
        const l = limits.find((x) => x.provider === p);
        return l ? <BudgetRow key={p} l={l} /> : null;
      })}
    </div>
  );
}

function Bar({ used, limit, label }: { used: number; limit: number | null; label: string }) {
  const pct = limit ? Math.min(100, (used / limit) * 100) : 0;
  return (
    <div className="bar-wrap" title={label}>
      <div className="bar-label small">
        <span>{label}</span>
        <span className="muted">{limit ? `${Math.round(pct)} %` : "kein Limit"}</span>
      </div>
      <div className="bar">
        <div className={`bar-fill ${pct >= 100 ? "err" : pct >= 80 ? "warn" : ""}`} style={{ width: `${pct}%` }} />
      </div>
    </div>
  );
}

function BudgetRow({ l }: { l: Limit }) {
  const { toast, reload } = useStore();
  const [b5, setB5] = useState(l.limit_5h_tokens?.toString() ?? "");
  const [b7, setB7] = useState(l.limit_7d_tokens?.toString() ?? "");
  const [usd, setUsd] = useState(l.limit_30d_usd?.toString() ?? "");
  const num = (v: string) => (v.trim() === "" ? null : Number(v));
  const save = async () => {
    try {
      await api.put(`/budgets/${l.provider}`, { limit_5h_tokens: num(b5), limit_7d_tokens: num(b7), limit_30d_usd: num(usd) });
      await reload();
      toast({ title: `Budget für ${providerLabel(l.provider)} gespeichert` });
    } catch (e) {
      toast({ title: "Fehler", body: e instanceof ApiError ? e.message : String(e), tone: "error" });
    }
  };
  return (
    <div className="cred" data-testid={`budget-${l.provider}`}>
      <div className="row">
        <strong className="grow">{providerLabel(l.provider)}</strong>
        {l.limited_until && <span className="tag err">Abo-Limit bis {fmtTime(l.limited_until)}</span>}
        {l.budget_until && <span className="tag err">Budget erschöpft bis {fmtTime(l.budget_until)}</span>}
      </div>
      <Bar used={l.used_5h} limit={l.limit_5h_tokens} label={`5 Std.: ${fmtTokens(l.used_5h)} Tokens · ${l.runs_5h} Runs · Cache ${fmtTokens(l.cache_read_5h)}`} />
      <Bar used={l.used_7d} limit={l.limit_7d_tokens} label={`7 Tage: ${fmtTokens(l.used_7d)} Tokens`} />
      <Bar used={l.cost_30d} limit={l.limit_30d_usd} label={`30 Tage: $${l.cost_30d.toFixed(2)}`} />
      <div className="row wrap">
        <label>
          5h-Budget (Tokens)
          <input inputMode="numeric" value={b5} onChange={(e) => setB5(e.target.value)} placeholder="unbegrenzt" />
        </label>
        <label>
          7-Tage-Budget (Tokens)
          <input inputMode="numeric" value={b7} onChange={(e) => setB7(e.target.value)} placeholder="unbegrenzt" />
        </label>
        <label>
          30-Tage-Budget (USD)
          <input inputMode="decimal" value={usd} onChange={(e) => setUsd(e.target.value)} placeholder="unbegrenzt" />
        </label>
      </div>
      <div className="row wrap">
        <button className="small primary" onClick={save}>
          Speichern
        </button>
        {l.observed_limit_tokens ? (
          <button className="small ghost" onClick={() => setB5(String(Math.floor(l.observed_limit_tokens! * 0.9)))}>
            Beobachtetes Limit {fmtTokens(l.observed_limit_tokens)} ({fmtTime(l.observed_at)}) → 90 % übernehmen
          </button>
        ) : null}
      </div>
    </div>
  );
}
