import { useChat } from "../../kernel/store/chat";
import { useDock } from "../../kernel/store/dock";

function fmt(ts: number | string | null | undefined): string {
  if (!ts) return "";
  const d = new Date(typeof ts === "string" ? ts : ts);
  if (isNaN(d.getTime())) return "";
  const now = new Date();
  const diff = (now.getTime() - d.getTime()) / 86400000;
  const pad = (n: number) => String(n).padStart(2, "0");
  if (diff < 1) return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  if (diff < 7) return `${d.getMonth() + 1}/${d.getDate()}`;
  return `${d.getFullYear()}/${d.getMonth() + 1}/${d.getDate()}`;
}

export function SessionTree() {
  const { conversations, currentConvId, switchConversation, newConversation, removeConversation } = useChat();
  const toggleArchive = useDock((s) => s.toggleArchive);

  const create = async () => {
    await newConversation();
    toggleArchive(false);
  };

  return (
    <>
      <div className="pad-header">
        <span className="pad-sigil" />
        <span className="pad-title">会话时序</span>
        <span className="pad-sub">SESSION · INDEX</span>
        <div className="pad-tools">
          <button className="iconbtn primary" onClick={() => toggleArchive(true)} title="全量档案面板">◳</button>
        </div>
      </div>
      <div className="pad-body">
        <div className="seshtree">
          {conversations.map((c) => (
            <div
              key={c.id}
              className={`sesh-row ${c.id === currentConvId ? "on" : ""}`}
              onClick={() => switchConversation(c.id)}
              title={c.title}
            >
              <span className="sesh-dot" />
              <span className="sesh-t">{c.title || "（未命名会话）"}</span>
              <span className="sesh-meta">{fmt(c.updated_at ?? c.created_at)}</span>
            </div>
          ))}
          {conversations.length === 0 && (
            <div style={{ padding: "20px 4px", color: "var(--faint)", fontSize: 12, textAlign: "center" }}>
              暂无会话，开始创造一段对话吧
            </div>
          )}
        </div>
        <button className="new-sesh-btn" onClick={create}>＋ 开启新的水晶会话</button>
      </div>
    </>
  );
}