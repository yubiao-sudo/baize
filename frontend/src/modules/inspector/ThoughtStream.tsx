import { useChat } from "../../kernel/store/chat";
import type { ThoughtEvent } from "../../kernel/types";

const KIND_GLYPH: Partial<Record<ThoughtEvent["kind"], string>> = {
  awaken: "✺", memory: "❂", thinking: "◎", phase: "▸",
  model: "◈", focus: "◎", plan: "✱", critic: "✕", rag: "❖",
  tool_call: "⚙", tool_progress: "↻", tool_result: "✓",
  author_tool: "⚒", permission: "⚑", mode: "◉",
  test_pipeline: "↯", saying: "❝",
};

function metaOf(e: ThoughtEvent): string {
  if (e.model) return e.model;
  if (e.duration_ms != null) return `${e.duration_ms}ms`;
  if (e.tool) return e.tool;
  return "";
}

export function ThoughtStream() {
  const thoughts = useChat((s) => s.thoughts);
  return (
    <>
      <div className="pad-header">
        <span className="pad-sigil" />
        <span className="pad-title">思维流</span>
        <span className="pad-sub">THOUGHT · FEED</span>
      </div>
      <div className="pad-body">
        {thoughts.length === 0 ? (
          <div className="thought-empty">◌ 静待灵感 · AWAITING</div>
        ) : (
          <div className="thought-feed">
            {thoughts.slice().reverse().slice(0, 60).map((e) => {
              const pct = e.progress_percent ?? null;
              const m = metaOf(e);
              return (
                <div className="th-row" key={e.id}>
                  <i className={`th-glyph ${e.kind}`}>{KIND_GLYPH[e.kind] ?? "·"}</i>
                  <div className="th-main">
                    <div className="th-meta">
                      <div className="th-title">{e.label}</div>
                      {m && <small>{m}</small>}
                    </div>
                    {e.detail && <div className="th-desc">{e.detail}</div>}
                    {pct != null && <div className="th-progress"><span style={{ width: pct + "%" }} /></div>}
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </div>
    </>
  );
}