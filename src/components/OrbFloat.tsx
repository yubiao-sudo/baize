import { useEffect, useRef, useState } from "react";
import { emit, listen } from "@tauri-apps/api/event";
import { getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";
import type { ThoughtEvent } from "../types";

/**
 * 桌面悬浮球（#/orb 独立透明窗口）：
 * - 64px 小水球常驻桌面，颜色随白泽活动状态呼吸（空闲蓝 / 思考紫 / 干活青 / 说话亮青脉冲）
 * - 单击展开迷你面板（窗口随之放大）：当前活动 + 快速派任务输入 + 回主窗
 * - 状态来源：主窗 baize-orb-status 广播 + baize:tts-* 语音事件；输入经 baize-float-send 转发主窗
 * - 球体区域可拖动；右上 ✕ 关闭悬浮球
 */

const TONE_COLOR: Record<string, string> = {
  idle: "#3b82f6",
  active: "#a78bfa",
  tool: "#22d3ee",
  phase: "#60a5fa",
};

interface OrbStatus {
  label: string;
  detail: string;
  tone: "idle" | "active" | "tool" | "phase";
  busy: boolean;
}

export default function OrbFloat() {
  const [expanded, setExpanded] = useState(false);
  const [status, setStatus] = useState<OrbStatus>({
    label: "待命",
    detail: "",
    tone: "idle",
    busy: false,
  });
  const [speaking, setSpeaking] = useState(false);
  const [pulse, setPulse] = useState(0);
  const [draft, setDraft] = useState("");
  const pulseRef = useRef(0);

  // 悬浮球窗口本体透明：body 背景由 index.css 的 --bg 覆盖为透明
  useEffect(() => {
    document.documentElement.style.background = "transparent";
    document.body.style.background = "transparent";
  }, []);

  // 主窗活动状态广播
  useEffect(() => {
    const unlisteners: Promise<() => void>[] = [
      listen<OrbStatus>("baize-orb-status", (e) => setStatus(e.payload)),
      listen<{ speaking: boolean }>("baize:tts-state", (e) => setSpeaking(e.payload.speaking)),
      listen<{ energy: number }>("baize:tts-pulse", (e) => {
        pulseRef.current = e.payload.energy;
        setPulse(e.payload.energy);
        window.setTimeout(() => setPulse(0), 160);
      }),
    ];
    return () => {
      void Promise.all(unlisteners).then((fs) => fs.forEach((f) => f()));
    };
  }, []);

  // 展开 / 收起：窗口尺寸跟随
  useEffect(() => {
    const w = getCurrentWindow();
    void w.setSize(new LogicalSize(expanded ? 264 : 64, expanded ? 330 : 64));
  }, [expanded]);

  const color = speaking ? "#22d3ee" : TONE_COLOR[status.tone] || "#3b82f6";
  const scale = 1 + pulse * 0.16 + (status.busy ? 0.02 : 0);

  // 拖动：位移超 4px 才进入系统拖动（阈值内松手仍是点击展开，二者互不干扰）
  const downPos = useRef<{ x: number; y: number } | null>(null);
  const dragStarted = useRef(false);
  const onMouseDown = (e: React.MouseEvent) => {
    if (e.button !== 0) return;
    downPos.current = { x: e.clientX, y: e.clientY };
    dragStarted.current = false;
  };
  const onMouseMove = (e: React.MouseEvent) => {
    const p = downPos.current;
    if (!p || dragStarted.current) return;
    if (Math.hypot(e.clientX - p.x, e.clientY - p.y) > 4) {
      dragStarted.current = true;
      void getCurrentWindow().startDragging();
    }
  };
  const onClick = () => {
    if (dragStarted.current) return;
    setExpanded((v) => !v);
  };

  const send = () => {
    const text = draft.trim();
    if (!text) return;
    void emit("baize-float-send", { text }).catch(() => {});
    setDraft("");
  };

  return (
    <div className="orb-float-root">
      <div
        className={`orb-float${expanded ? " expanded" : ""}`}
        style={{ "--fo-color": color, transform: `scale(${scale})` } as React.CSSProperties}
        onMouseDown={onMouseDown}
        onMouseMove={onMouseMove}
        onMouseUp={() => (downPos.current = null)}
        onClick={onClick}
      >
        <div className="orb-float-halo" />
        <div className="orb-float-ball" />
        {status.busy && <span className="orb-float-dot" title={status.label} />}
      </div>

      {expanded && (
        <div className="orb-float-panel" onClick={(e) => e.stopPropagation()}>
          <div className="orb-float-head">
            <span className="orb-float-status" style={{ color }}>
              {speaking ? "说话中" : status.label}
            </span>
            <button
              className="orb-float-btn"
              title="聚焦主窗口"
              onClick={() => {
                void emit("baize-float-focus").catch(() => {});
              }}
            >
              ⌂
            </button>
            <button
              className="orb-float-btn"
              title="关闭悬浮球"
              onClick={() => void getCurrentWindow().close()}
            >
              ✕
            </button>
          </div>
          {status.detail && <div className="orb-float-detail">{status.detail}</div>}
          <div className="orb-float-inputrow">
            <input
              className="orb-float-input"
              placeholder="给白泽派个任务…"
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") send();
              }}
              autoFocus
            />
            <button className="orb-float-send" onClick={send} title="发送">
              ➤
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

/** 事件名导出：主窗与悬浮球的契约（App.tsx 侧使用） */
export const FLOAT_EVENTS = ["baize-orb-status", "baize-float-send", "baize-float-focus"] as const;
export type { ThoughtEvent };
