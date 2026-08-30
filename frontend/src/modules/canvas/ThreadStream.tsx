import { useChat } from "../../kernel/store/chat";
import type { ChatMsg } from "../../kernel/types";
import { renderMd } from "../../lib/md";

/**
 * Thread Stream 时间线：
 * · 用户消息气泡贴右（渐变胶囊）
 * · 助手摘要贴左（仅显示轮次头部 + 小预览），点击可同步右栏滚动定位
 */
export function ThreadStream({
  onJumpAssistant,
  empty,
}: {
  onJumpAssistant: (idx: number) => void;
  empty: React.ReactNode;
}) {
  const history = useChat((s) => s.history);

  if (history.length === 0) return <>{empty}</>;

  // 轮次切分：user + 之后连续 assistant = 一轮
  const rounds: Array<{ user?: ChatMsg; assist?: ChatMsg; idx: number }> = [];
  let i = 0;
  while (i < history.length) {
    const m = history[i];
    if (m.role === "user") {
      const next = i + 1;
      const nextA = history[next]?.role === "assistant" ? history[next] : undefined;
      rounds.push({ user: m, assist: nextA, idx: i });
      i += nextA ? 2 : 1;
    } else {
      // 孤立 assistant（比如首条系统），也算一轮
      rounds.push({ assist: m, idx: i });
      i++;
    }
  }

  return (
    <div className="threadstream">
      {rounds.map((r, ri) => (
        <div key={r.idx} style={{ display: "flex", flexDirection: "column", gap: 8 }}>
          {r.user && (
            <div className="t-user">
              <div className="t-badge">你 · 第 {ri + 1} 轮</div>
              <div className="t-bubble-user">{r.user.content}</div>
              {r.user.attachments && r.user.attachments.length > 0 && (
                <div style={{ display: "flex", flexWrap: "wrap", gap: 5, marginTop: 4 }}>
                  {r.user.attachments.map((p, k) => {
                    const label = p.replace(/^\[ws\]/, "").split(/[\\/]/).slice(-1)[0];
                    return (
                      <span key={p + k} style={{
                        fontFamily: "var(--mono)", fontSize: 10.5,
                        padding: "3px 8px", borderRadius: 999,
                        background: "var(--violet-dim)", color: "var(--violet-hi)",
                      }}>{p.startsWith("[ws]") ? "🗁" : "📄"} {label}</span>
                    );
                  })}
                </div>
              )}
            </div>
          )}
          {r.assist && (
            <div className="t-assist">
              <button className="t-badge" onClick={() => onJumpAssistant(r.idx + (r.user ? 1 : 0))} style={{ background: "none", textAlign: "left" }}>
                <b>白泽助手</b> · 点击跳转
              </button>
              <div
                className="md"
                style={{
                  fontSize: 12.5, color: "var(--ink-dim)",
                  display: "-webkit-box", WebkitLineClamp: 4, WebkitBoxOrient: "vertical",
                  overflow: "hidden", lineHeight: 1.6,
                }}
                dangerouslySetInnerHTML={{
                  __html: renderMd(
                    r.assist.content.length > 400 ? r.assist.content.slice(0, 400) + "…" : r.assist.content,
                  ),
                }}
              />
            </div>
          )}
        </div>
      ))}
    </div>
  );
}