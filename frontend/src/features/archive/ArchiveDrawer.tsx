import { exportConversation } from "../../core/api";
import { useChat } from "../../core/store/chat";
import { useHud } from "../../core/store/hud";

const fmt = (ts: number) => {
  const d = new Date(ts);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
};

/** 会话档案抽屉：切换 / 新建 / 导出 / 删除 */
export default function ArchiveDrawer() {
  const conversations = useChat((s) => s.conversations);
  const currentConvId = useChat((s) => s.currentConvId);
  const switchConversation = useChat((s) => s.switchConversation);
  const newConversation = useChat((s) => s.newConversation);
  const removeConversation = useChat((s) => s.removeConversation);
  const toggleArchive = useHud((s) => s.toggleArchive);
  const notify = useHud((s) => s.notify);

  return (
    <div className="archive-mask" onClick={toggleArchive}>
      <aside className="archive" onClick={(e) => e.stopPropagation()}>
        <header className="arc-h">
          <span>会话档案</span>
          <button className="mini-btn" onClick={toggleArchive}>
            ✕
          </button>
        </header>
        <div className="arc-list">
          {conversations.map((c) => (
            <div
              key={c.id}
              className={`arc-item ${c.id === currentConvId ? "on" : ""}`}
              onClick={() => void switchConversation(c.id)}
            >
              <span className="arc-title">{c.title || "未命名会话"}</span>
              <span className="arc-time">{fmt(c.created_at)}</span>
              <span className="arc-ops" onClick={(e) => e.stopPropagation()}>
                <button
                  className="mini-btn"
                  title="导出会话"
                  onClick={() =>
                    void exportConversation(c.id).then((p) => {
                      if (p) notify("info", `已导出：${p}`);
                    })
                  }
                >
                  ⤓
                </button>
                <button
                  className="mini-btn danger"
                  title="删除会话"
                  onClick={() => void removeConversation(c.id)}
                >
                  ✕
                </button>
              </span>
            </div>
          ))}
        </div>
        <footer className="arc-f">
          <button className="arc-new" onClick={() => void newConversation()}>
            ✚ 新会话
          </button>
        </footer>
      </aside>
    </div>
  );
}