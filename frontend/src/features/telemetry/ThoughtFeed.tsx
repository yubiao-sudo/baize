import { useEffect, useRef } from "react";
import { useChat } from "../../core/store/chat";
import type { ThoughtEvent } from "../../core/types";

const KIND_GLYPH: Record<string, string> = {
  thinking: "✦", tool_call: "⚙", tool_result: "✓", tool_progress: "◔",
  permission: "⚠", awaken: "◈", memory: "✷", phase: "▸", model: "⌁",
  focus: "◎", plan: "☰", critic: "⚖", rag: "⌕", mode: "⌘",
  author_tool: "✎", test_pipeline: "⧗", saying: "❝",
};

function ThoughtRow({ t }: { t: ThoughtEvent }) {
  const showBar =
    t.kind === "tool_progress" &&
    typeof t.progress === "number" &&
    t.phase !== "done" &&
    t.phase !== "failed";
  return (
    <div className={`tf-row ${t.kind}`}>
      <i className="tf-kind">{KIND_GLYPH[t.kind] ?? "·"}</i>
      <div className="tf-main">
        <div className="tf-line">
          <span className="tf-label">{t.label}</span>
          {(t.vendor || t.version) && (
            <span className="tf-meta">{[t.vendor, t.version].filter(Boolean).join(" · ")}</span>
          )}
        </div>
        {t.detail && <div className="tf-detail">{t.detail}</div>}
        {showBar && (
          <div className="tf-bar">
            <span style={{ width: `${t.progress}%` }} />
          </div>
        )}
      </div>
    </div>
  );
}

/** 思维遥测流：实时思考 / 工具调用 / 安装进度 */
export default function ThoughtFeed() {
  const thoughts = useChat((s) => s.thoughts);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = ref.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [thoughts]);

  if (thoughts.length === 0) {
    return <div className="tf-empty">等待白泽思考…</div>;
  }
  return (
    <div className="tf-list" ref={ref}>
      {thoughts.map((t, i) => (
        <ThoughtRow key={t.id ?? i} t={t} />
      ))}
    </div>
  );
}