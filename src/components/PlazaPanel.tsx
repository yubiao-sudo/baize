import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  onPermissionRequest,
  plazaDeleteItem,
  plazaList,
  plazaMarketCatalog,
  plazaMarketInstall,
  plazaRun,
  plazaSaveItem,
  resolvePermission,
} from "../api";
import { useChat } from "../stores/chat";
import type { DiyToolSpec, PermissionRequest, PlazaItem } from "../types";

const KIND_LABEL: Record<string, string> = {
  tool: "工具",
  workflow: "工作流",
  skill: "技能",
};

const SOURCE_LABEL: Record<string, string> = {
  builtin: "内置",
  diy: "自研",
  market: "市场",
};

const TRUST_LABEL: Record<string, string> = {
  trusted: "可信",
  authored: "白泽自研",
  untrusted: "未受信",
};

const OUTPUT_LABEL: Record<string, string> = {
  document: "文档",
  terminal: "终端",
  browser: "浏览器",
  execution_flow: "执行流",
  notification: "通知",
  todo: "待办",
  clipboard: "剪贴板",
};
const ALL_OUTPUTS = Object.keys(OUTPUT_LABEL);

const genId = () =>
  "diy_" + Date.now().toString(36) + Math.random().toString(36).slice(2, 6);

const DEFAULT_SCHEMA = '{"type":"object","properties":{}}';

function trustClass(trust: string): string {
  if (trust === "trusted") return "ok";
  if (trust === "untrusted") return "warn";
  return "";
}

interface ParamField {
  key: string;
  type: string;
  desc: string;
  required: boolean;
}

/** 从工具入参 JSON Schema 提取可填参数项（用于动态生成表单） */
function paramFields(schema: unknown): ParamField[] {
  if (!schema || typeof schema !== "object") return [];
  const s = schema as {
    properties?: Record<string, { type?: string; description?: string }>;
    required?: string[];
  };
  const props = (s.properties ?? {}) as Record<
    string,
    { type?: string; description?: string }
  >;
  const required = new Set(s.required ?? []);
  return Object.entries(props).map(([key, def]) => ({
    key,
    type: def?.type ?? "string",
    desc: (def?.description ?? "").toString(),
    required: required.has(key),
  }));
}

export default function PlazaPanel({ onClose }: { onClose: () => void }) {
  const [items, setItems] = useState<PlazaItem[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  // 浏览 / 筛选
  const [query, setQuery] = useState("");
  const [kindFilter, setKindFilter] = useState("");
  const [sourceFilter, setSourceFilter] = useState("");

  // 自研工具表单
  const [formOpen, setFormOpen] = useState(false);
  const [editing, setEditing] = useState<PlazaItem | null>(null);
  const [fName, setFName] = useState("");
  const [fDesc, setFDesc] = useState("");
  const [fCategory, setFCategory] = useState("通用");
  const [fMode, setFMode] = useState<"command" | "script">("command");
  const [fCommand, setFCommand] = useState("");
  const [fLang, setFLang] = useState("python");
  const [fCode, setFCode] = useState("");
  const [fOutputs, setFOutputs] = useState<string[]>([]);
  const [fSchema, setFSchema] = useState(DEFAULT_SCHEMA);
  const [saving, setSaving] = useState(false);

  // 运行
  const [runFor, setRunFor] = useState<PlazaItem | null>(null);
  const [runArgs, setRunArgs] = useState("{}");
  const [runFormVals, setRunFormVals] = useState<Record<string, string>>({});
  const [runJsonMode, setRunJsonMode] = useState(false);
  const [running, setRunning] = useState(false);
  const [runOutput, setRunOutput] = useState("");
  // 运行未受信工具时的审批弹窗（覆盖在广场之上，解决原卡片被压在底部无法点击的问题）
  const [approval, setApproval] = useState<PermissionRequest | null>(null);
  const approvalRef = useRef<PermissionRequest | null>(null);
  const removePending = useChat((s) => s.removePending);

  // 市场仓库
  const [marketOpen, setMarketOpen] = useState(false);
  const [marketItems, setMarketItems] = useState<PlazaItem[]>([]);
  const [marketBusy, setMarketBusy] = useState(false);
  const [installingId, setInstallingId] = useState("");

  const load = useCallback(async () => {
    setBusy(true);
    setError("");
    try {
      // 内置工具（builtin tool）暂不展示，后续按进度接入；工作流/技能/自研工具正常显示
      const all = await plazaList();
      setItems(all.filter((it) => !(it.kind === "tool" && it.source === "builtin")));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  // 订阅后端审批请求：运行未受信（市场）工具时，把授权确认渲染在广场内部，
  // 而不是落在被广场遮罩压住的底部卡片上。
  useEffect(() => {
    let disposed = false;
    let unlisten: () => void = () => {};
    onPermissionRequest((req) => {
      if (disposed) return;
      approvalRef.current = req;
      setApproval(req);
    }).then((fn) => {
      if (!disposed) unlisten = fn;
    });
    return () => {
      disposed = true;
      unlisten();
    };
  }, []);

  const decideApproval = async (ok: boolean) => {
    const req = approvalRef.current;
    if (!req) return;
    approvalRef.current = null;
    setApproval(null);
    await resolvePermission(req.id, ok, false);
    removePending(req.id);
  };

  const loadMarket = useCallback(async () => {
    setMarketBusy(true);
    setError("");
    try {
      setMarketItems(await plazaMarketCatalog());
    } catch (e) {
      setError(String(e));
    } finally {
      setMarketBusy(false);
    }
  }, []);

  const installFromMarket = async (it: PlazaItem) => {
    setInstallingId(it.id);
    setError("");
    try {
      await plazaMarketInstall(it.id);
      await load(); // 刷新广场列表（安装后出现在“自研/市场”来源中）
      await loadMarket(); // 刷新“已安装”状态
    } catch (e) {
      setError(String(e));
    } finally {
      setInstallingId("");
    }
  };

  const installedNames = useMemo(
    () => new Set(items.map((i) => i.name)),
    [items]
  );

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return items.filter((it) => {
      if (kindFilter && it.kind !== kindFilter) return false;
      if (sourceFilter && it.source !== sourceFilter) return false;
      if (!q) return true;
      return (
        it.name.toLowerCase().includes(q) ||
        it.description.toLowerCase().includes(q) ||
        it.category.toLowerCase().includes(q) ||
        it.tags.some((t) => t.toLowerCase().includes(q))
      );
    });
  }, [items, query, kindFilter, sourceFilter]);

  const openCreate = () => {
    setEditing(null);
    setFormOpen(true);
    setFName("");
    setFDesc("");
    setFCategory("通用");
    setFMode("command");
    setFCommand("");
    setFLang("python");
    setFCode("");
    setFOutputs([]);
    setFSchema(DEFAULT_SCHEMA);
  };

  const openEdit = (it: PlazaItem) => {
    setEditing(it);
    setFormOpen(true);
    setFName(it.name);
    setFDesc(it.description);
    setFCategory(it.category);
    setFOutputs(it.outputs);
    const d = it.diy;
    if (d?.command) {
      setFMode("command");
      setFCommand(d.command);
      setFLang(d.lang || "python");
      setFCode(d.code || "");
    } else {
      setFMode("script");
      setFCommand("");
      setFLang(d?.lang || "python");
      setFCode(d?.code || "");
    }
    setFSchema(d?.parameters ? JSON.stringify(d.parameters) : DEFAULT_SCHEMA);
  };

  const closeForm = () => {
    setFormOpen(false);
    setEditing(null);
  };

  const toggleOutput = (o: string) => {
    setFOutputs((prev) =>
      prev.includes(o) ? prev.filter((x) => x !== o) : [...prev, o]
    );
  };

  const save = async () => {
    if (!fName.trim()) {
      setError("请填写工具名称");
      return;
    }
    let params: unknown = {};
    try {
      params = JSON.parse(fSchema || DEFAULT_SCHEMA);
    } catch {
      setError("入参 JSON Schema 不是合法 JSON");
      return;
    }

    const diy: DiyToolSpec = {
      parameters: params,
      ...(fMode === "command"
        ? { command: fCommand.trim() }
        : { lang: fLang, code: fCode }),
    };

    if (fMode === "command" && !fCommand.trim()) {
      setError("请填写命令");
      return;
    }
    if (fMode === "script" && !fCode.trim()) {
      setError("请填写脚本代码");
      return;
    }

    const item: PlazaItem = {
      id: editing?.id ?? genId(),
      name: fName.trim(),
      description: fDesc.trim(),
      kind: "tool",
      source: "diy",
      category: fCategory.trim() || "通用",
      tags: [],
      trust: "authored",
      outputs: fOutputs,
      callable: true,
      diy,
    };

    setSaving(true);
    setError("");
    try {
      await plazaSaveItem(item);
      closeForm();
      await load();
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  const remove = async (it: PlazaItem) => {
    if (!window.confirm(`确定删除自研工具「${it.name}」？`)) return;
    try {
      await plazaDeleteItem(it.id);
      await load();
    } catch (e) {
      setError(String(e));
    }
  };

  const openRun = (it: PlazaItem) => {
    setRunFor(it);
    setRunArgs("{}");
    setRunFormVals({});
    setRunJsonMode(false);
    setRunOutput("");
    setApproval(null);
    approvalRef.current = null;
    setError("");
  };

  const runFields = useMemo(
    () => paramFields(runFor?.parameters ?? runFor?.diy?.parameters),
    [runFor]
  );

  const doRun = async () => {
    if (!runFor) return;

    // 组装入参：优先按 Schema 动态表单，其次原始 JSON
    let args: unknown = {};
    if (runJsonMode) {
      try {
        args = JSON.parse(runArgs || "{}");
      } catch {
        setError("运行参数不是合法 JSON");
        return;
      }
    } else if (runFields.length > 0) {
      const obj: Record<string, unknown> = {};
      for (const f of runFields) {
        const raw = (runFormVals[f.key] ?? "").trim();
        if (f.required && raw === "") {
          setError(`请填写必填参数「${f.key}」${f.desc ? `（${f.desc}）` : ""}`);
          return;
        }
        if (raw === "") continue;
        if (f.type === "number" || f.type === "integer") {
          const n = Number(raw);
          if (Number.isNaN(n)) {
            setError(`参数「${f.key}」应为数字`);
            return;
          }
          obj[f.key] = f.type === "integer" ? Math.trunc(n) : n;
        } else if (f.type === "boolean") {
          obj[f.key] = raw === "true" || raw === "1";
        } else if (f.type === "array" || f.type === "object") {
          try {
            obj[f.key] = JSON.parse(raw);
          } catch {
            setError(`参数「${f.key}」应为合法 JSON（${f.type}）`);
            return;
          }
        } else {
          obj[f.key] = raw;
        }
      }
      args = obj;
    }

    setRunning(true);
    setError("");
    setRunOutput("");
    try {
      const out = await plazaRun(runFor.name, args);
      setRunOutput(
        typeof out === "string" ? out : JSON.stringify(out, null, 2)
      );
    } catch (e) {
      setError(String(e));
    } finally {
      setRunning(false);
      // 运行结束（含超时/被拒）时清理残留的审批请求，避免底部卡片遗留
      if (approvalRef.current) {
        removePending(approvalRef.current.id);
        approvalRef.current = null;
        setApproval(null);
      }
    }
  };

  const diyCount = items.filter(
    (i) => i.source === "diy" || i.source === "market"
  ).length;

  return (
    <>
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
          <h3 style={{ margin: 0, fontSize: 15, letterSpacing: 1 }}>任务广场</h3>
          <span style={{ fontSize: 11, color: "var(--text-faint)" }}>
            {items.length} 个能力 · {diyCount} 个自研
          </span>
          <span style={{ flex: 1 }} />
          <button className="software-close" onClick={onClose} title="关闭">
            ×
          </button>
        </div>

        {/* 主体 */}
        <div className="software-body">
          {error && <div className="software-error">{error}</div>}

          {formOpen ? (
            <div className="software-col">
              <div className="software-sec-title">
                {editing ? `编辑自研工具「${editing.name}」` : "新建自研工具"}
              </div>

              <div className="software-row">
                <span className="software-kv">名称</span>
                <input
                  className="software-search-input"
                  style={{ flex: 1 }}
                  placeholder="如：生成周报"
                  value={fName}
                  onChange={(e) => setFName(e.target.value)}
                />
              </div>

              <div className="software-row">
                <span className="software-kv">描述</span>
                <input
                  className="software-search-input"
                  style={{ flex: 1 }}
                  placeholder="一句话说明这个工具做什么"
                  value={fDesc}
                  onChange={(e) => setFDesc(e.target.value)}
                />
              </div>

              <div className="software-row">
                <span className="software-kv">分类</span>
                <input
                  className="software-search-input"
                  style={{ flex: 1 }}
                  placeholder="通用 / 文件 / 网络·数据 …"
                  value={fCategory}
                  onChange={(e) => setFCategory(e.target.value)}
                />
              </div>

              <div className="software-sec-title">执行方式</div>
              <div style={{ display: "flex", gap: 8 }}>
                <button
                  className={fMode === "command" ? "software-primary" : "software-refresh"}
                  onClick={() => setFMode("command")}
                >
                  Shell 命令
                </button>
                <button
                  className={fMode === "script" ? "software-primary" : "software-refresh"}
                  onClick={() => setFMode("script")}
                >
                  脚本
                </button>
              </div>

              {fMode === "command" ? (
                <div className="software-row">
                  <span className="software-kv">命令</span>
                  <input
                    className="software-search-input"
                    style={{ flex: 1, fontFamily: "monospace" }}
                    placeholder="如：python collect.py {query}"
                    value={fCommand}
                    onChange={(e) => setFCommand(e.target.value)}
                  />
                </div>
              ) : (
                <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
                  <div className="software-row">
                    <span className="software-kv">语言</span>
                    <select
                      className="mode-select"
                      value={fLang}
                      onChange={(e) => setFLang(e.target.value)}
                    >
                      <option value="python">python</option>
                      <option value="nodejs">nodejs</option>
                    </select>
                  </div>
                  <textarea
                    className="software-search-input"
                    style={{ minHeight: 110, resize: "vertical", fontFamily: "monospace" }}
                    placeholder="脚本代码…"
                    value={fCode}
                    onChange={(e) => setFCode(e.target.value)}
                  />
                </div>
              )}

              <div className="software-sec-title">入参 JSON Schema</div>
              <textarea
                className="software-search-input"
                style={{ minHeight: 60, resize: "vertical", fontFamily: "monospace" }}
                placeholder={DEFAULT_SCHEMA}
                value={fSchema}
                onChange={(e) => setFSchema(e.target.value)}
              />

              <div className="software-sec-title">输出联动窗口</div>
              <div style={{ display: "flex", flexWrap: "wrap", gap: 6 }}>
                {ALL_OUTPUTS.map((o) => (
                  <button
                    key={o}
                    className={fOutputs.includes(o) ? "software-primary" : "software-refresh"}
                    style={{ padding: "4px 10px", fontSize: 12 }}
                    onClick={() => toggleOutput(o)}
                  >
                    {OUTPUT_LABEL[o]}
                  </button>
                ))}
              </div>

              <div style={{ display: "flex", gap: 8, marginTop: 12 }}>
                <button className="software-primary" onClick={save} disabled={saving}>
                  {saving ? "保存中…" : "保存"}
                </button>
                <button className="software-refresh" onClick={closeForm}>
                  取消
                </button>
              </div>
            </div>
          ) : runFor ? (
            <div className="software-col">
              <div className="software-sec-title">
                运行「{runFor.name}」
                {runFor.source === "market" && "（市场工具 · 未受信）"}
                {runFor.source === "diy" && "（自研工具）"}
              </div>
              {runFor.description && (
                <div style={{ fontSize: 11, color: "var(--text-faint)" }}>
                  {runFor.description}
                </div>
              )}

              {/* 参数填写：按入参 Schema 动态生成表单，无入参则直接执行 */}
              {runFields.length === 0 ? (
                <div style={{ fontSize: 11, color: "var(--text-faint)", marginTop: 8 }}>
                  该工具无需入参，可直接执行。
                </div>
              ) : (
                <div className="software-sec-title" style={{ marginTop: 10 }}>
                  入参
                  <button
                    className="software-refresh"
                    style={{ padding: "2px 8px", fontSize: 11, marginLeft: 8 }}
                    onClick={() => setRunJsonMode((v) => !v)}
                  >
                    {runJsonMode ? "表单模式" : "JSON 模式"}
                  </button>
                </div>
              )}

              {runFields.length > 0 && runJsonMode ? (
                <textarea
                  className="software-search-input"
                  style={{ minHeight: 90, resize: "vertical", fontFamily: "monospace" }}
                  placeholder='{"key": "value"}'
                  value={runArgs}
                  onChange={(e) => setRunArgs(e.target.value)}
                />
              ) : (
                runFields.map((f) => (
                  <div className="software-row" key={f.key}>
                    <span className="software-kv" style={{ minWidth: 110 }}>
                      {f.required && <span style={{ color: "var(--danger)" }}>*</span>}
                      {f.key}
                    </span>
                    <input
                      className="software-search-input"
                      style={{ flex: 1, fontFamily: "monospace" }}
                      placeholder={
                        f.desc ||
                        (f.type === "number" || f.type === "integer"
                          ? "数字"
                          : f.type === "boolean"
                          ? "true / false"
                          : f.type === "array" || f.type === "object"
                          ? "JSON 数组/对象"
                          : "文本")
                      }
                      value={runFormVals[f.key] ?? ""}
                      onChange={(e) =>
                        setRunFormVals((prev) => ({ ...prev, [f.key]: e.target.value }))
                      }
                    />
                  </div>
                ))
              )}

              <div style={{ display: "flex", gap: 8, marginTop: 12 }}>
                <button className="software-primary" onClick={doRun} disabled={running}>
                  {running ? "执行中…" : "执行"}
                </button>
                <button className="software-refresh" onClick={() => setRunFor(null)}>
                  返回
                </button>
              </div>

              {running && (
                <div style={{ marginTop: 12, fontSize: 12, color: "var(--cyan)" }}>
                  正在运行，请稍候…
                </div>
              )}

              {runOutput && (
                <div style={{ marginTop: 12 }}>
                  <div style={{ fontSize: 11, color: "var(--text-faint)", marginBottom: 4 }}>
                    执行结果
                  </div>
                  <pre
                    style={{
                      margin: 0,
                      padding: "10px",
                      background: "rgba(0,0,0,0.25)",
                      borderRadius: 6,
                      fontSize: 11,
                      maxHeight: "42vh",
                      overflow: "auto",
                      whiteSpace: "pre-wrap",
                      wordBreak: "break-all",
                      color: "var(--text-dim)",
                    }}
                  >
                    {runOutput}
                  </pre>
                </div>
              )}
            </div>
          ) : marketOpen ? (
            <div className="software-col">
              <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 8 }}>
                <button className="software-refresh" onClick={() => setMarketOpen(false)}>
                  ← 返回
                </button>
                <span style={{ fontSize: 13, letterSpacing: 1 }}>市场仓库 · 市面厂库</span>
              </div>
              <div style={{ fontSize: 11, color: "var(--text-faint)", marginBottom: 10 }}>
                以下能力来自市面厂库，安装后标记为「未受信」，执行前需你确认授权。
              </div>
              {marketBusy ? (
                <div style={{ color: "var(--text-dim)" }}>加载中…</div>
              ) : marketItems.length === 0 ? (
                <div style={{ color: "var(--text-faint)" }}>市场暂无上架的工具。</div>
              ) : (
                <div className="software-pkg-list">
                  {marketItems.map((m) => {
                    const installed = installedNames.has(m.name);
                    return (
                      <div
                        className="software-pkg"
                        key={m.id}
                        style={{ flexDirection: "column", alignItems: "stretch" }}
                      >
                        <div style={{ display: "flex", alignItems: "center", gap: 8, width: "100%" }}>
                          <span className="software-pkg-name" style={{ flex: 1 }}>
                            {m.name}
                          </span>
                          <span className="software-badge warn">未受信</span>
                        </div>

                        <div style={{ fontSize: 11, color: "var(--text-faint)", marginTop: 4 }}>
                          {m.description}
                        </div>

                        {(m.category || m.tags.length > 0) && (
                          <div style={{ display: "flex", gap: 6, marginTop: 4, flexWrap: "wrap" }}>
                            {m.category && (
                              <span className="software-badge" style={{ fontSize: 10 }}>
                                {m.category}
                              </span>
                            )}
                            {m.tags.map((t) => (
                              <span key={t} className="software-badge" style={{ fontSize: 10 }}>
                                #{t}
                              </span>
                            ))}
                          </div>
                        )}

                        <div style={{ display: "flex", gap: 6, marginTop: 6 }}>
                          {installed ? (
                            <span className="software-badge ok">已安装</span>
                          ) : (
                            <button
                              className="software-action"
                              onClick={() => installFromMarket(m)}
                              disabled={installingId === m.id}
                            >
                              {installingId === m.id ? "安装中…" : "安装"}
                            </button>
                          )}
                        </div>
                      </div>
                    );
                  })}
                </div>
              )}
            </div>
          ) : (
            <>
              {/* 搜索与筛选 */}
              <div style={{ display: "flex", gap: 8, marginBottom: 10, flexWrap: "wrap" }}>
                <input
                  className="software-search-input"
                  style={{ flex: 1, minWidth: 160 }}
                  placeholder="搜索工具 / 描述 / 分类 / 标签…"
                  value={query}
                  onChange={(e) => setQuery(e.target.value)}
                />
                <select
                  className="mode-select"
                  value={kindFilter}
                  onChange={(e) => setKindFilter(e.target.value)}
                >
                  <option value="">全部类型</option>
                  <option value="tool">工具</option>
                  <option value="workflow">工作流</option>
                  <option value="skill">技能</option>
                </select>
                <select
                  className="mode-select"
                  value={sourceFilter}
                  onChange={(e) => setSourceFilter(e.target.value)}
                >
                  <option value="">全部来源</option>
                  <option value="builtin">内置</option>
                  <option value="diy">自研</option>
                  <option value="market">市场</option>
                </select>
                <button className="software-primary" onClick={openCreate}>
                  ＋ 自研工具
                </button>
                <button
                  className={marketOpen ? "software-primary" : "software-refresh"}
                  onClick={() => {
                    setMarketOpen((v) => !v);
                    if (!marketOpen) void loadMarket();
                  }}
                >
                  市场仓库
                </button>
              </div>

              {busy ? (
                <div style={{ color: "var(--text-dim)" }}>加载中…</div>
              ) : filtered.length === 0 ? (
                <div style={{ color: "var(--text-faint)" }}>
                  没有匹配的能力。点击「＋ 自研工具」由白泽按需 DIY 一个。
                </div>
              ) : (
                <div className="software-pkg-list">
                  {filtered.map((it) => {
                    const removable = it.source !== "builtin";
                    return (
                      <div
                        className="software-pkg"
                        key={it.id}
                        style={{ flexDirection: "column", alignItems: "stretch" }}
                      >
                        <div style={{ display: "flex", alignItems: "center", gap: 8, width: "100%" }}>
                          <span className="software-pkg-name" style={{ flex: 1 }}>
                            {it.name}
                          </span>
                          <span className="software-badge">{KIND_LABEL[it.kind] || it.kind}</span>
                          <span className="software-badge">
                            {SOURCE_LABEL[it.source] || it.source}
                          </span>
                          <span className={`software-badge ${trustClass(it.trust)}`}>
                            {TRUST_LABEL[it.trust] || it.trust}
                          </span>
                        </div>

                        <div style={{ fontSize: 11, color: "var(--text-faint)", marginTop: 4 }}>
                          {it.description || "（无描述）"}
                        </div>

                        {(it.category || it.tags.length > 0) && (
                          <div style={{ display: "flex", gap: 6, marginTop: 4, flexWrap: "wrap" }}>
                            {it.category && (
                              <span className="software-badge" style={{ fontSize: 10 }}>
                                {it.category}
                              </span>
                            )}
                            {it.tags.map((t) => (
                              <span key={t} className="software-badge" style={{ fontSize: 10 }}>
                                #{t}
                              </span>
                            ))}
                          </div>
                        )}

                        {it.outputs.length > 0 && (
                          <div style={{ display: "flex", gap: 6, marginTop: 4, flexWrap: "wrap" }}>
                            <span style={{ fontSize: 10, color: "var(--text-faint)" }}>联动：</span>
                            {it.outputs.map((o) => (
                              <span key={o} className="software-badge" style={{ fontSize: 10 }}>
                                {OUTPUT_LABEL[o] || o}
                              </span>
                            ))}
                          </div>
                        )}

                        <div style={{ display: "flex", gap: 6, marginTop: 6, flexWrap: "wrap" }}>
                          {it.kind === "tool" && it.callable && (
                            <button className="software-action" onClick={() => openRun(it)}>
                              运行
                            </button>
                          )}
                          {removable && (
                            <button className="software-action" onClick={() => openEdit(it)}>
                              编辑
                            </button>
                          )}
                          {removable && (
                            <button className="software-action" onClick={() => remove(it)}>
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
      </div>
    </div>

    {running && approval && (
      <div
        style={{
          position: "fixed",
          inset: 0,
          background: "rgba(0,0,0,0.55)",
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          zIndex: 120,
        }}
      >
        <div className="acui-card permission" style={{ width: 420, maxWidth: "90vw" }}>
          <div className="acui-head">
            <span className="agent">泽</span>
            <span>请求授权 · {approval.tool}</span>
          </div>
          <div className="acui-body">
            <div style={{ marginBottom: 6 }}>
              该工具来自市场仓库（未受信），执行前需你确认以下真实载荷：
            </div>
            <pre>{JSON.stringify(approval.args, null, 2)}</pre>
          </div>
          <div className="acui-actions">
            <button className="acui-btn danger" onClick={() => void decideApproval(false)}>
              拒绝
            </button>
            <button className="acui-btn primary" onClick={() => void decideApproval(true)}>
              允许
            </button>
          </div>
        </div>
      </div>
    )}
    </>
  );
}