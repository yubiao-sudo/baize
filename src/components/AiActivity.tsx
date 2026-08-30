import { useEffect, useState } from "react";
import { useChat } from "../stores/chat";
import type { ThoughtEvent } from "../types";

/**
 * AI 活动状态条：从思考流事件 + busy/streaming 派生「白泽正在做什么」。
 * 空闲 / 思考 / 调用工具 / 规划阶段 / 生成中，纯派生展示（白龙马 Brain UI 的 ai-activity）。
 */
export interface Activity {
  label: string;
  detail: string;
  tone: "idle" | "active" | "tool" | "phase";
}

export function derive(thoughts: ThoughtEvent[], busy: boolean, streaming: string): Activity {
  if (!busy) return { label: "空闲", detail: "", tone: "idle" };
  if (streaming) return { label: "生成中", detail: "正在组织语言", tone: "active" };
  const last = thoughts[thoughts.length - 1];
  if (!last) return { label: "思考中", detail: "", tone: "active" };
  switch (last.kind) {
    case "tool_call":
      return { label: "调用工具", detail: last.label.replace(/^调用工具 · /, ""), tone: "tool" };
    case "tool_result":
      return { label: "工具完成", detail: last.label.replace(/^工具完成 · /, ""), tone: "tool" };
    case "phase":
      return { label: last.label, detail: last.detail, tone: "phase" };
    case "model":
      return { label: last.label, detail: "", tone: "phase" };
    case "focus":
      return { label: "话题切换", detail: last.detail, tone: "phase" };
    case "plan":
      return { label: "制定计划", detail: last.detail, tone: "phase" };
    default:
      return { label: "思考中", detail: "", tone: "active" };
  }
}

export default function AiActivity() {
  const busy = useChat((s) => s.busy);
  const streaming = useChat((s) => s.streaming);
  const thoughts = useChat((s) => s.thoughts);
  const [state, setState] = useState<Activity>(() => derive(thoughts, busy, streaming));

  useEffect(() => {
    setState(derive(thoughts, busy, streaming));
  }, [thoughts, busy, streaming]);

  return (
    <div className={`ai-activity ${state.tone}`}>
      <span className="ai-activity-dot" />
      <span className="ai-activity-label">{state.label}</span>
      {state.detail && <span className="ai-activity-detail">{state.detail}</span>}
    </div>
  );
}
