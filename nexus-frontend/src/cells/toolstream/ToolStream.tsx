// ==========================================================================
// 工具帧时间线 · ToolStream
//   listens:  tool.frame
//   emits:    —（只读型 Cell）
// 特点：按 frameId 做 in-place 合并，收到 running 再收到 ok 时不产生新行
// ==========================================================================
import { useMemo, useState } from "react";
import { PrismCell } from "../PrismCell";
import { useCell } from "../cell.hooks";
import type { ToolFrame } from "../../bus/prism.types";

export function ToolStream() {
  const [frames, setFrames] = useState<Record<string, ToolFrame & { convId?: string }>>({});
  const [filter, setFilter] = useState<"all" | ToolFrame["status"]>("all");

  useCell(
    {
      name: "工具·时间线",
      category: "toolstream",
    },
    {
      "tool.frame": (env) => {
        const p = env.payload as ToolFrame & { convId?: string };
        if (!p.frameId) return;
        setFrames((prev) => ({
          ...prev,
          [p.frameId]: { ...(prev[p.frameId] ?? {}), ...p },
        }));
      },
    }
  );

  const list = useMemo(() => {
    const arr = Object.values(frames).sort((a, b) => (a.startedAt ?? 0) - (b.startedAt ?? 0));
    return filter === "all" ? arr : arr.filter((f) => f.status === filter);
  }, [frames, filter]);

  const stats = useMemo(() => {
    const all = Object.values(frames);
    return {
      all: all.length,
      running: all.filter((f) => f.status === "running").length,
      ok: all.filter((f) => f.status === "ok").length,
      fail: all.filter((f) => f.status === "fail").length,
    };
  }, [frames]);

  function summarize(args: unknown): string {
    try {
      const s = typeof args === "string" ? args : JSON.stringify(args ?? {});
      return s.length > 80 ? s.slice(0, 80) + "…" : s;
    } catch {
      return String(args);
    }
  }

  return (
    <PrismCell
      title="工具·时间线"
      subtitle="Tool Trace · Wave Signal"
      tools={
        <div style={{ display: "flex", gap: 4 }}>
          {(["all", "running", "ok", "fail"] as const).map((k) => (
            <button
              key={k}
              onClick={() => setFilter(k)}
              className={`prism-btn ${filter === k ? "" : "ghost"}`}
              style={{ padding: "3px 10px", fontSize: 11 }}
            >
              {k === "all" ? `全部 ${stats.all}` :
               k === "running" ? `进行中 ${stats.running}` :
               k === "ok"    ? `成功 ${stats.ok}`    :
                               `失败 ${stats.fail}`}
            </button>
          ))}
        </div>
      }
    >
      {list.length === 0 ? (
        <div style={{ color: "var(--prism-ink-3)", fontSize: 12.5, textAlign: "center", padding: "14px 0" }}>
          暂无工具帧。发送一条提到"计划/代码"的消息试试，这里会出现模拟工具调用记录。
        </div>
      ) : (
        <div className="tool-timeline">
          {list.map((f) => (
            <div key={f.frameId} className={`tool-frame ${f.status}`}>
              <div className="t-dot" />
              <div>
                <div className="t-name">{f.tool}</div>
                <div className="t-args">{summarize(f.args)}</div>
                {f.resultSnip ? <div className="t-result">→ {f.resultSnip}</div> : null}
              </div>
              <div className="t-status">
                {f.status === "running" && f.startedAt
                  ? `${((performance.now() - f.startedAt) / 1000).toFixed(1)}s`
                  : f.status.toUpperCase()}
              </div>
            </div>
          ))}
        </div>
      )}
    </PrismCell>
  );
}