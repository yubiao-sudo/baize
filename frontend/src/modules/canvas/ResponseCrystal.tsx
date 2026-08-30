import { useEffect, useRef } from "react";
import { useChat } from "../../kernel/store/chat";
import { renderMd } from "../../lib/md";
import { ComparePrism } from "./ComparePrism";

/**
 * 水晶应答台：显示完整的助手回复（与 ThreadStream 左栏的 4 行截断形成对比）
 * · 支持 streaming 追加态 · thinking 等待态 · 附件陈列
 * · 空态时渲染 WelcomeCore（用户提供的 send 函数）
 */
export function ResponseCrystal({ empty }: { empty: React.ReactNode }) {
  const history = useChat((s) => s.history);
  const streaming = useChat((s) => s.streaming);
  const busy = useChat((s) => s.busy);
  const scrollRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [history, streaming]);

  // 渲染轮次：user+assistant 成对（user 在水晶台仍简短显示，assistant 全量渲染）
  const assistMsgs: Array<{ idx: number; content: string; isStreaming?: boolean; attach?: string[] }> = [];
  history.forEach((m, idx) => {
    if (m.role === "assistant") {
      assistMsgs.push({ idx, content: m.content, attach: m.attachments });
    }
  });

  if (history.length === 0) {
    return (
      <div ref={scrollRef} style={{ height: "100%", overflow: "auto" }}>
        {empty}
      </div>
    );
  }

  return (
    <div ref={scrollRef} style={{ height: "100%", overflow: "auto", display: "flex", flexDirection: "column", gap: 14 }}>
      {assistMsgs.map((m, i) => {
        const last = i === assistMsgs.length - 1;
        const isStreaming = last && busy && streaming.length > 0;
        const content = (isStreaming ? m.content + streaming : m.content) || " ";
        return (
          <div key={m.idx} className={`t-crystal ${isStreaming ? "live" : ""}`}>
            <div className="crystal-title-row">
              <div className="crystal-title">◈ 第 {i + 1} 次应答 {isStreaming ? " · 输出中…" : ""}</div>
            </div>
            <div className="md" dangerouslySetInnerHTML={{ __html: renderMd(content) }} />
            {last && busy && streaming.length === 0 && !m.content && (
              <div className="thinkdots"><i /><i /><i /></div>
            )}
            {m.attach && m.attach.length > 0 && (
              <div className="crystal-attach">
                {m.attach.map((p, k) => (
                  <span key={p + k}>{p.startsWith("[ws]") ? "🗁" : "📄"} {p.replace(/^\[ws\]/, "").split(/[\\/]/).slice(-1)[0]}</span>
                ))}
              </div>
            )}
          </div>
        );
      })}
      <ComparePrism />
    </div>
  );
}