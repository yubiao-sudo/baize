import { useChat } from "../../kernel/store/chat";

export function AttachmentShelf() {
  const attachments: string[] = useChat((s) => (s.history.length ? (s.history[s.history.length - 1]?.attachments ?? []) : []));
  if (attachments.length === 0) return null;
  return (
    <>
      <div className="pad-header">
        <span className="pad-sigil" style={{ background: "var(--orchid)" }} />
        <span className="pad-title">附件架</span>
        <span className="pad-sub">ATTACH · SHELF</span>
      </div>
      <div className="pad-body">
        <div className="shelf-title"><span>当前轮次附件 {attachments.length}</span></div>
        {attachments.map((p, i) => {
          const label = p.replace(/^\[ws\]/, "").split(/[\\/]/).slice(-1)[0];
          const isWs = p.startsWith("[ws]");
          return (
            <div className="attachcard" key={p + i}>
              <i>{isWs ? "🗁" : "📄"}</i>
              <span title={p}>{label}</span>
              <em>{isWs ? "工作目录" : "文件"}</em>
            </div>
          );
        })}
      </div>
    </>
  );
}