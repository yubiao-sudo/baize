import { useCallback, useEffect, useState } from "react";
import {
  scheduleAddJob,
  scheduleClearLogs,
  scheduleDeleteJob,
  scheduleJobLogs,
  scheduleListJobs,
  scheduleSetEnabled,
  scheduleUpdateJob,
} from "../api";
import type { JobRun, ScheduledJob } from "../types";

type Tab = "jobs" | "logs";

const PRESETS: { label: string; expr: string }[] = [
  { label: "每小时", expr: "0 * * * *" },
  { label: "每天 9 点", expr: "0 9 * * *" },
  { label: "每 30 分钟", expr: "*/30 * * * *" },
  { label: "每周一 9 点", expr: "0 9 * * 1" },
  { label: "每月 1 日 0 点", expr: "0 0 1 * *" },
];

const TABS: { id: Tab; label: string }[] = [
  { id: "jobs", label: "任务列表" },
  { id: "logs", label: "执行日志" },
];

/** 毫秒时间戳 → 本地可读格式 */
function fmtTime(ms: number | null): string {
  if (!ms) return "—";
  const d = new Date(ms);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(
    d.getMinutes()
  )}`;
}

function typeLabel(t: string): string {
  return t === "agent" ? "AI 任务" : "命令";
}

function statusLabel(s: string): string {
  if (s === "success") return "成功";
  if (s === "failed") return "失败";
  if (s === "running") return "运行中";
  return s;
}

export default function SchedulePanel({ onClose }: { onClose: () => void }) {
  const [tab, setTab] = useState<Tab>("jobs");
  const [jobs, setJobs] = useState<ScheduledJob[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  // 新建 / 编辑表单
  const [editing, setEditing] = useState<ScheduledJob | null>(null); // null 且 formOpen=true 表示新建
  const [formOpen, setFormOpen] = useState(false);
  const [cronExpr, setCronExpr] = useState("0 9 * * *");
  const [title, setTitle] = useState("");
  const [taskType, setTaskType] = useState<"agent" | "command">("agent");
  const [task, setTask] = useState("");
  const [saving, setSaving] = useState(false);

  // 执行日志
  const [logsFor, setLogsFor] = useState<string>(""); // 空 = 全部
  const [logs, setLogs] = useState<JobRun[]>([]);
  const [logsBusy, setLogsBusy] = useState(false);

  const loadJobs = useCallback(async () => {
    setBusy(true);
    setError("");
    try {
      setJobs(await scheduleListJobs());
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, []);

  const loadLogs = useCallback(async (jobId: string) => {
    setLogsBusy(true);
    try {
      setLogs(await scheduleJobLogs(jobId, 100));
    } catch (e) {
      setLogs([]);
    } finally {
      setLogsBusy(false);
    }
  }, []);

  useEffect(() => {
    void loadJobs();
  }, [loadJobs]);

  // 切到日志页时加载（logsFor 变化也重新加载）
  useEffect(() => {
    if (tab === "logs") void loadLogs(logsFor);
  }, [tab, logsFor, loadLogs]);

  const openCreate = () => {
    setEditing(null);
    setFormOpen(true);
    setCronExpr("0 9 * * *");
    setTitle("");
    setTaskType("agent");
    setTask("");
  };

  const openEdit = (j: ScheduledJob) => {
    setEditing(j);
    setFormOpen(true);
    setCronExpr(j.cron_expr);
    setTitle(j.title);
    setTaskType(j.task_type === "command" ? "command" : "agent");
    setTask(j.command);
  };

  const closeForm = () => {
    setFormOpen(false);
    setEditing(null);
  };

  const save = async () => {
    if (!cronExpr.trim() || !task.trim()) {
      setError("cron 表达式和任务内容不能为空");
      return;
    }
    setSaving(true);
    setError("");
    try {
      if (editing) {
        await scheduleUpdateJob(editing.id, cronExpr.trim(), title.trim(), taskType, task);
      } else {
        await scheduleAddJob(cronExpr.trim(), title.trim(), taskType, task);
      }
      closeForm();
      await loadJobs();
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  const toggle = async (j: ScheduledJob) => {
    try {
      await scheduleSetEnabled(j.id, !j.enabled);
      await loadJobs();
    } catch (e) {
      setError(String(e));
    }
  };

  const remove = async (j: ScheduledJob) => {
    if (!window.confirm(`确定删除任务「${j.title || j.command}」及其执行日志？`)) return;
    try {
      await scheduleDeleteJob(j.id);
      await loadJobs();
    } catch (e) {
      setError(String(e));
    }
  };

  const showLogs = (j: ScheduledJob) => {
    setLogsFor(j.id);
    setTab("logs");
  };

  const clearLogs = async () => {
    if (!window.confirm(logsFor ? "确定清空该任务的执行日志？" : "确定清空全部执行日志？")) return;
    try {
      await scheduleClearLogs(logsFor);
      await loadLogs(logsFor);
    } catch (e) {
      setError(String(e));
    }
  };

  const enabledCount = jobs.filter((j) => j.enabled).length;

  return (
    <div className="rpanel">
      <div
        style={{
          flex: 1,
          minHeight: 0,
          display: "flex",
          flexDirection: "column",
          overflow: "hidden",
        }}
      >
        {/* 头部 */}
        <div
          style={{
            display: "flex",
            alignItems: "center",
            gap: 10,
            padding: "14px 18px",
            borderBottom: "1px solid var(--border-soft)",
          }}
        >
          <h3 style={{ margin: 0, fontSize: 15, letterSpacing: 1 }}>定时任务</h3>
          <span style={{ fontSize: 11, color: "var(--text-faint)" }}>
            {enabledCount} 个启用 · 周期自动执行
          </span>
          <span style={{ flex: 1 }} />
          <button className="software-close" onClick={onClose} title="关闭">
            ×
          </button>
        </div>

        {/* 标签栏 */}
        <div className="software-tabs">
          {TABS.map((t) => (
            <button
              key={t.id}
              className={`software-tab ${tab === t.id ? "active" : ""}`}
              onClick={() => setTab(t.id)}
            >
              {t.label}
            </button>
          ))}
        </div>

        {/* 主体 */}
        <div className="software-body">
          {error && <div className="software-error">{error}</div>}

          {tab === "jobs" && (
            <JobsView
              jobs={jobs}
              busy={busy}
              editing={editing}
              formOpen={formOpen}
              cronExpr={cronExpr}
              setCronExpr={setCronExpr}
              title={title}
              setTitle={setTitle}
              taskType={taskType}
              setTaskType={setTaskType}
              task={task}
              setTask={setTask}
              saving={saving}
              onCreate={openCreate}
              onEdit={openEdit}
              onSave={save}
              onCancel={closeForm}
              onToggle={toggle}
              onRemove={remove}
              onLogs={showLogs}
            />
          )}

          {tab === "logs" && (
            <LogsView
              logs={logs}
              busy={logsBusy}
              logsFor={logsFor}
              onAll={() => setLogsFor("")}
              onClear={clearLogs}
            />
          )}
        </div>
      </div>
    </div>
  );
}

// ─────────── 任务列表 ───────────
function JobsView({
  jobs,
  busy,
  editing,
  formOpen,
  cronExpr,
  setCronExpr,
  title,
  setTitle,
  taskType,
  setTaskType,
  task,
  setTask,
  saving,
  onCreate,
  onEdit,
  onSave,
  onCancel,
  onToggle,
  onRemove,
  onLogs,
}: {
  jobs: ScheduledJob[];
  busy: boolean;
  editing: ScheduledJob | null;
  formOpen: boolean;
  cronExpr: string;
  setCronExpr: (s: string) => void;
  title: string;
  setTitle: (s: string) => void;
  taskType: "agent" | "command";
  setTaskType: (s: "agent" | "command") => void;
  task: string;
  setTask: (s: string) => void;
  saving: boolean;
  onCreate: () => void;
  onEdit: (j: ScheduledJob) => void;
  onSave: () => void;
  onCancel: () => void;
  onToggle: (j: ScheduledJob) => void;
  onRemove: (j: ScheduledJob) => void;
  onLogs: (j: ScheduledJob) => void;
}) {
  if (formOpen) {
    return (
      <div className="software-col">
        <div className="software-sec-title">{editing ? "编辑任务" : "新建任务"}</div>

        <div className="software-row">
          <span className="software-kv">任务类型</span>
          <select
            className="mode-select"
            value={taskType}
            onChange={(e) => setTaskType(e.target.value as "agent" | "command")}
          >
            <option value="agent">AI 任务（自然语言，交给白泽 Agent）</option>
            <option value="command">命令（PowerShell 直连）</option>
          </select>
        </div>

        <div className="software-row">
          <span className="software-kv">标题（可选）</span>
          <input
            className="software-search-input"
            style={{ flex: 1 }}
            placeholder="如：每天自动备份"
            value={title}
            onChange={(e) => setTitle(e.target.value)}
          />
        </div>

        <div className="software-row">
          <span className="software-kv">cron 表达式</span>
          <input
            className="software-search-input"
            style={{ flex: 1, fontFamily: "monospace" }}
            placeholder="分 时 日 月 周，如 0 9 * * *"
            value={cronExpr}
            onChange={(e) => setCronExpr(e.target.value)}
          />
        </div>
        <div style={{ display: "flex", gap: 6, flexWrap: "wrap", marginTop: -4 }}>
          {PRESETS.map((p) => (
            <button
              key={p.expr}
              className="software-action"
              onClick={() => setCronExpr(p.expr)}
              title={p.expr}
            >
              {p.label}
            </button>
          ))}
        </div>

        <div className="software-sec-title" style={{ marginTop: 8 }}>
          {taskType === "agent" ? "任务内容（自然语言描述）" : "PowerShell 命令"}
        </div>
        <textarea
          className="software-search-input"
          style={{ minHeight: 90, resize: "vertical", fontFamily: "monospace" }}
          placeholder={
            taskType === "agent"
              ? "如：备份 C:\\Users\\OMEN\\Documents 到 D 盘并验证完整性"
              : "如：Remove-Item $env:TEMP\\* -Recurse -Force"
          }
          value={task}
          onChange={(e) => setTask(e.target.value)}
        />

        <div style={{ display: "flex", gap: 8, marginTop: 10 }}>
          <button className="software-primary" onClick={onSave} disabled={saving}>
            {saving ? "保存中…" : "保存"}
          </button>
          <button className="software-refresh" onClick={onCancel}>
            取消
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="software-col">
      <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 8 }}>
        <button className="software-primary" onClick={onCreate}>
          ＋ 新建任务
        </button>
        <span style={{ fontSize: 11, color: "var(--text-faint)" }}>
          支持周期命令与 AI 自然语言任务，到点自动执行
        </span>
      </div>

      {busy ? (
        <div style={{ color: "var(--text-dim)" }}>加载中…</div>
      ) : jobs.length === 0 ? (
        <div style={{ color: "var(--text-faint)" }}>
          还没有定时任务。点击「新建任务」创建一个，例如每天 9 点自动备份。
        </div>
      ) : (
        <div className="software-pkg-list">
          {jobs.map((j) => (
            <div className="software-pkg" key={j.id} style={{ flexDirection: "column", alignItems: "stretch" }}>
              <div style={{ display: "flex", alignItems: "center", gap: 8, width: "100%" }}>
                <button
                  className="software-action"
                  style={{
                    background: j.enabled ? "var(--cyan)" : "transparent",
                    color: j.enabled ? "#000" : "var(--text-dim)",
                    minWidth: 52,
                  }}
                  onClick={() => onToggle(j)}
                  title={j.enabled ? "暂停任务" : "恢复任务"}
                >
                  {j.enabled ? "启用中" : "已暂停"}
                </button>
                <span className="software-pkg-name" title={j.command} style={{ flex: 1 }}>
                  {j.title || j.command}
                </span>
                <span className="software-pkg-ver" title={j.cron_expr}>
                  {j.cron_expr}
                </span>
                <span className="software-badge" style={{ fontSize: 10 }}>
                  {typeLabel(j.task_type)}
                </span>
              </div>

              <div style={{ display: "flex", gap: 6, marginTop: 6, flexWrap: "wrap" }}>
                <button className="software-action" onClick={() => onEdit(j)}>
                  编辑
                </button>
                <button className="software-action" onClick={() => onLogs(j)}>
                  日志
                </button>
                <button className="software-action" onClick={() => onRemove(j)}>
                  删除
                </button>
                <span style={{ flex: 1 }} />
                <span style={{ fontSize: 10, color: "var(--text-faint)" }} title={j.last_result ?? ""}>
                  上次 {fmtTime(j.last_run_at)}
                </span>
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

// ─────────── 执行日志 ───────────
function LogsView({
  logs,
  busy,
  logsFor,
  onAll,
  onClear,
}: {
  logs: JobRun[];
  busy: boolean;
  logsFor: string;
  onAll: () => void;
  onClear: () => void;
}) {
  return (
    <div className="software-col">
      <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 8 }}>
        {logsFor ? (
          <>
            <span className="software-pkg-name">{logs[0]?.job_title || "任务"}</span>
            <button className="software-action" onClick={onAll}>
              ← 全部
            </button>
          </>
        ) : (
          <span style={{ fontSize: 12, color: "var(--text-faint)" }}>全部任务（最近 100 条）</span>
        )}
        <span style={{ flex: 1 }} />
        <button className="software-action" onClick={onClear}>
          清空日志
        </button>
      </div>

      {busy ? (
        <div style={{ color: "var(--text-dim)" }}>加载中…</div>
      ) : logs.length === 0 ? (
        <div style={{ color: "var(--text-faint)" }}>暂无执行记录。</div>
      ) : (
        <div className="software-pkg-list">
          {logs.map((r) => (
            <div className="software-pkg" key={r.id} style={{ flexDirection: "column", alignItems: "stretch" }}>
              <div style={{ display: "flex", alignItems: "center", gap: 8, width: "100%" }}>
                <span
                  className={`software-badge ${
                    r.status === "success" ? "ok" : r.status === "failed" ? "warn" : ""
                  }`}
                >
                  {statusLabel(r.status)}
                </span>
                {!logsFor && (
                  <span className="software-pkg-name" style={{ flex: 1 }} title={r.job_title}>
                    {r.job_title}
                  </span>
                )}
                <span className="software-pkg-ver">{fmtTime(r.started_at)}</span>
                {r.finished_at && (
                  <span style={{ fontSize: 10, color: "var(--text-faint)" }}>
                    {Math.max(0, Math.round((r.finished_at - r.started_at) / 1000))}s
                  </span>
                )}
              </div>
              {r.result && (
                <pre
                  style={{
                    margin: "6px 0 0",
                    padding: "8px",
                    background: "rgba(0,0,0,0.25)",
                    borderRadius: 6,
                    fontSize: 11,
                    maxHeight: 180,
                    overflow: "auto",
                    whiteSpace: "pre-wrap",
                    wordBreak: "break-all",
                    color: "var(--text-dim)",
                  }}
                >
                  {r.result}
                </pre>
              )}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}