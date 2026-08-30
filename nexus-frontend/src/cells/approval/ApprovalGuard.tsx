// ==========================================================================
// 审批联锁卡 · ApprovalGuard
//   listens:  approval.arrive / approval.resolved
//   emits:    cmd.approval.resolve
// ==========================================================================
import { useState } from "react";
import { PrismCell } from "../PrismCell";
import { useCell } from "../cell.hooks";
import type { ApprovalItem } from "../../bus/prism.types";

export function ApprovalGuard() {
  const [queue, setQueue] = useState<ApprovalItem[]>([]);
  const [history, setHistory] = useState<{ id: string; allow: boolean; ts: number }[]>([]);

  useCell(
    { name: "审批·联锁", category: "approval", emits: ["cmd.approval.resolve"] },
    {
      "approval.arrive": (env) => {
        const p = env.payload as ApprovalItem;
        setQueue((prev) => (prev.some((x) => x.id === p.id) ? prev : [...prev, p]));
      },
      "approval.resolved": (env) => {
        const p = env.payload as { id: string; allow: boolean };
        setQueue((prev) => prev.filter((x) => x.id !== p.id));
        setHistory((h) => [{ id: p.id, allow: p.allow, ts: Date.now() }, ...h].slice(0, 8));
      },
    }
  );

  const head = queue[0];

  return (
    <PrismCell
      title="审批·联锁"
      subtitle="Approval Guard · Interlock"
      tools={
        queue.length > 0 ? (
          <span className="nexus-chip bad">PENDING · {queue.length}</span>
        ) : (
          <span className="nexus-chip ok">全部放行</span>
        )
      }
    >
      {head ? (
        <div className={`approval-card ${head.cls}`}>
          <span className="tag-cls">{head.cls}</span>
          <h4>
            <code style={{ background: "rgba(255,255,255,0.06)", padding: "1px 6px", borderRadius: 4, fontWeight: 500 }}>
              {head.tool}
            </code>{" "}
            {head.description ?? "请求执行工具"}
          </h4>
          <pre>{JSON.stringify(head.args, null, 2)}</pre>
          <div className="approval-actions">
            <button
              className="prism-btn block"
              onClick={() =>
                queue.forEach((q) =>
                  prismBusEmit("cmd.approval.resolve", { id: q.id, allow: true, reason: "批量允许" })
                )
              }
            >
              ✓ 全部允许
            </button>
            <button
              className="prism-btn"
              onClick={() => prismBusEmit("cmd.approval.resolve", { id: head.id, allow: true, reason: "单次允许" })}
            >
              允许本次
            </button>
            <button
              className="prism-btn danger"
              onClick={() => prismBusEmit("cmd.approval.resolve", { id: head.id, allow: false, reason: "用户拒绝" })}
            >
              ✕ 拒绝
            </button>
          </div>
        </div>
      ) : (
        <div style={{ color: "var(--prism-ink-3)", fontSize: 12.5, textAlign: "center", padding: "14px 0" }}>
          当前没有待审批操作。发送"写代码"相关指令会触发一次模拟审批演示。
        </div>
      )}

      {history.length > 0 ? (
        <div style={{ marginTop: 12, borderTop: "1px dashed var(--prism-cell-edge)", paddingTop: 12 }}>
          <div style={{ fontSize: 11, color: "var(--prism-ink-3)", marginBottom: 6, letterSpacing: "0.1em", textTransform: "uppercase" }}>
            最近处理
          </div>
          {history.map((h) => (
            <div
              key={h.id}
              style={{
                display: "flex", justifyContent: "space-between",
                padding: "5px 8px", fontSize: 11.5, color: "var(--prism-ink-2)",
                borderBottom: "1px dashed color-mix(in srgb, var(--prism-cell-edge) 40%, transparent)",
              }}
            >
              <span style={{ fontFamily: "ui-monospace, monospace" }}>{h.id.slice(-8)}</span>
              <span style={{ color: h.allow ? "var(--prism-success)" : "var(--prism-danger)" }}>
                {h.allow ? "✓ ALLOW" : "✕ DENY"}
              </span>
            </div>
          ))}
        </div>
      ) : null}
    </PrismCell>
  );
}

// 小型辅助：Cell 外发射 Bus 信号（审批按钮的批量操作场景用）
import { prismBus } from "../../bus/prism.bus";
function prismBusEmit(kind: string, payload: unknown, opts?: object) {
  prismBus.emit(kind, payload as object, { source: "approval.guard.ui", ...(opts as object) });
}