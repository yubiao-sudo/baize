import { useEffect, useMemo, useRef } from "react";
import { useChat } from "../stores/chat";
import type { ThoughtEvent } from "../types";

// 右侧思考流：突出「执行了什么 / 反思了什么」，隐藏冗余的阶段重复与审批细节
const EXCLUDE_KINDS = new Set(["permission"]);

// 阶段里程碑的友好说明（后端 detail 为空时兜底）
const PHASE_DETAIL: Record<string, string> = {
  规划: "分析需求并制定执行步骤",
  执行: "调用工具执行任务",
  等待授权: "等待你确认本次操作",
  反思: "回顾执行结果，整理关键要点",
  完成: "本轮任务处理完成",
};

// 判断是否需要从思考流中隐藏：审批卡已单独展示，「执行」阶段每轮重复且与「调用工具」信息重复
function isNoise(t: ThoughtEvent): boolean {
  if (EXCLUDE_KINDS.has(t.kind)) return true;
  if (t.kind === "phase" && t.label === "执行") return true;
  return false;
}

// 截断长文本（工具参数/输出），压平换行，便于单行摘要
function clamp(s: string, n = 120): string {
  const one = s.replace(/\s+/g, " ").trim();
  return one.length > n ? one.slice(0, n) + "…" : one;
}

// 从 assistant 消息的 trace（JSON: { thoughts, todos }）解析出该轮的历史思考
function parseTrace(trace?: string): ThoughtEvent[] {
  if (!trace) return [];
  try {
    const obj = JSON.parse(trace) as { thoughts?: unknown };
    if (!Array.isArray(obj.thoughts)) return [];
    const thoughts = (obj.thoughts as Array<{ ts?: number; kind?: string; label?: string; detail?: string }>)
      .filter((t) => t && typeof t.label === "string")
      .map((t) => ({
        ts: t.ts ?? 0,
        kind: (t.kind as ThoughtEvent["kind"]) ?? "thinking",
        label: t.label as string,
        detail: t.detail ?? "",
      }));
    // 合并同一安装的多条 tool_progress，避免右侧思考流刷出几十条重复进度项
    const out: ThoughtEvent[] = [];
    for (const t of thoughts) {
      if (t.kind === "tool_progress") {
        const last = out[out.length - 1];
        if (last && last.kind === "tool_progress" && last.label === t.label) {
          out[out.length - 1] = t;
          continue;
        }
      }
      out.push(t);
    }
    return out;
  } catch {
    return [];
  }
}

export default function ThoughtStream() {
  const history = useChat((s) => s.history);
  const busy = useChat((s) => s.busy);
  const live = useChat((s) => s.thoughts);

  // 聚合整个会话的历史思考流：遍历每条 assistant 消息的 trace
  const historical = useMemo(() => {
    const out: ThoughtEvent[] = [];
    history.forEach((m, mi) => {
      if (m.role !== "assistant") return;
      for (const t of parseTrace(m.trace)) {
        out.push({ ...t, id: `h-${mi}-${out.length}` });
      }
    });
    return out;
  }, [history]);

  // 生成中：历史轮次 + 本轮实时思考；空闲/结束：只显示已固化的历史（避免重复）
  const items = useMemo(() => {
    const raw = busy ? [...historical, ...live] : historical;
    return raw
      .filter((t) => !isNoise(t))
      .map((t) => {
        const detail =
          t.detail || (t.kind === "phase" ? PHASE_DETAIL[t.label] ?? "" : "");
        return { ...t, detail: clamp(detail) };
      });
  }, [historical, live, busy]);

  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = ref.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [items.length]);

  return (
    <div className="panel-block thought">
      <div className="panel-head">
        思考流 <span className="tag">全会话</span>
      </div>
      <div className="thought-list" ref={ref}>
        {items.map((t) => (
          <div key={t.id ?? `${t.kind}-${t.ts}`} className={`thought-item ${t.kind}`}>
            <span className={`thought-kind ${t.kind}`} />
            <div className="thought-body">
              <div className="thought-label">{t.label}</div>
              {t.detail && <div className="thought-detail">{t.detail}</div>}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}