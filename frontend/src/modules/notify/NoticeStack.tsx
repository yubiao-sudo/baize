import { useDock } from "../../kernel/store/dock";

export function NoticeStack() {
  const notices = useDock((s) => s.notices);
  const removeNotice = useDock((s) => s.removeNotice);
  if (notices.length === 0) return null;
  return (
    <div className="notistack">
      {notices.map((n) => (
        <div
          key={n.id}
          className={`notecard ${n.tier === "alert" ? "alert" : ""}`}
          onClick={() => { n.onClick?.(); removeNotice(n.id); }}
        >
          <i>{n.tier === "alert" ? "⚑" : "◈"}</i>
          <div>
            {n.title && <div style={{ fontSize: 12.5, fontWeight: 600, color: "var(--violet-hi)", marginBottom: 3 }}>{n.title}</div>}
            <p>{n.body}</p>
          </div>
        </div>
      ))}
    </div>
  );
}