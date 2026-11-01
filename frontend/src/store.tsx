import { createContext, useCallback, useContext, useEffect, useRef, useState, type ReactNode } from "react";
import { api, type Limit, type Meta, type Notification, type ProjectListItem, type Settings, type Task, type TaskEvent, type User } from "./api";

type Toast = { id: number; title: string; body?: string; taskId?: string | null; tone?: "info" | "error" };

type Store = {
  me: User;
  meta: Meta;
  tasks: Task[];
  projects: ProjectListItem[];
  settings: Settings | null;
  limits: Limit[];
  notifications: Notification[];
  lastEvent: Record<string, TaskEvent>;
  toasts: Toast[];
  openTask: string | null;
  setOpenTask: (id: string | null) => void;
  reload: () => Promise<void>;
  reloadProjects: () => Promise<void>;
  reloadSettings: () => Promise<void>;
  setNotifications: (n: Notification[]) => void;
  toast: (t: Omit<Toast, "id">) => void;
  dismissToast: (id: number) => void;
  onTaskEvent: (taskId: string, fn: (e: TaskEvent) => void) => () => void;
};

const Ctx = createContext<Store | null>(null);

export const useStore = () => {
  const s = useContext(Ctx);
  if (!s) throw new Error("store missing");
  return s;
};

export function StoreProvider({ me, meta, children }: { me: User; meta: Meta; children: ReactNode }) {
  const [tasks, setTasks] = useState<Task[]>([]);
  const [projects, setProjects] = useState<ProjectListItem[]>([]);
  const [settings, setSettings] = useState<Settings | null>(null);
  const [limits, setLimits] = useState<Limit[]>([]);
  const [notifications, setNotifications] = useState<Notification[]>([]);
  const [lastEvent, setLastEvent] = useState<Record<string, TaskEvent>>({});
  const [toasts, setToasts] = useState<Toast[]>([]);
  const [openTask, setOpenTaskState] = useState<string | null>(() => new URLSearchParams(location.search).get("task"));
  const listeners = useRef(new Map<string, Set<(e: TaskEvent) => void>>());

  const setOpenTask = useCallback((id: string | null) => {
    setOpenTaskState(id);
    const u = new URL(location.href);
    if (id) u.searchParams.set("task", id);
    else u.searchParams.delete("task");
    history.replaceState(null, "", u);
  }, []);

  const reloadProjects = useCallback(async () => setProjects(await api.get<ProjectListItem[]>("/projects")), []);
  const reloadSettings = useCallback(async () => setSettings(await api.get<Settings>("/settings")), []);
  const reloadLimits = useCallback(async () => setLimits(await api.get<Limit[]>("/limits")), []);
  const reload = useCallback(async () => {
    const [t, n] = await Promise.all([api.get<Task[]>("/tasks"), api.get<Notification[]>("/notifications")]);
    setTasks(t);
    setNotifications(n);
    await Promise.all([reloadProjects(), reloadSettings(), reloadLimits()]);
  }, [reloadProjects, reloadSettings, reloadLimits]);

  const toast = useCallback((t: Omit<Toast, "id">) => {
    const id = Date.now() + Math.random();
    setToasts((ts) => [...ts.slice(-3), { ...t, id }]);
    setTimeout(() => setToasts((ts) => ts.filter((x) => x.id !== id)), 7000);
  }, []);
  const dismissToast = useCallback((id: number) => setToasts((ts) => ts.filter((x) => x.id !== id)), []);

  const onTaskEvent = useCallback((taskId: string, fn: (e: TaskEvent) => void) => {
    const m = listeners.current;
    if (!m.has(taskId)) m.set(taskId, new Set());
    m.get(taskId)!.add(fn);
    return () => void m.get(taskId)?.delete(fn);
  }, []);

  useEffect(() => {
    reload().catch(() => {});
    let es: EventSource | null = null;
    let retry: ReturnType<typeof setTimeout>;
    const connect = () => {
      es = new EventSource("/api/events");
      es.onmessage = (m) => {
        const msg = JSON.parse(m.data);
        switch (msg.type) {
          case "task": {
            const t: Task = msg.task;
            setTasks((ts) => {
              const i = ts.findIndex((x) => x.id === t.id);
              if (i < 0) return [...ts, t];
              const c = ts.slice();
              c[i] = t;
              return c;
            });
            if (t.status !== "running") reloadLimits().catch(() => {});
            break;
          }
          case "task_event":
            setLastEvent((le) => ({ ...le, [msg.task_id]: msg.event }));
            listeners.current.get(msg.task_id)?.forEach((fn) => fn(msg.event));
            break;
          case "notification": {
            const n: Notification = { ...msg.notification, read_at: null, created_at: new Date().toISOString() };
            setNotifications((ns) => [n, ...ns]);
            toast({ title: n.title, body: n.body, taskId: n.task_id, tone: ["blocked", "task_failed"].includes(n.kind) ? "error" : "info" });
            break;
          }
          case "resync":
            reload().catch(() => {});
            break;
        }
      };
      es.onerror = () => {
        es?.close();
        retry = setTimeout(() => {
          reload().catch(() => {});
          connect();
        }, 3000);
      };
    };
    connect();
    const iv = setInterval(() => reloadLimits().catch(() => {}), 60_000);
    return () => {
      es?.close();
      clearTimeout(retry);
      clearInterval(iv);
    };
  }, [reload, reloadLimits, toast]);

  return (
    <Ctx.Provider
      value={{
        me,
        meta,
        tasks,
        projects,
        settings,
        limits,
        notifications,
        lastEvent,
        toasts,
        openTask,
        setOpenTask,
        reload,
        reloadProjects,
        reloadSettings,
        setNotifications,
        toast,
        dismissToast,
        onTaskEvent,
      }}
    >
      {children}
    </Ctx.Provider>
  );
}
