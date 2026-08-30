import { useState } from "react";
import { useChat } from "../../kernel/store/chat";
import type { ChatMsg, ThoughtEvent } from "../../kernel/types";

const KIND_GLYPH: Record<string, string> = {
  tool_call: "⚙", tool_progress: "↻", tool_result: "✓", author_tool: "⚒",
  permission: "⚑", awaken: "✺", memory: "❂", thinking: "◎",
  phase: "▸", model: "◈", focus: "◎", plan: "✱", critic: "✕", rag: "❖",
  mode: "◉", test_pipeline: "↯", saying: "❝",
};

const KIND_CLASS: Record<string, "tool" | "result" | "perm" | "phase"> = {
  tool_call: "tool", tool_progress: "tool", author_tool: "tool",
  tool_result: "result",
  permission: "perm",
  phase: "phase", plan: "phase", rag: "phase", thinking: "phase",
  awaken: "phase", memory: "phase", model: "phase", focus: "phase",
  critic: "phase", mode: "phase", test_pipeline: "phase", saying: "phase",
};

/**
 * 执行流抽屉：
 * · 汇总历史 assistant 消息的 trace + 当前轮次 thoughts
 * · 可展开 / 折叠头部（点击 header 切换）
 */
export function TraceDrawer() {
  const history = useChat((s) => s.history);
  const thoughts = useChat((s) => s.thoughts);
  const [open, setOpen] = useState(true);

  // 收集所有 trace 条目（从历史消息的 trace 字段）
  const entries: Array<{ kind: string; label: string; detail?: string; progress?: number }> = [];
  history.forEach((m: ChatMsg) => {
    if (Array.isArray(m.trace)) {
      for (const ev of m.trace as ThoughtEvent[]) {
        entries.push({
          kind: ev.kind,
          label: ev.label,
          detail: ev.detail ?? ev.tool ?? ev.model ?? undefined,
          progress: ev.progress_percent,
        });
      }
    }
  });
  // 叠加当前实时 thoughts（按时间顺序插入末尾）
  thoughts.forEach((ev) => {
    entries.push({
      kind: ev.kind,
      label: ev.label,
      detail: ev.detail ?? ev.tool ?? ev.model ?? undefined,
      progress: ev.progress_percent,
    });
  });

  return (
    <div style={{ flex: 1, display: "flex", flexDirection: "column", minHeight: 0 }}>
      <button
        className="trace-head"
        style={{ paddingBottom: 6, marginBottom: 4, borderBottom: "1px solid var(--line)" }}
        onClick={() => setOpen((o) => !o)}
      >
        <span style={{ fontSize: 13 }}>{open ? "▾" : "▸"}</span>
        <b>{entries.length}</b> 条执行记录 · 当前展示 <b>{open ? "全开" : "折叠"}</b>
        <span style={{ marginLeft: "auto", color: "var(--faint)" }}>点击切换</span>
      </button>
      {open && (
        <div className="trace-list">
          {entries.length === 0 && (
            <div style={{ color: "var(--faint)", fontSize: 12, padding: "12px 4px" }}>
              ◌ 暂无执行记录。下一次发射指令后将在此追踪每一次工具 / 思维阶段。
            </div>
          )}
          {entries.slice(-200).map((e, i) => {
            const cls = KIND_CLASS[e.kind];
            return (
              <div key={i} className={`tr-row ${cls ?? ""}`}>
                <i className="tr-sym">{KIND_GLYPH[e.kind] ?? "·"}</i>
                <span className="tr-title">{e.label}</span>
                {e.detail && <span className="tr-desc">{e.detail}</span>}
                {e.progress != null && (
                  <div className="tr-progress" style={{ width: 60, marginLeft: "auto" }}>
                    <span style={{ width: e.progress + "%" }} />
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}