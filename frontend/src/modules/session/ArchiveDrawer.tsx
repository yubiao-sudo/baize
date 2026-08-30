import { useChat } from "../../kernel/store/chat";
import { useDock } from "../../kernel/store/dock";

function fmt(ts: number | string | null | undefined): string {
  if (!ts) return "";
  const d = new Date(typeof ts === "string" ? ts : ts);
  if (isNaN(d.getTime())) return "";
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}/${String(d.getMonth() + 1).padStart(2, "0")}/${String(d.getDate()).padStart(2, "0")} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

export function ArchiveDrawer() {
  const open = useDock((s) => s.archiveOpen);
  const toggle = useDock((s) => s.toggleArchive);
  const { conversations, currentConvId, switchConversation, newConversation, removeConversation } = useChat();

  if (!open) return null;
  const select = async (id: string) => {
    await switchConversation(id);
    toggle(false);
  };
  const create = async () => {
    await newConversation();
    toggle(false);
  };
  return (
    <div className="drawer-mask" onClick={(e) => { if (e.target === e.currentTarget) toggle(false); }}>
      <aside className="drawer" onClick={(e) => e.stopPropagation()}>
        <div className="drawer-head">
          <span className="pad-sigil" />
          会话档案
          <span style={{ fontFamily: "var(--mono)", fontSize: 11, color: "var(--faint)", letterSpacing: 1.5, marginLeft: 8 }}>SESSION · ARCHIVE</span>
          <button className="iconbtn" onClick={() => toggle(false)}>✕</button>
        </div>
        <div className="pad-body" style={{ flex: 1, overflowY: "auto" }}>
          <div className="seshtree">
            {conversations.length === 0 && (
              <div style={{ padding: 40, textAlign: "center", color: "var(--faint)", fontSize: 12 }}>
                档案为空，开启一段新会话吧
              </div>
            )}
            {conversations.map((c) => (
              <div
                key={c.id}
                className={`sesh-row ${c.id === currentConvId ? "on" : ""}`}
                onClick={() => select(c.id)}
                title={c.title}
              >
                <span className="sesh-dot" />
                <span className="sesh-t">{c.title || "（未命名会话）"}</span>
                <span className="sesh-meta">{fmt(c.updated_at ?? c.created_at)}</span>
                <button
                  className="iconbtn danger"
                  onClick={(e) => {
                    e.stopPropagation();
                    if (confirm("确认删除此会话？")) void removeConversation(c.id);
                  }}
                  style={{ marginLeft: 4 }}
                >✕</button>
              </div>
            ))}
          </div>
          <button className="new-sesh-btn" onClick={create}>＋ 开启新的水晶会话</button>
        </div>
      </aside>
    </div>
  );
}