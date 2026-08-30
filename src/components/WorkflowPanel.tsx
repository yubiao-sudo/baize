import { useCallback, useEffect, useState } from "react";
import {
  workflowClearRuns,
  workflowDelete,
  workflowList,
  workflowRun,
  workflowRuns,
  workflowSave,
} from "../api";
import type { Workflow, WorkflowRun as WorkflowRunRow, WorkflowStage } from "../types";

type Tab = "workflows" | "runs";

const TABS: { id: Tab; label: string }[] = [
  { id: "workflows", label: "工作流列表" },
  { id: "runs", label: "执行日志" },
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

function statusLabel(s: string): string {
  if (s === "success") return "成功";
  if (s === "failed") return "失败";
  if (s === "running") return "运行中";
  return s;
}

const genId = () =>
  "wf_" + Date.now().toString(36) + Math.random().toString(36).slice(2, 6);

export default function WorkflowPanel({ onClose }: { onClose: () => void }) {
  const [tab, setTab] = useState<Tab>("workflows");
  const [workflows, setWorkflows] = useState<Workflow[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  // 新建 / 编辑表单
  const [formOpen, setFormOpen] = useState(false);
  const [editing, setEditing] = useState<Workflow | null>(null);
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [stages, setStages] = useState<WorkflowStage[]>([]);
  const [saving, setSaving] = useState(false);

  // 运行
  const [runFor, setRunFor] = useState<Workflow | null>(null);
  const [runInput, setRunInput] = useState("");
  const [running, setRunning] = useState(false);
  const [runOutput, setRunOutput] = useState("");

  // 执行日志
  const [runsFor, setRunsFor] = useState("");
  const [runs, setRuns] = useState<WorkflowRunRow[]>([]);
  const [logsBusy, setLogsBusy] = useState(false);

  const loadWorkflows = useCallback(async () => {
    setBusy(true);
    setError("");
    try {
      setWorkflows(await workflowList());
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, []);

  const loadRuns = useCallback(async (workflowId: string) => {
    setLogsBusy(true);
    try {
      setRuns(await workflowRuns(workflowId, 100));
    } catch (e) {
      setRuns([]);
    } finally {
      setLogsBusy(false);
    }
  }, []);

  useEffect(() => {
    void loadWorkflows();
  }, [loadWorkflows]);

  useEffect(() => {
    if (tab === "runs") void loadRuns(runsFor);
  }, [tab, runsFor, loadRuns]);

  const openCreate = () => {
    setEditing(null);
    setFormOpen(true);
    setName("");
    setDescription("");
    setStages([{ name: "阶段 1", prompt: "" }]);
  };

  const openEdit = (w: Workflow) => {
    setEditing(w);
    setFormOpen(true);
    setName(w.name);
    setDescription(w.description);
    setStages(w.stages.map((s) => ({ ...s })));
  };

  const closeForm = () => {
    setFormOpen(false);
    setEditing(null);
    setStages([]);
  };

  const save = async () => {
    if (!name.trim()) {
      setError("请填写工作流名称");
      return;
    }
    const valid = stages.filter((s) => s.name.trim() || s.prompt.trim());
    if (valid.length === 0) {
      setError("请至少配置一个阶段");
      return;
    }
    const wf: Workflow = {
      id: editing?.id ?? genId(),
      name: name.trim(),
      description: description.trim(),
      stages: valid.map((s) => ({
        name: s.name.trim() || "未命名阶段",
        prompt: s.prompt,
      })),
    };
    setSaving(true);
    setError("");
    try {
      await workflowSave(wf);
      closeForm();
      await loadWorkflows();
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  const remove = async (w: Workflow) => {
    if (!window.confirm(`确定删除工作流「${w.name}」及其执行日志？`)) return;
    try {
      await workflowDelete(w.id);
      await loadWorkflows();
    } catch (e) {
      setError(String(e));
    }
  };

  const openRun = (w: Workflow) => {
    setRunFor(w);
    setRunInput("");
    setRunOutput("");
  };

  const doRun = async () => {
    if (!runFor) return;
    if (!runInput.trim()) {
      setError("请输入工作流的输入内容");
      return;
    }
    setRunning(true);
    setError("");
    try {
      const out = await workflowRun(runFor.id, runInput);
      setRunOutput(out || "(工作流已执行，结果已写入右侧文档窗口)");
      await loadRuns(runsFor);
    } catch (e) {
      setError(String(e));
    } finally {
      setRunning(false);
    }
  };

  const clearRuns = async () => {
    if (!window.confirm(runsFor ? "确定清空该工作流的执行日志？" : "确定清空全部执行日志？")) return;
    try {
      await workflowClearRuns(runsFor);
      await loadRuns(runsFor);
    } catch (e) {
      setError(String(e));
    }
  };

  const showLogs = (w: Workflow) => {
    setRunsFor(w.id);
    setTab("runs");
  };

  const customCount = workflows.filter((w) => !w.id.startsWith("builtin")).length;

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
          <h3 style={{ margin: 0, fontSize: 15, letterSpacing: 1 }}>可编排工作流</h3>
          <span style={{ fontSize: 11, color: "var(--text-faint)" }}>
            {workflows.length} 个 · {customCount} 个自定义
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

          {tab === "workflows" && (
            <div className="software-col">
              {formOpen ? (
                <>
                  <div className="software-sec-title">{editing ? "编辑工作流" : "新建工作流"}</div>

                  <div className="software-row">
                    <span className="software-kv">名称</span>
                    <input
                      className="software-search-input"
                      style={{ flex: 1 }}
                      placeholder="如：周报生成器"
                      value={name}
                      onChange={(e) => setName(e.target.value)}
                    />
                  </div>

                  <div className="software-row">
                    <span className="software-kv">描述</span>
                    <input
                      className="software-search-input"
                      style={{ flex: 1 }}
                      placeholder="一句话说明这个工作流做什么"
                      value={description}
                      onChange={(e) => setDescription(e.target.value)}
                    />
                  </div>

                  <div className="software-sec-title" style={{ marginTop: 6 }}>
                    阶段（提示词模板，可用 {"{input}"} 引用上一阶段输出）
                  </div>
                  {stages.map((s, i) => (
                    <div
                      key={i}
                      style={{
                        border: "1px solid var(--border-soft)",
                        borderRadius: 8,
                        padding: 8,
                        marginBottom: 8,
                        display: "flex",
                        flexDirection: "column",
                        gap: 6,
                      }}
                    >
                      <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                        <span style={{ fontSize: 11, color: "var(--text-faint)" }}>阶段 {i + 1}</span>
                        <input
                          className="software-search-input"
                          style={{ flex: 1 }}
                          placeholder="阶段名（如：需求分析）"
                          value={s.name}
                          onChange={(e) =>
                            setStages(stages.map((x, j) => (j === i ? { ...x, name: e.target.value } : x)))
                          }
                        />
                        <button
                          className="software-action"
                          onClick={() => setStages(stages.filter((_, j) => j !== i))}
                          disabled={stages.length <= 1}
                        >
                          移除
                        </button>
                      </div>
                      <textarea
                        className="software-search-input"
                        style={{ minHeight: 70, resize: "vertical", fontFamily: "monospace" }}
                        placeholder={"该阶段的提示词，例如：请把下面的内容做成要点清单：\\n{input}"}
                        value={s.prompt}
                        onChange={(e) =>
                          setStages(stages.map((x, j) => (j === i ? { ...x, prompt: e.target.value } : x)))
                        }
                      />
                    </div>
                  ))}
                  <button
                    className="software-action"
                    onClick={() => setStages([...stages, { name: `阶段 ${stages.length + 1}`, prompt: "" }])}
                  >
                    ＋ 添加阶段
                  </button>

                  <div style={{ display: "flex", gap: 8, marginTop: 12 }}>
                    <button className="software-primary" onClick={save} disabled={saving}>
                      {saving ? "保存中…" : "保存"}
                    </button>
                    <button className="software-refresh" onClick={closeForm}>
                      取消
                    </button>
                  </div>
                </>
              ) : runFor ? (
                <>
                  <div className="software-sec-title">
                    运行「{runFor.name}」（{runFor.stages.length} 个阶段）
                  </div>
                  <div className="software-row">
                    <span className="software-kv">输入</span>
                    <textarea
                      className="software-search-input"
                      style={{ flex: 1, minHeight: 100, resize: "vertical" }}
                      placeholder="工作流要处理的输入内容（文本，或本地文件路径）"
                      value={runInput}
                      onChange={(e) => setRunInput(e.target.value)}
                    />
                  </div>
                  <div style={{ display: "flex", gap: 8, marginTop: 10 }}>
                    <button className="software-primary" onClick={doRun} disabled={running}>
                      {running ? "执行中…" : "执行"}
                    </button>
                    <button className="software-refresh" onClick={() => setRunFor(null)}>
                      返回
                    </button>
                  </div>
                  {runOutput && (
                    <pre
                      style={{
                        margin: "12px 0 0",
                        padding: "10px",
                        background: "rgba(0,0,0,0.25)",
                        borderRadius: 6,
                        fontSize: 11,
                        maxHeight: "48vh",
                        overflow: "auto",
                        whiteSpace: "pre-wrap",
                        wordBreak: "break-all",
                        color: "var(--text-dim)",
                      }}
                    >
                      {runOutput}
                    </pre>
                  )}
                </>
              ) : (
                <>
                  <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 8 }}>
                    <button className="software-primary" onClick={openCreate}>
                      ＋ 新建工作流
                    </button>
                    <span style={{ fontSize: 11, color: "var(--text-faint)" }}>
                      多阶段流水线，逐阶段用强模型执行，前阶段输出传给下一阶段
                    </span>
                  </div>

                  {busy ? (
                    <div style={{ color: "var(--text-dim)" }}>加载中…</div>
                  ) : workflows.length === 0 ? (
                    <div style={{ color: "var(--text-faint)" }}>
                      还没有工作流。点击「新建工作流」创建一个。
                    </div>
                  ) : (
                    <div className="software-pkg-list">
                      {workflows.map((w) => {
                        const builtin = w.id === "summary_report" || w.id === "write_spec";
                        return (
                          <div
                            className="software-pkg"
                            key={w.id}
                            style={{ flexDirection: "column", alignItems: "stretch" }}
                          >
                            <div style={{ display: "flex", alignItems: "center", gap: 8, width: "100%" }}>
                              <span className="software-pkg-name" style={{ flex: 1 }}>
                                {w.name}
                              </span>
                              <span className="software-pkg-ver">{w.stages.length} 阶段</span>
                              <span className="software-badge" style={{ fontSize: 10 }}>
                                {builtin ? "内置" : "自定义"}
                              </span>
                            </div>
                            {w.description && (
                              <div style={{ fontSize: 11, color: "var(--text-faint)", marginTop: 4 }}>
                                {w.description}
                              </div>
                            )}
                            <div style={{ display: "flex", gap: 6, marginTop: 6, flexWrap: "wrap" }}>
                              <button className="software-action" onClick={() => openRun(w)}>
                                运行
                              </button>
                              {!builtin && (
                                <button className="software-action" onClick={() => openEdit(w)}>
                                  编辑
                                </button>
                              )}
                              <button className="software-action" onClick={() => showLogs(w)}>
                                日志
                              </button>
                              {!builtin && (
                                <button className="software-action" onClick={() => remove(w)}>
                                  删除
                                </button>
                              )}
                            </div>
                          </div>
                        );
                      })}
                    </div>
                  )}
                </>
              )}
            </div>
          )}

          {tab === "runs" && (
            <div className="software-col">
              <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 8 }}>
                <select
                  className="mode-select"
                  value={runsFor}
                  onChange={(e) => setRunsFor(e.target.value)}
                >
                  <option value="">全部工作流</option>
                  {workflows.map((w) => (
                    <option key={w.id} value={w.id}>
                      {w.name}
                    </option>
                  ))}
                </select>
                <span style={{ flex: 1 }} />
                <button className="software-action" onClick={clearRuns}>
                  清空日志
                </button>
              </div>

              {logsBusy ? (
                <div style={{ color: "var(--text-dim)" }}>加载中…</div>
              ) : runs.length === 0 ? (
                <div style={{ color: "var(--text-faint)" }}>暂无执行记录。</div>
              ) : (
                <div className="software-pkg-list">
                  {runs.map((r) => (
                    <div
                      className="software-pkg"
                      key={r.id}
                      style={{ flexDirection: "column", alignItems: "stretch" }}
                    >
                      <div style={{ display: "flex", alignItems: "center", gap: 8, width: "100%" }}>
                        <span
                          className={`software-badge ${
                            r.status === "success" ? "ok" : r.status === "failed" ? "warn" : ""
                          }`}
                        >
                          {statusLabel(r.status)}
                        </span>
                        {!runsFor && (
                          <span className="software-pkg-name" style={{ flex: 1 }}>
                            {r.workflow_name}
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
          )}
        </div>
      </div>
    </div>
  );
}