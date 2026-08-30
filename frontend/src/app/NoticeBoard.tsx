import { useHud } from "../core/store/hud";

/** 右上角通知栈（IM 状态 / 系统通知摘要） */
export default function NoticeBoard() {
  const notices = useHud((s) => s.notices);
  const dismiss = useHud((s) => s.dismiss);
  if (notices.length === 0) return null;
  return (
    <div className="notices">
      {notices.map((n) => (
        <div key={n.id} className={`notice ${n.kind}`} onClick={() => dismiss(n.id)}>
          <span className="notice-glyph">{n.kind === "alert" ? "⚠" : "◈"}</span>
          <span className="notice-text">{n.text}</span>
        </div>
      ))}
    </div>
  );
}