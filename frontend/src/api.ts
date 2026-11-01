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

export type ProjectListItem = { id: string; name: string; description: string; role: string; owner_name: string; git_remote_url: string | null };

export type Commit = { sha: string; author: string; date: string; subject: string };

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
  git_remote_url: string | null;
  git_branch: string;
  auto_push: boolean;
  has_git_token: boolean;
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
  used_5h: number;
  used_7d: number;
  cost_30d: number;
  cache_read_5h: number;
  runs_5h: number;
  limit_5h_tokens: number | null;
  limit_7d_tokens: number | null;
  limit_30d_usd: number | null;
  limited_until: string | null;
  budget_until: string | null;
  budget_reason: string | null;
  observed_limit_tokens: number | null;
  observed_at: string | null;
  blocked: boolean;
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

export const PROVIDER_LABEL: Record<string, string> = { claude: "Claude", codex: "Codex (GPT)", gemini: "Gemini", mock: "Mock" };
export const providerLabel = (p: string) => PROVIDER_LABEL[p] ?? p;

/** Highest utilisation (0..1+) across configured budgets, or null if none set. */
export function budgetUsage(l: Limit): number | null {
  const parts: number[] = [];
  if (l.limit_5h_tokens) parts.push(l.used_5h / l.limit_5h_tokens);
  if (l.limit_7d_tokens) parts.push(l.used_7d / l.limit_7d_tokens);
  if (l.limit_30d_usd) parts.push(l.cost_30d / l.limit_30d_usd);
  return parts.length ? Math.max(...parts) : null;
}

export function limitChip(l: Limit): { text: string; tone: "" | "warn" | "err" } {
  const name = providerLabel(l.provider);
  if (l.limited_until) return { text: `${name} ⏸ Abo-Limit bis ${fmtTime(l.limited_until)}`, tone: "err" };
  if (l.budget_until) return { text: `${name} ⏸ Budget bis ${fmtTime(l.budget_until)}`, tone: "err" };
  const u = budgetUsage(l);
  if (u !== null) return { text: `${name} ${Math.round(u * 100)} %`, tone: u >= 0.8 ? "warn" : "" };
  return { text: `${name} ${fmtTokens(l.used_5h)}`, tone: "" };
}

export function b64ToBytes(b64: string) {
  const s = atob(b64.replace(/-/g, "+").replace(/_/g, "/") + "=".repeat((4 - (b64.length % 4)) % 4));
  return Uint8Array.from(s, (c) => c.charCodeAt(0));
}
