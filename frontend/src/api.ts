export type Usage = {
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  cost_usd: number;
};

export type TaskStatus =
  | "pending_approval"
  | "queued"
  | "running"
  | "waiting_input"
  | "done"
  | "failed"
  | "blocked"
  | "cancelled";

export type Task = {
  id: string;
  user_id: string;
  project_id: string;
  parent_id: string | null;
  kind: "task" | "fix" | "onempty";
  title: string;
  prompt: string;
  context: string;
  provider: string;
  model: string | null;
  status: TaskStatus;
  position: number;
  attempt: number;
  depth: number;
  created_by_agent: boolean;
  questions: string[] | null;
  handoff: string | null;
  summary: string | null;
  error: string | null;
  review: { approved: boolean; issues: string[] } | null;
  usage: Usage | null;
  snapshot_before: string | null;
  snapshot_after: string | null;
  created_at: string;
  started_at: string | null;
  finished_at: string | null;
  updated_at: string;
};

export type TaskEvent = { id: number; ts: string; kind: string; content: string };

export type ProjectListItem = { id: string; name: string; description: string; role: string; owner_name: string };

export type Project = {
  id: string;
  owner_id: string;
  name: string;
  description: string;
  goal: string;
  verify_command: string | null;
  review_enabled: boolean;
  review_provider: string | null;
  review_model: string | null;
  role: "owner" | "editor" | "viewer";
};

export type Settings = {
  parallelism: number;
  default_provider: string;
  onempty_enabled: boolean;
  onempty_actions: string[];
  onempty_cooldown_minutes: number;
  max_auto_tasks: number;
  max_fix_attempts: number;
  ntfy_url: string | null;
  last_onempty_at?: string | null;
};

export type Limit = {
  provider: string;
  limited_until: string | null;
  window_started_at: string | null;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cost_usd: number;
  runs: number;
};

export type Notification = {
  id: string;
  kind: string;
  title: string;
  body: string;
  task_id: string | null;
  read_at: string | null;
  created_at: string;
  push?: boolean;
};

export type Meta = {
  providers: string[];
  credential_kinds: Record<string, string[]>;
  oidc: { id: string; name: string }[];
  allow_registration: boolean;
};

export type User = { id: string; email: string; name: string; is_admin: boolean };

export class ApiError extends Error {
  constructor(public status: number, message: string) {
    super(message);
  }
}

async function req<T>(method: string, path: string, body?: unknown): Promise<T> {
  const init: RequestInit = { method, credentials: "same-origin", headers: {} };
  if (body instanceof Blob || body instanceof ArrayBuffer) {
    init.body = body as BodyInit;
    (init.headers as Record<string, string>)["Content-Type"] = "application/octet-stream";
  } else if (body !== undefined) {
    init.body = JSON.stringify(body);
    (init.headers as Record<string, string>)["Content-Type"] = "application/json";
  }
  const r = await fetch(`/api${path}`, init);
  if (!r.ok) {
    let msg = r.statusText;
    try {
      msg = (await r.json()).error ?? msg;
    } catch {
      /* not json */
    }
    throw new ApiError(r.status, msg);
  }
  return r.json() as Promise<T>;
}

export const api = {
  get: <T>(p: string) => req<T>("GET", p),
  post: <T>(p: string, b?: unknown) => req<T>("POST", p, b ?? {}),
  put: <T>(p: string, b: unknown) => req<T>("PUT", p, b),
  patch: <T>(p: string, b: unknown) => req<T>("PATCH", p, b),
  del: <T>(p: string) => req<T>("DELETE", p),
  upload: <T>(p: string, f: Blob) => req<T>("POST", p, f),
};

export const STATUS_LABEL: Record<TaskStatus, string> = {
  pending_approval: "Freigabe",
  queued: "Wartend",
  running: "Läuft",
  waiting_input: "Rückfrage",
  done: "Erledigt",
  failed: "Fehlgeschlagen",
  blocked: "Blockiert",
  cancelled: "Abgebrochen",
};

export const fmtTokens = (n: number) => (n >= 1e6 ? `${(n / 1e6).toFixed(1)}M` : n >= 1e3 ? `${Math.round(n / 1e3)}k` : `${n}`);

export const fmtTime = (iso: string | null) => {
  if (!iso) return "";
  const d = new Date(iso);
  const sameDay = d.toDateString() === new Date().toDateString();
  return sameDay
    ? d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })
    : d.toLocaleString([], { day: "2-digit", month: "2-digit", hour: "2-digit", minute: "2-digit" });
};

export const duration = (a: string | null, b: string | null) => {
  if (!a) return "";
  const s = Math.max(0, Math.round(((b ? new Date(b) : new Date()).getTime() - new Date(a).getTime()) / 1000));
  return s < 60 ? `${s}s` : s < 3600 ? `${Math.floor(s / 60)}m ${s % 60}s` : `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
};
