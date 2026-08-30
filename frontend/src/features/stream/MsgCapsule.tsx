import { memo, useMemo, useState } from "react";
import { renderMd } from "../../lib/md";
import type { ChatMsg, ThoughtEvent, Todo } from "../../core/types";
import ComparePrism from "./ComparePrism";

interface Trace {
  thoughts?: ThoughtEvent[];
  todos?: Todo[];
}

function parseTrace(raw?: string): Trace | null {
  if (!raw) return null;
  try {
    const v = JSON.parse(raw) as Trace;
    return v && (v.thoughts?.length || v.todos?.length) ? v : null;
  } catch {
    return null;
  }
}

const basename = (p: string) => p.split(/[\\/]/).pop() ?? p;

const KIND_GLYPH: Record<string, string> = {
  thinking: "✦", tool_call: "⚙", tool_result: "✓", tool_progress: "◔",
  permission: "⚠", awaken: "◈", memory: "✷", phase: "▸", model: "⌁",
  focus: "◎", plan: "☰", critic: "⚖", rag: "⌕", mode: "⌘",
  author_tool: "✎", test_pipeline: "⧗", saying: "❝",
};

/** 执行流折叠甲板：回放本轮思考与工具调用 */
function TraceDeck({ trace }: { trace: Trace }) {
  const [open, setOpen] = useState(false);
  const thoughts = trace.thoughts ?? [];
  const todos = trace.todos ?? [];
  const done = todos.filter((t) => t.status === "completed").length;
  return (
    <div className={`tracedeck ${open ? "open" : ""}`}>
      <button className="tracedeck-toggle" onClick={() => setOpen((v) => !v)}>
        <span className="tg-arrow">{open ? "▾" : "▸"}</span>
        执行流 · {thoughts.length} 步{todos.length ? ` · 任务 ${done}/${todos.length}` : ""}
      </button>
      {open && (
        <div className="tracedeck-body">
          {thoughts.map((t, i) => (
            <div className="td-row" key={t.id ?? i}>
              <i className={`td-kind ${t.kind}`}>{KIND_GLYPH[t.kind] ?? "·"}</i>
              <span className="td-label">{t.label}</span>
              {t.detail && <span className="td-detail">{t.detail}</span>}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

/** 消息胶囊：用户 = 切角指令舱，助手 = 全息应答卡 */
function MsgCapsule({ msg, live }: { msg: ChatMsg; live?: boolean }) {
  const html = useMemo(() => renderMd(msg.content), [msg.content]);
  const trace = useMemo(() => parseTrace(msg.trace), [msg.trace]);

  if (msg.role === "user") {
    return (
      <div className="cap user">
        <div className="cap-h">
          <span className="cap-tag">指挥官</span>
        </div>
        <div className="cap-b user-text">{msg.content}</div>
        {msg.attachments && msg.attachments.length > 0 && (
          <div className="cap-attach">
            {msg.attachments.map((a) => (
              <span key={a} title={a}>◈ {basename(a)}</span>
            ))}
          </div>
        )}
      </div>
    );
  }

  return (
    <div className={`cap assist ${live ? "live" : ""}`}>
      <div className="cap-h">
        <span className="cap-tag">白泽</span>
        {live && <span className="cap-state">接收中</span>}
      </div>
      {msg.branches && msg.branches.length > 0 ? (
        <ComparePrism branches={msg.branches} />
      ) : (
        <div className="cap-b md" dangerouslySetInnerHTML={{ __html: html }} />
      )}
      {trace && <TraceDeck trace={trace} />}
    </div>
  );
}

export default memo(MsgCapsule);