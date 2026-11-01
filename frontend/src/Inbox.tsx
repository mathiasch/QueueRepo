import { api, fmtTime } from "./api";
import { useStore } from "./store";

export function InboxView() {
  const { notifications, setNotifications, setOpenTask, tasks } = useStore();
  const attention = tasks.filter((t) => t.status === "waiting_input" || t.status === "pending_approval");
  const unread = notifications.filter((n) => !n.read_at).length;

  const markAll = async () => {
    await api.post("/notifications/read-all");
    const now = new Date().toISOString();
    setNotifications(notifications.map((n) => ({ ...n, read_at: n.read_at ?? now })));
  };
  const open = async (id: string, taskId: string | null) => {
    await api.post(`/notifications/${id}/read`);
    setNotifications(notifications.map((n) => (n.id === id ? { ...n, read_at: new Date().toISOString() } : n)));
    if (taskId) setOpenTask(taskId);
  };

  return (
    <div className="inbox">
      {attention.length > 0 && (
        <section className="section">
          <div className="section-head">
            <h3>Wartet auf dich ({attention.length})</h3>
          </div>
          <div className="list">
            {attention.map((t) => (
              <div key={t.id} className="item attention" onClick={() => setOpenTask(t.id)}>
                <div className="item-main">
                  <span className={`dot ${t.status}`} />
                  <div className="grow">
                    <div className="title">{t.title}</div>
                    <div className="meta muted">{t.status === "waiting_input" ? (t.questions ?? []).join(" · ") : "Freigabe für neuen Agent-Auftrag"}</div>
                  </div>
                </div>
              </div>
            ))}
          </div>
        </section>
      )}
      <section className="section">
        <div className="section-head">
          <h3>Benachrichtigungen</h3>
          {unread > 0 && (
            <button className="small ghost" onClick={markAll}>
              Alle gelesen
            </button>
          )}
        </div>
        <div className="list">
          {notifications.length === 0 && <div className="muted pad">Keine Benachrichtigungen.</div>}
          {notifications.map((n) => (
            <div key={n.id} className={`item notif ${n.read_at ? "read" : "unread"} k-${n.kind}`} onClick={() => open(n.id, n.task_id)}>
              <div className="item-main">
                <div className="grow">
                  <div className="title">{n.title}</div>
                  {n.body && <div className="muted pre clamp">{n.body}</div>}
                </div>
                <span className="muted small">{fmtTime(n.created_at)}</span>
              </div>
            </div>
          ))}
        </div>
      </section>
    </div>
  );
}
