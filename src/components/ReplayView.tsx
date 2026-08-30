import { useEffect, useMemo, useState } from "react";
import type { ThoughtEvent } from "../types";

/**
 * 执行回放：把一次任务的思考流变成可播放 / 可拖动的「行动纪录片」。
 * 数据来自消息持久化的 trace（thoughts 列表，含真实时间戳）。
 * 播放节奏依据相邻事件的真实时间差压缩推进；支持倍速、拖动进度条、点时间轴跳步。
 */

const KIND_META: Record<string, { icon: string; label: string; color: string }> = {
  thinking: { icon: "💭", label: "思考", color: "#60a5fa" },
  tool_call: { icon: "🔧", label: "调用工具", color: "#22d3ee" },
  tool_result: { icon: "✓", label: "工具完成", color: "#34d399" },
  tool_progress: { icon: "⏳", label: "执行进度", color: "#22d3ee" },
  permission: { icon: "🔑", label: "等待授权", color: "#fbbf24" },
  memory: { icon: "🧠", label: "记忆检索", color: "#a78bfa" },
  phase: { icon: "▸", label: "阶段", color: "#93c5fd" },
  model: { icon: "🤖", label: "模型", color: "#93c5fd" },
  focus: { icon: "🎯", label: "话题切换", color: "#93c5fd" },
  plan: { icon: "🗺", label: "制定计划", color: "#f472b6" },
  critic: { icon: "⚖", label: "自我审视", color: "#f472b6" },
  rag: { icon: "📚", label: "知识检索", color: "#a78bfa" },
  mode: { icon: "🎭", label: "工作模式", color: "#93c5fd" },
  author_tool: { icon: "🛠", label: "自研工具", color: "#34d399" },
  test_pipeline: { icon: "🧪", label: "测试流水线", color: "#22d3ee" },
  saying: { icon: "💬", label: "说道", color: "#60a5fa" },
  awaken: { icon: "☀", label: "唤醒", color: "#fbbf24" },
};

const meta = (kind: string) => KIND_META[kind] || { icon: "·", label: kind, color: "#93c5fd" };

export default function ReplayView({
  thoughts,
  onClose,
}: {
  thoughts: ThoughtEvent[];
  onClose: () => void;
}) {
  const [idx, setIdx] = useState(0);
  const [playing, setPlaying] = useState(true);
  const [speed, setSpeed] = useState(1);

  const cur = thoughts[idx];
  const total = thoughts.length;

  // 真实时间差 → 播放节奏：相邻事件间隔 / 4（压缩），夹在 260ms~1.6s
  useEffect(() => {
    if (!playing || total === 0) return;
    if (idx >= total - 1) {
      setPlaying(false);
      return;
    }
    const dt = Math.max(0, thoughts[idx + 1].ts - thoughts[idx].ts);
    const delay = Math.min(1600, Math.max(260, dt / 4)) / speed;
    const t = window.setTimeout(() => setIdx((i) => Math.min(i + 1, total - 1)), delay);
    return () => window.clearTimeout(t);
  }, [playing, idx, speed, thoughts, total]);

  // 键盘：← → 步进 / 空格播放 / Esc 关闭
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
      else if (e.key === "ArrowRight") setIdx((i) => Math.min(i + 1, total - 1));
      else if (e.key === "ArrowLeft") setIdx((i) => Math.max(i - 1, 0));
      else if (e.key === " ") {
        e.preventDefault();
        setPlaying((p) => !p);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose, total, thoughts]);

  const span = useMemo(() => {
    if (total < 2) return "";
    const secs = Math.round((thoughts[total - 1].ts - thoughts[0].ts) / 1000);
    return secs >= 60 ? `${Math.floor(secs / 60)}分${secs % 60}秒` : `${secs}秒`;
  }, [thoughts, total]);

  if (!cur) return null;
  const m = meta(cur.kind);

  return (
    <div className="replay-mask" onClick={onClose}>
      <div className="replay" onClick={(e) => e.stopPropagation()}>
        <div className="replay-head">
          <span className="replay-title">▶ 执行回放</span>
          <span className="replay-meta">
            共 {total} 步 · 真实耗时 {span}
          </span>
          <button className="replay-close" onClick={onClose} title="关闭 (Esc)">
            ✕
          </button>
        </div>

        {/* 主舞台：当前步骤 */}
        <div className="replay-stage" style={{ "--step-color": m.color } as React.CSSProperties}>
          <div className="replay-icon">{m.icon}</div>
          <div className="replay-step">
            <span className="replay-badge">{m.label}</span>
            <div className="replay-label">{cur.label}</div>
            {cur.detail && <div className="replay-detail">{cur.detail}</div>}
            {cur.kind === "tool_progress" && cur.progress !== undefined && (
              <div className="replay-progress">
                <div className="replay-progress-fill" style={{ width: `${cur.progress}%` }} />
              </div>
            )}
            <div className="replay-count">
              第 {idx + 1} / {total} 步
            </div>
          </div>
        </div>

        {/* 控制条 */}
        <div className="replay-controls">
          <button className="replay-ctl" onClick={() => setIdx(0)} title="回到开头">
            ⏮
          </button>
          <button
            className="replay-ctl primary"
            onClick={() => {
              if (idx >= total - 1) setIdx(0);
              setPlaying((p) => !p);
            }}
            title="播放 / 暂停 (空格)"
          >
            {playing ? "⏸" : "▶"}
          </button>
          <button
            className="replay-ctl"
            onClick={() => setIdx((i) => Math.min(i + 1, total - 1))}
            title="下一步 (→)"
          >
            ⏭
          </button>
          <input
            className="replay-slider"
            type="range"
            min={0}
            max={Math.max(0, total - 1)}
            value={idx}
            onChange={(e) => {
              setPlaying(false);
              setIdx(Number(e.target.value));
            }}
          />
          <button
            className="replay-ctl speed"
            onClick={() => setSpeed((s) => (s === 1 ? 2 : s === 2 ? 4 : 1))}
            title="播放速度"
          >
            {speed}x
          </button>
        </div>

        {/* 时间轴：每步一个刻度点，颜色按类型 */}
        <div className="replay-timeline">
          {thoughts.map((t, i) => {
            const km = meta(t.kind);
            return (
              <button
                key={t.id || i}
                className={`replay-dot${i === idx ? " current" : i < idx ? " past" : ""}`}
                style={{ "--dot-color": km.color } as React.CSSProperties}
                title={`${km.label} · ${t.label}`}
                onClick={() => {
                  setPlaying(false);
                  setIdx(i);
                }}
              >
                <span className="replay-dot-stick" />
              </button>
            );
          })}
        </div>
      </div>
    </div>
  );
}
