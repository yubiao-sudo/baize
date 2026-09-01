import { useCallback, useEffect, useRef, useState } from "react";
import {
  getImLog,
  getPendingPermissions,
  resolvePermission,
  scheduleListJobs,
  scheduleSetEnabled,
} from "../api";
import { useChat } from "../stores/chat";
import type { ImLogEntry, PermissionRequest, ScheduledJob } from "../types";

/** 毫秒时间戳 → 时分秒 */
function fmtTime(ms: number): string {
  if (!ms) return "—";
  const d = new Date(ms);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

type Tab = "approval" | "im" | "schedule";

/**
 * 消息中心：审批 / IM 消息 / 定时提醒三合一面板。
 * - 审批：待处理的高危操作审批与任务计划确认，行内直接同意/拒绝（与聊天卡片/IM 同一审批链）
 * - IM 消息：复用 IM 总线环形日志（手机发来的指令 + 白泽回传的审批/结果）
 * - 定时提醒：定时任务清单 + 启停开关
 */
export default function MessageCenterPanel({ onClose }: { onClose: () => void }) {
  const [tab, setTab] = useState<Tab>("approval");
  // 审批：以 store 的 pending 为准（事件实时驱动），挂载时拉一次兜底
  const pending = useChat((s) => s.pending);
  const removePending = useChat((s) => s.removePending);
  const [staleApprovals, setStaleApprovals] = useState<PermissionRequest[]>([]);
  // IM 日志
  const [imLogs, setImLogs] = useState<ImLogEntry[]>([]);
  const [imAuto, setImAuto] = useState(true);
  // 定时任务
  const [jobs, setJobs] = useState<ScheduledJob[]>([]);
  const busy = useRef(false);

  const loadApprovals = useCallback(async () => {
    try {
      const list = await getPendingPermissions();
      // 只显示 store 里没有的（store 由事件实时维护，这里兜底补漏）
      setStaleApprovals(list.filter((r) => !pending.some((p) => p.id === r.id)));
    } catch {
      /* 忽略 */
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pending]);

  const loadIm = useCallback(async () => {
    try {
      setImLogs(await getImLog());
    } catch {
      /* 忽略 */
    }
  }, []);

  const loadJobs = useCallback(async () => {
    try {
      setJobs(await scheduleListJobs());
    } catch {
      /* 忽略 */
    }
  }, []);

  useEffect(() => {
    if (tab === "approval") void loadApprovals();
    if (tab === "im") void loadIm();
    if (tab === "schedule") void loadJobs();
  }, [tab, loadApprovals, loadIm, loadJobs]);

  // IM 日志自动刷新
  useEffect(() => {
    if (tab !== "im" || !imAuto) return;
    const t = setInterval(() => void loadIm(), 3000);
    return () => clearInterval(t);
  }, [tab, imAuto, loadIm]);

  const decide = async (id: string, approved: boolean) => {
    await resolvePermission(id, approved, false);
    removePending(id);
    setStaleApprovals((s) => s.filter((r) => r.id !== id));
  };

  const toggleJob = async (job: ScheduledJob) => {
    if (busy.current) return;
    busy.current = true;
    try {
      await scheduleSetEnabled(job.id, !job.enabled);
      await loadJobs();
    } finally {
      busy.current = false;
    }
  };

  const allApprovals = [...pending, ...staleApprovals];

  return (
    <div className="rpanel">
      <div style={{ flex: 1, minHeight: 0, display: "flex", flexDirection: "column", overflow: "hidden" }}>
        {/* 头部 */}
        <div style={{ display: "flex", alignItems: "center", gap: 10, padding: "14px 18px", borderBottom: "1px solid var(--border-soft)" }}>
          <h3 style={{ margin: 0, fontSize: 15, letterSpacing: 1 }}>消息中心</h3>
          <span style={{ fontSize: 11, color: "var(--text-faint)" }}>
            审批 · IM 消息 · 定时提醒，一处处理
          </span>
          <span style={{ flex: 1 }} />
          <button className="side-btn" onClick={onClose} title="关闭">
            ✕
          </button>
        </div>

        {/* 页签 */}
        <div className="software-tabs" style={{ padding: "10px 18px 0" }}>
          <button className={`software-tab${tab === "approval" ? " on" : ""}`} onClick={() => setTab("approval")}>
            待审批{allApprovals.length > 0 ? ` (${allApprovals.length})` : ""}
          </button>
          <button className={`software-tab${tab === "im" ? " on" : ""}`} onClick={() => setTab("im")}>
            IM 消息
          </button>
          <button className={`software-tab${tab === "schedule" ? " on" : ""}`} onClick={() => setTab("schedule")}>
            定时提醒
          </button>
        </div>

        {/* 内容区 */}
        <div style={{ flex: 1, minHeight: 0, overflowY: "auto", padding: "12px 18px 18px" }}>
          {tab === "approval" && (
            <div className="software-pkg-list">
              {allApprovals.length === 0 && (
                <div className="mc-empty">没有待处理的审批。高危操作与任务计划的确认请求会出现在这里。</div>
              )}
              {allApprovals.map((req) => {
                const plan = (req.detail ?? {}) as { title?: string; steps?: string[] };
                return (
                  <div key={req.id} className="software-pkg">
                    <div className="software-pkg-info">
                      <div className="software-pkg-name">
                        {req.tool === "plan_confirm"
                          ? `任务计划：${plan.title || "未命名"}`
                          : `权限请求 · ${req.tool}`}
                      </div>
                      <div className="software-pkg-ver">
                        {req.tool === "plan_confirm"
                          ? (plan.steps || []).join(" → ")
                          : JSON.stringify(req.args).slice(0, 120)}
                      </div>
                    </div>
                    <span className="software-badge warn">{req.tool === "plan_confirm" ? "计划" : "审批"}</span>
                    <button className="software-action danger" onClick={() => void decide(req.id, false)}>
                      拒绝
                    </button>
                    <button className="software-action" onClick={() => void decide(req.id, true)}>
                      同意
                    </button>
                  </div>
                );
              })}
            </div>
          )}

          {tab === "im" && (
            <>
              <label style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 12, color: "var(--text-dim)", marginBottom: 10 }}>
                <input type="checkbox" checked={imAuto} onChange={(e) => setImAuto(e.target.checked)} />
                自动刷新（3 秒）
              </label>
              <div className="software-pkg-list">
                {imLogs.length === 0 && <div className="mc-empty">暂无 IM 收发记录。微信/飞书接入后，指令与回传会在这里流转。</div>}
                {[...imLogs].reverse().map((l, i) => (
                  <div key={`${l.ts}-${i}`} className="software-pkg">
                    <div className="software-pkg-info">
                      <div className="software-pkg-name">
                        <span className={`mc-dir ${l.direction === "in" ? "in" : "out"}`}>
                          {l.direction === "in" ? "↓ 收到" : "↑ 回传"}
                        </span>
                        {l.channel === "feishu" ? "飞书" : "微信"}
                        <span style={{ color: "var(--text-faint)", marginLeft: 8 }}>{fmtTime(l.ts)}</span>
                      </div>
                      <div className="software-pkg-ver">{(l.text || "").slice(0, 140)}</div>
                    </div>
                  </div>
                ))}
              </div>
            </>
          )}

          {tab === "schedule" && (
            <div className="software-pkg-list">
              {jobs.length === 0 && <div className="mc-empty">暂无定时任务。对白泽说「每天 X 点做 XX」即可创建。</div>}
              {jobs.map((j) => (
                <div key={j.id} className="software-pkg">
                  <div className="software-pkg-info">
                    <div className="software-pkg-name">{j.title}</div>
                    <div className="software-pkg-ver">
                      cron {j.cron_expr} · {j.task_type === "agent" ? "Agent 任务" : "命令"}
                      {j.last_run_at ? ` · 上次 ${fmtTime(j.last_run_at)}` : " · 未执行过"}
                      {j.last_result ? ` · ${j.last_result.slice(0, 40)}` : ""}
                    </div>
                  </div>
                  <span className={`software-badge ${j.enabled ? "ok" : "warn"}`}>{j.enabled ? "启用" : "停用"}</span>
                  <button className="software-action" onClick={() => void toggleJob(j)}>
                    {j.enabled ? "停用" : "启用"}
                  </button>
                </div>
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
