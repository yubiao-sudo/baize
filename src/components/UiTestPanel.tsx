import { Fragment, useEffect, useState, type CSSProperties } from "react";
import {
  onThought,
  pickFiles,
  pickFolder,
  readFile,
  testExportCases,
  testGenerateCases,
  testRunApi,
  testRunSelected,
  testRunUi,
  testLoadProjects,
  testSaveProject,
  testDeleteProject,
  testAutoDetectProject,
  testPrepareEnv,
  testImportOpenapi,
  testListRecords,
  testTrendGet,
  openPath,
} from "../api";
import type { TestTrendPoint } from "../api";
import type {
  ApiCaseResult,
  ApiTestResult,
  ExecutionRecord,
  GenerateTestCasesResult,
  ProjectProfile,
  SelectedCaseResult,
  TestCase,
  TestRunSelectedResult,
  UiStepResult,
  UiTestResult,
} from "../types";

type Tab = "project" | "cases" | "ui" | "api";

const TABS: { id: Tab; label: string }[] = [
  { id: "project", label: "项目配置" },
  { id: "cases", label: "用例生成" },
  { id: "ui", label: "UI 测试" },
  { id: "api", label: "接口测试" },
];

/** 测试面板动效样式（spinner / 进度条 / 徽标淡入），随面板首次渲染注入一次 */
const TEST_PANEL_CSS = `
.baize-spin {
  display: inline-block; width: 12px; height: 12px; border-radius: 50%;
  border: 2px solid var(--border-soft, rgba(255,255,255,0.15));
  border-top-color: var(--accent, #4f8cff);
  animation: baize-rot 0.8s linear infinite;
}
@keyframes baize-rot { to { transform: rotate(360deg); } }
.baize-progress {
  height: 6px; border-radius: 3px; overflow: hidden;
  background: var(--border-soft, rgba(255,255,255,0.12));
}
.baize-progress-bar {
  height: 100%; border-radius: 3px; background: var(--accent, #4f8cff);
  transition: width 0.35s ease;
}
.baize-progress-indet { width: 38% !important; animation: baize-slide 1.1s ease-in-out infinite; }
@keyframes baize-slide { 0% { margin-left: -38%; } 100% { margin-left: 100%; } }
.baize-chip {
  font-size: 11px; padding: 1px 8px; border-radius: 999px;
  border: 1px solid var(--border-soft, rgba(255,255,255,0.15));
  animation: baize-pop 0.25s ease;
}
@keyframes baize-pop { from { transform: scale(0.6); opacity: 0; } to { transform: scale(1); opacity: 1; } }
`;

const UI_STEPS_EXAMPLE = `[
  { "action": "wait", "ms": 800 },
  { "action": "window_focus", "name": "记事本" },
  { "action": "type_text", "text": "hello" },
  { "action": "assert", "ocr_text": "hello" }
]`;

// Web 页面自动化示例：走内置受控浏览器（桌面 Chrome，持久化登录态）
const WEB_STEPS_EXAMPLE = `[
  { "action": "open_page", "url": "http://localhost:5173/login" },
  { "action": "fill_input", "selector": "#username", "text": "tester" },
  { "action": "fill_input", "selector": "#password", "text": "***" },
  { "action": "upload_file", "path": "D:/testdata/avatar.png", "selector": "#avatar-input" },
  { "action": "ocr_captcha", "source": ".captcha-img", "target": "#captcha-input" },
  { "action": "click_selector", "selector": "button[type=submit]" },
  { "action": "assert_page_text", "text": "登录成功" }
]`;

const API_REQUESTS_EXAMPLE = `[
  { "name": "健康检查", "method": "GET", "url": "https://httpbin.org/get", "expect_status": 200 },
  { "name": "创建用户", "method": "POST", "url": "https://httpbin.org/post", "body": "{\\"name\\":\\"test\\"}", "expect_status": 200, "expect_body_contains": "name" }
]`;

/**
 * 软件测试工程师专用面板：需求 → 测试用例 → (UI / 接口) 自动化执行 → 报告。
 * 仅在「软件测试工程师」工作模式（qa-engineer）下通过顶栏「测试」按钮进入。
 */
export default function UiTestPanel({ onClose }: { onClose: () => void }) {
  const [tab, setTab] = useState<Tab>("project");
  // 当前激活的被测项目（由「项目配置」选择，供用例生成/执行环节复用，触发环境隔离与执行记录落盘）
  const [activeProject, setActiveProject] = useState<ProjectProfile | null>(null);
  return (
    <div className="rpanel">
      <style>{TEST_PANEL_CSS}</style>
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
          <h3 style={{ margin: 0, fontSize: 15, letterSpacing: 1 }}>自动化测试</h3>
          <span style={{ fontSize: 11, color: "var(--text-faint)" }}>
            需求 → 用例 → 执行 → 报告
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

        {/* 主体：四个子视图常驻挂载，仅用 display 切换，保证切换标签不丢已填数据 */}
        <div className="software-body">
          <div style={{ display: tab === "project" ? "block" : "none" }}>
            <ProjectView onActiveProjectChange={setActiveProject} />
          </div>
          <div style={{ display: tab === "cases" ? "block" : "none" }}>
            <CaseGenView project={activeProject} />
          </div>
          <div style={{ display: tab === "ui" ? "block" : "none" }}>
            <UiTestView />
          </div>
          <div style={{ display: tab === "api" ? "block" : "none" }}>
            <ApiTestView project={activeProject} />
          </div>
        </div>
      </div>
    </div>
  );
}

// ─────────── 被测对象台账（项目配置：第 0 步，测试闭环前置输入源） ───────────

const PROJECT_TYPES = [
  { value: "web", label: "Web 应用" },
  { value: "desktop", label: "桌面应用" },
  { value: "mobile", label: "移动 App" },
  { value: "api", label: "纯接口/服务" },
  { value: "miniprogram", label: "小程序" },
];
const READINESS_OPTIONS = [
  { value: "running", label: "已部署（直接测）" },
  { value: "boot", label: "白泽拉起" },
  { value: "login", label: "需登录态" },
];
const ENV_OPTIONS = [
  { value: "test", label: "test 测试环境" },
  { value: "staging", label: "staging 预发环境" },
  { value: "prod", label: "prod 生产环境" },
];

const fieldLabel: CSSProperties = { fontSize: 11, color: "var(--text-faint)" };
const fieldCol: CSSProperties = { display: "flex", flexDirection: "column", gap: 4 };

function emptyProfile(): ProjectProfile {
  return {
    id: "",
    name: "",
    project_type: "web",
    source: "",
    ui_entry: "",
    api_base: "",
    api_doc: "",
    repo_or_path: "",
    readiness: "running",
    run_command: "",
    account: "",
    env_tag: "test",
    report_dir: "",
  };
}

function ProjectView({ onActiveProjectChange }: { onActiveProjectChange: (p: ProjectProfile | null) => void }) {
  const [projects, setProjects] = useState<ProjectProfile[]>([]);
  const [profile, setProfile] = useState<ProjectProfile>(emptyProfile);
  const [selectedId, setSelectedId] = useState("");
  const [detectText, setDetectText] = useState("");
  const [busy, setBusy] = useState(false);
  const [detecting, setDetecting] = useState(false);
  const [msg, setMsg] = useState("");
  const [error, setError] = useState("");
  // 执行记录回看（当前选中项目）
  const [records, setRecords] = useState<ExecutionRecord[] | null>(null);
  const [recordsBusy, setRecordsBusy] = useState(false);
  // 执行趋势（回归质量曲线）；refreshKey 变化时重新拉取
  const [trendKey, setTrendKey] = useState(0);

  useEffect(() => {
    void (async () => {
      try {
        setProjects(await testLoadProjects());
      } catch (e) {
        setError(String(e));
      }
    })();
  }, []);

  const patch = (k: keyof ProjectProfile, v: string) =>
    setProfile((prev) => ({ ...prev, [k]: v }));

  const selectProject = (id: string) => {
    const p = projects.find((x) => x.id === id);
    if (p) {
      setSelectedId(p.id);
      setProfile({ ...p });
      onActiveProjectChange({ ...p });
      setMsg("");
      setError("");
    }
  };

  const newProject = () => {
    setSelectedId("");
    setProfile(emptyProfile());
    onActiveProjectChange(null);
    setMsg("");
    setError("");
  };

  const save = async () => {
    if (!profile.name.trim()) {
      setError("请先填写项目名");
      return;
    }
    setBusy(true);
    setError("");
    setMsg("");
    try {
      const list = await testSaveProject(profile);
      setProjects(list);
      const kept = profile.id
        ? list.find((x) => x.id === profile.id)
        : list[list.length - 1];
      if (kept) {
        setProfile({ ...kept });
        setSelectedId(kept.id);
        onActiveProjectChange({ ...kept });
      }
      setMsg("已保存项目档案");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    if (!selectedId) {
      setError("请先在列表选择一个项目再删除");
      return;
    }
    setBusy(true);
    setError("");
    setMsg("");
    try {
      const list = await testDeleteProject(selectedId);
      setProjects(list);
      setSelectedId("");
      setProfile(emptyProfile());
      onActiveProjectChange(null);
      setMsg("已删除该项目档案");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const autoDetect = async () => {
    if (!detectText.trim()) {
      setError("自动识别需要先在下方「识别框」粘贴需求文本");
      return;
    }
    setDetecting(true);
    setError("");
    setMsg("");
    try {
      const p = await testAutoDetectProject(
        detectText.trim(),
        profile.repo_or_path.trim() || undefined
      );
      const merged = { ...emptyProfile(), ...p, id: "" };
      if (!merged.project_type) merged.project_type = "web";
      if (!merged.readiness) merged.readiness = "running";
      if (!merged.env_tag) merged.env_tag = "test";
      setProfile(merged);
      setMsg("自动识别完成，请核对字段后保存");
    } catch (e) {
      setError(String(e));
    } finally {
      setDetecting(false);
    }
  };

  const prepareEnv = async () => {
    if (!profile.run_command.trim()) {
      setError("当前项目未填写启动命令（run_command）");
      return;
    }
    setBusy(true);
    setError("");
    setMsg("");
    try {
      const r = await testPrepareEnv(profile.run_command.trim());
      setMsg(r.detail);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const browseRepo = async () => {
    try {
      const d = await pickFolder();
      if (d) patch("repo_or_path", d);
    } catch (e) {
      setError(String(e));
    }
  };

  const browseReportDir = async () => {
    try {
      const d = await pickFolder();
      if (d) patch("report_dir", d);
    } catch (e) {
      setError(String(e));
    }
  };

  const loadRecords = async () => {
    // 需要已保存的项目（有 id）；未保存的用 id 空串也能查（目录按 名称_id 组织）
    if (!profile.name.trim()) {
      setError("请先填写或选择一个项目，再回看执行记录");
      return;
    }
    setRecordsBusy(true);
    setError("");
    try {
      setRecords(
        await testListRecords(
          profile.name.trim(),
          selectedId || profile.id,
          profile.report_dir || undefined
        )
      );
    } catch (e) {
      setError(String(e));
    } finally {
      setRecordsBusy(false);
    }
  };

  return (
    <div className="software-col">
      {/* ① 已有台账列表 */}
      <div className="software-sec-title" style={{ marginBottom: 8 }}>
        被测对象台账
      </div>
      <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
        <select
          className="mode-select"
          style={{ flex: 1, minWidth: 0 }}
          value={selectedId}
          onChange={(e) => selectProject(e.target.value)}
        >
          <option value="">— 选择已有项目（或点「新建」）—</option>
          {projects.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
              {p.env_tag ? ` · ${p.env_tag}` : ""}
              {p.project_type ? ` · ${p.project_type}` : ""}
            </option>
          ))}
        </select>
        <button className="software-action" onClick={newProject}>新建</button>
        <button className="software-action" onClick={() => void remove()} disabled={busy}>
          删除
        </button>
      </div>
      <div style={{ fontSize: 11, color: "var(--text-faint)", marginTop: 6 }}>
        共 {projects.length} 个项目档案 · 一次录入、四个阶段共同复用
      </div>

      {/* ①b 回归趋势：每次自动执行入库一条，历史通过率一眼可见 */}
      <div className="software-sec-title" style={{ margin: "18px 0 8px" }}>
        回归趋势
        <button
          className="software-action"
          style={{ marginLeft: 8, fontSize: 11, padding: "2px 8px" }}
          onClick={() => setTrendKey((k) => k + 1)}
        >
          刷新
        </button>
      </div>
      <TrendChart projectId={selectedId || profile.id} refreshKey={trendKey} />

      {/* ② 自动识别 */}
      <div className="software-pkg" style={{ marginTop: 12, flexDirection: "column", alignItems: "stretch" }}>
        <span className="software-sec-title" style={{ margin: 0 }}>自动识别（从需求文本嗅探项目形态与地址）</span>
        <textarea
          className="software-search-input"
          style={{ minHeight: 80, resize: "vertical", marginTop: 8 }}
          placeholder="粘贴需求文档/需求描述，白泽会自动推断：项目形态、UI 入口 URL、接口 base_url、就绪方式、环境标识…"
          value={detectText}
          onChange={(e) => setDetectText(e.target.value)}
        />
        <div style={{ display: "flex", gap: 8, marginTop: 8, alignItems: "center" }}>
          <button className="software-primary" onClick={() => void autoDetect()} disabled={detecting}>
            {detecting ? "识别中…" : "自动识别"}
          </button>
          <span style={{ fontSize: 11, color: "var(--text-faint)" }}>
            识别结果填充到下方表单；敏感信息（账号/命令）请手动补全
          </span>
        </div>
      </div>

      {/* ③ 手动补全表单 */}
      <div className="software-pkg" style={{ marginTop: 12, flexDirection: "column", alignItems: "stretch" }}>
        <span className="software-sec-title" style={{ margin: 0 }}>项目档案明细</span>
        <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 10, marginTop: 10 }}>
          <div style={{ ...fieldCol, gridColumn: "1 / -1" }}>
            <span style={fieldLabel}>项目名 *</span>
            <input
              className="software-search-input"
              placeholder="如：电商后台管理系统"
              value={profile.name}
              onChange={(e) => patch("name", e.target.value)}
            />
          </div>

          <div style={fieldCol}>
            <span style={fieldLabel}>项目形态</span>
            <select
              className="mode-select"
              value={profile.project_type}
              onChange={(e) => patch("project_type", e.target.value)}
            >
              {PROJECT_TYPES.map((o) => (
                <option key={o.value} value={o.value}>{o.label}</option>
              ))}
            </select>
          </div>

          <div style={fieldCol}>
            <span style={fieldLabel}>环境标识</span>
            <select
              className="mode-select"
              value={profile.env_tag}
              onChange={(e) => patch("env_tag", e.target.value)}
            >
              {ENV_OPTIONS.map((o) => (
                <option key={o.value} value={o.value}>{o.label}</option>
              ))}
            </select>
          </div>

          <div style={fieldCol}>
            <span style={fieldLabel}>就绪方式</span>
            <select
              className="mode-select"
              value={profile.readiness}
              onChange={(e) => patch("readiness", e.target.value)}
            >
              {READINESS_OPTIONS.map((o) => (
                <option key={o.value} value={o.value}>{o.label}</option>
              ))}
            </select>
          </div>

          <div style={fieldCol}>
            <span style={fieldLabel}>测试账号 / Token（敏感）</span>
            <input
              className="software-search-input"
              placeholder="可选"
              value={profile.account}
              onChange={(e) => patch("account", e.target.value)}
            />
          </div>

          <div style={{ ...fieldCol, gridColumn: "1 / -1" }}>
            <span style={fieldLabel}>Web UI 入口 URL（web 形态）</span>
            <input
              className="software-search-input"
              placeholder="如：http://localhost:5173/login"
              value={profile.ui_entry}
              onChange={(e) => patch("ui_entry", e.target.value)}
            />
          </div>

          <div style={{ ...fieldCol, gridColumn: "1 / -1" }}>
            <span style={fieldLabel}>接口 base_url</span>
            <input
              className="software-search-input"
              placeholder="如：http://localhost:8080/api/v1"
              value={profile.api_base}
              onChange={(e) => patch("api_base", e.target.value)}
            />
          </div>

          <div style={{ ...fieldCol, gridColumn: "1 / -1" }}>
            <span style={fieldLabel}>openapi/swagger 文档（可选，用于直出接口用例）</span>
            <input
              className="software-search-input"
              placeholder="本地路径或在线地址"
              value={profile.api_doc}
              onChange={(e) => patch("api_doc", e.target.value)}
            />
          </div>

          <div style={{ ...fieldCol, gridColumn: "1 / -1" }}>
            <span style={fieldLabel}>需求文档来源</span>
            <input
              className="software-search-input"
              placeholder="本地文件 / 飞书链接 / URL"
              value={profile.source}
              onChange={(e) => patch("source", e.target.value)}
            />
          </div>

          <div style={{ ...fieldCol, gridColumn: "1 / -1" }}>
            <span style={fieldLabel}>代码仓库 / 本地目录</span>
            <div style={{ display: "flex", gap: 6 }}>
              <input
                className="software-search-input"
                style={{ flex: 1 }}
                placeholder="本地目录或仓库地址"
                value={profile.repo_or_path}
                onChange={(e) => patch("repo_or_path", e.target.value)}
              />
              <button className="software-action" onClick={() => void browseRepo()}>浏览</button>
            </div>
          </div>

          <div style={{ ...fieldCol, gridColumn: "1 / -1" }}>
            <span style={fieldLabel}>启动命令（就绪方式 = 白泽拉起 时生效）</span>
            <input
              className="software-search-input"
              placeholder="如：npm run dev  或  docker compose up"
              value={profile.run_command}
              onChange={(e) => patch("run_command", e.target.value)}
            />
          </div>

          <div style={{ ...fieldCol, gridColumn: "1 / -1" }}>
            <span style={fieldLabel}>
              报告保存目录（留空 = 系统文档目录 / 白泽测试记录；文档目录被搬家工具重定向时建议显式指定）
            </span>
            <div style={{ display: "flex", gap: 6 }}>
              <input
                className="software-search-input"
                style={{ flex: 1 }}
                placeholder="如：D:\测试报告（报告、MD、可复用脚本、失败截图都存这里）"
                value={profile.report_dir}
                onChange={(e) => patch("report_dir", e.target.value)}
              />
              <button className="software-action" onClick={() => void browseReportDir()}>浏览</button>
            </div>
          </div>
        </div>

        <div style={{ display: "flex", gap: 8, marginTop: 12, flexWrap: "wrap", alignItems: "center" }}>
          <button className="software-primary" onClick={() => void save()} disabled={busy}>
            {busy ? "保存中…" : "保存档案"}
          </button>
          {profile.readiness === "boot" && (
            <button className="software-action" onClick={() => void prepareEnv()} disabled={busy}>
              环境准备（拉起应用）
            </button>
          )}
          {profile.env_tag === "prod" && (
            <span style={{ fontSize: 11, color: "var(--danger)" }}>
              ⚠ 生产环境：接口测试将被环境隔离硬门拦截
            </span>
          )}
        </div>
      </div>

      {/* ④ 执行记录回看 */}
      <div className="software-pkg" style={{ marginTop: 12, flexDirection: "column", alignItems: "stretch" }}>
        <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <span className="software-sec-title" style={{ margin: 0, flex: 1 }}>执行记录回看</span>
          <button className="software-action" onClick={() => void loadRecords()} disabled={recordsBusy}>
            {recordsBusy ? "读取中…" : records === null ? "查看本项目的记录" : "刷新"}
          </button>
        </div>
        {records !== null && (
          records.length === 0 ? (
            <div style={{ fontSize: 11, color: "var(--text-faint)", marginTop: 8 }}>
              暂无执行记录：勾选用例并传入激活项目执行后，报告自动落盘到「文档 / 白泽测试记录」
            </div>
          ) : (
            <div style={{ marginTop: 8, display: "flex", flexDirection: "column", gap: 6 }}>
              {records.map((r) => (
                <div
                  key={r.stem}
                  className="software-pkg"
                  style={{ padding: "6px 10px", alignItems: "center", gap: 8 }}
                >
                  <span style={{ flex: 1, minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", fontSize: 12 }} title={r.title}>
                    {r.ts ? `${r.ts} · ` : ""}{r.title || r.stem}
                  </span>
                  {r.html && (
                    <button className="software-action" onClick={() => void openPath(r.html!)} title="用浏览器打开 HTML 报告">
                      HTML 报告
                    </button>
                  )}
                  {r.md && (
                    <button className="software-action" onClick={() => void openPath(r.md!)} title="打开 Markdown 报告">
                      MD
                    </button>
                  )}
                </div>
              ))}
            </div>
          )
        )}
      </div>

      {msg && (
        <div style={{ marginTop: 10, fontSize: 12, color: "var(--ok, #22c55e)" }}>{msg}</div>
      )}
      {error && <div className="software-error" style={{ marginTop: 10 }}>{error}</div>}
    </div>
  );
}

// ─────────── 回归趋势图（通过率历史曲线，回归质量一眼可见） ───────────

function TrendChart({ projectId, refreshKey }: { projectId: string; refreshKey: number }) {
  const [points, setPoints] = useState<TestTrendPoint[] | null>(null);
  const [error, setError] = useState("");

  useEffect(() => {
    if (!projectId) {
      setPoints(null);
      return;
    }
    let alive = true;
    testTrendGet(projectId)
      .then((list) => {
        if (alive) {
          setPoints(list);
          setError("");
        }
      })
      .catch((e) => {
        if (alive) setError(String(e));
      });
    return () => {
      alive = false;
    };
  }, [projectId, refreshKey]);

  if (!projectId) return null;
  if (error) return <div className="software-error" style={{ marginTop: 4 }}>{error}</div>;
  if (!points) return null;
  if (points.length === 0) {
    return (
      <div style={{ fontSize: 12, color: "var(--text-faint)" }}>
        暂无执行趋势 · 执行一次自动化测试后，这里会出现通过率历史曲线
      </div>
    );
  }

  const W = 560;
  const H = 150;
  const PAD_L = 30;
  const PAD_R = 12;
  const PAD_T = 10;
  const PAD_B = 22;
  const iw = W - PAD_L - PAD_R;
  const ih = H - PAD_T - PAD_B;
  const n = points.length;
  const xAt = (i: number) => PAD_L + (n === 1 ? iw / 2 : (i * iw) / (n - 1));
  const yAt = (rate: number) => PAD_T + ih * (1 - Math.min(100, Math.max(0, rate)) / 100);
  const line = points.map((p, i) => `${xAt(i).toFixed(1)},${yAt(p.rate).toFixed(1)}`).join(" ");
  const area = `${xAt(0).toFixed(1)},${(PAD_T + ih).toFixed(1)} ${line} ${xAt(n - 1).toFixed(1)},${(PAD_T + ih).toFixed(1)}`;
  const dotColor = (p: TestTrendPoint) => (p.rate >= 90 ? "#22c55e" : p.rate >= 70 ? "#f59e0b" : "#ef4444");
  const fmt = (ts: number) => {
    const d = new Date(ts);
    const p2 = (v: number) => String(v).padStart(2, "0");
    return `${p2(d.getMonth() + 1)}-${p2(d.getDate())} ${p2(d.getHours())}:${p2(d.getMinutes())}`;
  };
  // X 轴标签最多 6 个，均匀抽样
  const labelIdx = new Set<number>();
  const labelCount = Math.min(6, n);
  for (let k = 0; k < labelCount; k++) {
    labelIdx.add(Math.round((k * (n - 1)) / Math.max(1, labelCount - 1)));
  }
  const last = points[n - 1];

  return (
    <div
      className="software-pkg"
      style={{ marginTop: 0, flexDirection: "column", alignItems: "stretch", gap: 6, padding: "10px 12px" }}
    >
      <div style={{ fontSize: 11, color: "var(--text-faint)" }}>
        最近 {n} 次执行 · 最新通过率 <b style={{ color: dotColor(last) }}>{last.rate}%</b>
        （{last.passed}/{last.total} 通过）
      </div>
      <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", height: "auto", display: "block" }}>
        {/* 网格与刻度 */}
        {[0, 50, 100].map((v) => (
          <g key={v}>
            <line x1={PAD_L} y1={yAt(v)} x2={W - PAD_R} y2={yAt(v)} stroke="currentColor" strokeOpacity={0.12} strokeWidth={1} />
            <text x={PAD_L - 6} y={yAt(v) + 3} textAnchor="end" fontSize={9} fill="currentColor" fillOpacity={0.45}>
              {v}
            </text>
          </g>
        ))}
        {/* 通过率面积 + 折线 */}
        <polygon points={area} fill="#22c55e" fillOpacity={0.10} />
        <polyline points={line} fill="none" stroke="#22c55e" strokeWidth={1.8} strokeLinejoin="round" />
        {/* 数据点（悬停看详情） */}
        {points.map((p, i) => (
          <g key={i}>
            <circle cx={xAt(i)} cy={yAt(p.rate)} r={3} fill={dotColor(p)}>
              <title>{`${p.name}\n${fmt(p.ts)} · ${p.passed}/${p.total} 通过 · 通过率 ${p.rate}%`}</title>
            </circle>
            {labelIdx.has(i) && (
              <text x={xAt(i)} y={H - 8} textAnchor="middle" fontSize={9} fill="currentColor" fillOpacity={0.45}>
                {fmt(p.ts).split(" ")[0]}
              </text>
            )}
          </g>
        ))}
      </svg>
    </div>
  );
}

// ─────────── 可复用脚本文件（自动执行链路落盘 / 手写文件通用） ───────────

interface LoadableScript {
  title: string;
  kind: string;
  ui_steps: unknown[];
  api_requests: unknown[];
  setup?: unknown[];
  teardown?: unknown[];
}

/** 解析脚本文件：{ name, created, scripts:[...] }（自动执行时落盘的 *_scripts.json）；
 *  兼容裸 ui_steps / api_requests 数组 */
function parseScriptFile(text: string): { fileTitle: string; items: LoadableScript[] } {
  const v = JSON.parse(text) as Record<string, unknown> | unknown[] | null;
  const items: LoadableScript[] = [];
  let fileTitle = "";
  if (v && !Array.isArray(v) && Array.isArray(v.scripts)) {
    fileTitle = String((v as Record<string, unknown>).name ?? "");
    for (const s of v.scripts as Record<string, unknown>[]) {
      items.push({
        title: String(s.title ?? "") || `用例${Number(s.case_index ?? 0) + 1}`,
        kind: String(s.kind ?? ""),
        ui_steps: Array.isArray(s.ui_steps) ? (s.ui_steps as unknown[]) : [],
        api_requests: Array.isArray(s.api_requests) ? (s.api_requests as unknown[]) : [],
        setup: Array.isArray(s.setup) ? (s.setup as unknown[]) : undefined,
        teardown: Array.isArray(s.teardown) ? (s.teardown as unknown[]) : undefined,
      });
    }
  } else if (Array.isArray(v)) {
    const looksApi = v.some(
      (x) => x && typeof x === "object" && ("url" in x || "method" in x)
    );
    items.push({
      title: "整个文件",
      kind: looksApi ? "api" : "ui",
      ui_steps: looksApi ? [] : v,
      api_requests: looksApi ? v : [],
    });
  }
  return { fileTitle, items };
}

/** 弹出文件选择并解析脚本文件；用户取消返回 null */
async function pickAndParseScripts(): Promise<{ fileTitle: string; items: LoadableScript[] } | null> {
  const files = await pickFiles();
  if (!files || files.length === 0) return null;
  const raw = await readFile(files[0]);
  return parseScriptFile(String(raw));
}

// ─────────── 用例生成 ───────────

/** 用例类型可选项（与后端 CASE_TYPE_OPTIONS 保持一致） */
const CASE_TYPES = ["功能", "UI", "接口", "安全", "性能"];

/** 导出格式标签 */
const EXPORT_FORMATS: { id: "json" | "csv" | "xlsx"; label: string }[] = [
  { id: "json", label: "JSON" },
  { id: "csv", label: "CSV" },
  { id: "xlsx", label: "Excel" },
];

function CaseGenView({ project }: { project: ProjectProfile | null }) {
  const [requirement, setRequirement] = useState("");
  const [path, setPath] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [result, setResult] = useState<GenerateTestCasesResult | null>(null);
  const [stages, setStages] = useState<{ label: string; detail: string }[]>([]);
  const [selected, setSelected] = useState<Record<number, boolean>>({});
  const [execBusy, setExecBusy] = useState(false);
  const [execResult, setExecResult] = useState<TestRunSelectedResult | null>(null);
  // 自动执行实时进度：逐条 ✓/✕ 徽标 + 进度条（后端每执行完一条广播一次）
  const [execTotal, setExecTotal] = useState(0);
  const [execItems, setExecItems] = useState<{ title: string; ok: boolean }[]>([]);
  // 用例类型多选（缺省全选）+ 每类条数（0 = 不限）
  const [typeSel, setTypeSel] = useState<Record<string, boolean>>(() =>
    Object.fromEntries(CASE_TYPES.map((t) => [t, true]))
  );
  const [perType, setPerType] = useState(0);
  // 导出格式选择与保存结果
  const [exportFmt, setExportFmt] = useState<"json" | "csv" | "xlsx">("xlsx");
  const [savedPath, setSavedPath] = useState<string | null>(null);

  const chosenTypes = CASE_TYPES.filter((t) => typeSel[t]);

  // 订阅后端 test_pipeline 阶段事件，实时可视化「读取文档 → 需求分析 → 用例设计 → 覆盖检查」
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void onThought((t) => {
      if (disposed || t.kind !== "test_pipeline") return;
      // 「自动执行」逐条结构化进度：追加实时 ✓/✕ 徽标
      if (t.label === "自动执行" && t.title && typeof t.ok === "boolean") {
        setExecItems((prev) => [...prev, { title: t.title!, ok: t.ok! }]);
        return;
      }
      setStages((prev) => {
        const i = prev.findIndex((s) => s.label === t.label);
        if (i >= 0) {
          const next = prev.slice();
          next[i] = { label: t.label, detail: t.detail };
          return next;
        }
        return [...prev, { label: t.label, detail: t.detail }];
      });
    }).then((f) => {
      if (!disposed) unlisten = f;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  const choose = async () => {
    try {
      const files = await pickFiles();
      if (files && files.length > 0) setPath(files[0]);
    } catch (e) {
      setError(String(e));
    }
  };

  const run = async () => {
    if (!requirement.trim() && !path.trim()) {
      setError("请填写需求文本或选择需求文档路径（二选一）");
      return;
    }
    if (chosenTypes.length === 0) {
      setError("请至少选择一种用例类型");
      return;
    }
    setBusy(true);
    setError("");
    setResult(null);
    setStages([]);
    setSelected({});
    setExecResult(null);
    setSavedPath(null);
    try {
      const r = await testGenerateCases(
        requirement.trim() || undefined,
        path.trim() || undefined,
        chosenTypes,
        perType > 0 ? perType : undefined
      );
      setResult(r);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  /** 导出用例：勾选了几条就导几条，一条没勾则导出全部 */
  const doExport = async () => {
    const chosen = cases.filter((_, i) => selected[i]);
    const list = chosen.length > 0 ? chosen : cases;
    if (list.length === 0) return;
    try {
      const p = await testExportCases(
        list as unknown[],
        exportFmt,
        project?.name ? `${project.name}-测试用例` : "测试用例"
      );
      setSavedPath(p);
    } catch (e) {
      setError(String(e));
    }
  };

  // 展示列表：按类别分节（按所选类型顺序），组内优先级 P0→P3（与导出文件同序）
  const prioOf = (p: string) => {
    const m = /(\d+)/.exec(p ?? "");
    return m ? Number(m[1]) : 9;
  };
  const typeRank = (t: string) => {
    const i = chosenTypes.indexOf(t);
    return i >= 0 ? i : 99;
  };
  const cases: TestCase[] = [...(result?.cases ?? [])].sort(
    (a, b) =>
      typeRank(a.case_type) - typeRank(b.case_type) ||
      prioOf(a.priority) - prioOf(b.priority)
  );

  const toggleCase = (i: number) =>
    setSelected((prev) => ({ ...prev, [i]: !prev[i] }));

  const toggleAll = (on: boolean) => {
    const next: Record<number, boolean> = {};
    cases.forEach((_, i) => {
      next[i] = on;
    });
    setSelected(next);
  };

  const runSelected = async () => {
    const chosen = cases.filter((_, i) => selected[i]);
    if (chosen.length === 0) {
      setError("请先勾选要执行的用例（在右侧「用例清单」中打勾）");
      return;
    }
    setExecBusy(true);
    setError("");
    // 立即反馈：清掉上一轮生成阶段，说明执行形式与耗时点（脚本化是云端调用，需数十秒）
    setExecResult(null);
    setExecItems([]);
    setExecTotal(chosen.length);
    setStages([
      {
        label: "执行准备",
        detail: `已提交 ${chosen.length} 条用例。执行形式：先由云端强模型把用例「脚本化」（约需数十秒），再逐条真实执行——API 用例直接发请求，UI 用例由内置受控浏览器/桌面自动化操作被测项目（需在项目配置里填好 ui_entry / api_base）。`,
      },
    ]);
    try {
      const r = await testRunSelected("自动执行选中用例", chosen, project);
      setExecResult(r);
      if (r?.report_html) await openPath(r.report_html);
    } catch (e) {
      setError(String(e));
    } finally {
      setExecBusy(false);
    }
  };

  return (
    <div className="software-col">
      {project ? (
        <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 10, fontSize: 12, flexWrap: "wrap" }}>
          <span className="software-badge">当前项目</span>
          <span style={{ color: "var(--text)" }}>{project.name}</span>
          <span style={{ color: "var(--text-faint)" }}>· {project.project_type} · {project.env_tag}</span>
          {project.env_tag === "prod" && (
            <span style={{ fontSize: 11, color: "var(--danger)" }}>生产环境：执行将被环境隔离硬门拦截</span>
          )}
        </div>
      ) : (
        <div
          style={{
            marginBottom: 10,
            padding: "6px 10px",
            fontSize: 12,
            borderRadius: 6,
            border: "1px solid var(--warning, #eab308)",
            color: "var(--warning, #eab308)",
          }}
        >
          {execResult
            ? "⚠ 本次执行未绑定被测项目：报告与可复用脚本【没有保存】，重跑前请先到「项目配置」选择或新建项目（Web 项目形态选 web 并填 UI 入口）"
            : "未选择被测项目：执行时不做环境隔离、报告与脚本不会落盘（到「项目配置」选择或新建，Web 项目需填 UI 入口）"}
        </div>
      )}
      <div className="software-sec-title">需求输入（文本 / 文档，二选一）</div>
      <textarea
        className="software-search-input"
        style={{ minHeight: 120, resize: "vertical" }}
        placeholder="粘贴需求描述，例如：登录功能需支持账号密码登录，密码 6-20 位，连续 5 次失败锁定 10 分钟…"
        value={requirement}
        onChange={(e) => setRequirement(e.target.value)}
      />

      <div className="software-row" style={{ marginTop: 8 }}>
        <span className="software-kv">需求文档</span>
        <input
          className="software-search-input"
          style={{ flex: 1 }}
          placeholder="或选择本地 txt/md/csv/docx/pdf 文件路径"
          value={path}
          onChange={(e) => setPath(e.target.value)}
        />
        <button className="software-action" onClick={() => void choose()} title="选择文件">
          浏览
        </button>
      </div>

      <div className="software-row" style={{ marginTop: 10, alignItems: "center", gap: 6, flexWrap: "wrap" }}>
        <span className="software-kv">用例类型</span>
        {CASE_TYPES.map((t) => (
          <button
            key={t}
            onClick={() => setTypeSel((prev) => ({ ...prev, [t]: !prev[t] }))}
            style={{
              padding: "2px 10px",
              fontSize: 11,
              borderRadius: 999,
              border: "1px solid var(--border-soft)",
              background: typeSel[t] ? "var(--accent)" : "transparent",
              color: typeSel[t] ? "#fff" : "var(--text-dim)",
              cursor: "pointer",
            }}
            title={typeSel[t] ? "点击取消该类型" : "点击勾选该类型"}
          >
            {t}
          </button>
        ))}
      </div>

      <div className="software-row" style={{ marginTop: 8, alignItems: "center", gap: 8 }}>
        <span className="software-kv">每类条数</span>
        <input
          type="number"
          min={0}
          max={50}
          value={perType}
          onChange={(e) => setPerType(Math.max(0, Math.min(50, Number(e.target.value) || 0)))}
          className="software-search-input"
          style={{ width: 76 }}
        />
        <span style={{ fontSize: 11, color: "var(--text-faint)" }}>
          0 = 不限（按测试方法论自然覆盖）· 已选 {chosenTypes.length} 类
          {perType > 0 && chosenTypes.length > 0 ? ` · 预计约 ${perType * chosenTypes.length} 条` : ""}
        </span>
      </div>

      <div style={{ display: "flex", gap: 8, marginTop: 10, alignItems: "center" }}>
        <button className="software-primary" onClick={() => void run()} disabled={busy}>
          {busy ? "生成中…" : "生成测试用例"}
        </button>
        <span style={{ fontSize: 11, color: "var(--text-faint)" }}>
          结果同步写入右侧文档窗口
        </span>
      </div>

      {stages.length > 0 && (
        <div
          className="software-pkg"
          style={{ marginTop: 10, flexDirection: "column", alignItems: "stretch" }}
        >
          {stages.map((s, i) => {
            const isLast = i === stages.length - 1;
            const running = busy || execBusy;
            const done = !running || !isLast;
            return (
              <div
                key={s.label}
                style={{ display: "flex", alignItems: "center", gap: 8, padding: "3px 0", fontSize: 12 }}
              >
                <span style={{ width: 16, textAlign: "center", color: done ? "var(--ok, #22c55e)" : "var(--accent)" }}>
                  {done ? "✓" : <span className="baize-spin" style={{ width: 10, height: 10 }} />}
                </span>
                <span style={{ color: "var(--text-dim)", minWidth: 72, flexShrink: 0 }}>{s.label}</span>
                <span style={{ color: "var(--text-faint)", flex: 1, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>
                  {s.detail}
                </span>
              </div>
            );
          })}
        </div>
      )}

      {error && <div className="software-error" style={{ marginTop: 10 }}>{error}</div>}

      {/* 自动执行实时进度：脚本化阶段不定长条纹，逐条执行阶段确定进度 + ✓/✕ 徽标流 */}
      {execBusy && execResult === null && (
        <div
          className="software-pkg"
          style={{ marginTop: 10, flexDirection: "column", alignItems: "stretch", gap: 8 }}
        >
          {execItems.length === 0 ? (
            <>
              <div style={{ display: "flex", alignItems: "center", gap: 8, fontSize: 12, color: "var(--text-dim)" }}>
                <span className="baize-spin" />
                云端脚本化生成中，正在把用例翻译为可执行脚本…
              </div>
              <div className="baize-progress">
                <div className="baize-progress-bar baize-progress-indet" />
              </div>
            </>
          ) : (
            <>
              <div style={{ display: "flex", alignItems: "center", gap: 8, fontSize: 12, flexWrap: "wrap" }}>
                <span style={{ color: "var(--text-dim)" }}>逐条执行中</span>
                <span style={{ color: "var(--text)", fontWeight: 600 }}>
                  {execItems.length}/{execTotal}
                </span>
                <span style={{ fontSize: 11, color: "var(--ok, #22c55e)" }}>
                  通过 {execItems.filter((x) => x.ok).length}
                </span>
                <span style={{ fontSize: 11, color: "var(--danger, #ef4444)" }}>
                  失败 {execItems.length - execItems.filter((x) => x.ok).length}
                </span>
              </div>
              <div className="baize-progress">
                <div
                  className="baize-progress-bar"
                  style={{ width: `${Math.round((execItems.length / Math.max(1, execTotal)) * 100)}%` }}
                />
              </div>
              <div style={{ display: "flex", flexWrap: "wrap", gap: 4 }}>
                {execItems.map((it, i) => (
                  <span
                    key={i}
                    className="baize-chip"
                    title={it.title}
                    style={{ color: it.ok ? "var(--ok, #22c55e)" : "var(--danger, #ef4444)" }}
                  >
                    {it.ok ? "✓" : "✕"} {i + 1}
                  </span>
                ))}
              </div>
            </>
          )}
        </div>
      )}

      {result && (
        <div className="software-pkg" style={{ marginTop: 12, flexDirection: "column", alignItems: "stretch" }}>
          <div style={{ display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap" }}>
            <span className={`software-badge ${result.ok ? "ok" : "warn"}`}>
              {result.ok ? "生成成功" : "生成失败"}
            </span>
            <span style={{ fontSize: 12, color: "var(--text-dim)" }}>
              需求点 {result.requirements} 个 · 测试用例 {result.test_cases} 条
            </span>
          </div>
          {result.coverage && (
            <pre
              style={{
                margin: "8px 0 0",
                padding: "8px",
                background: "rgba(0,0,0,0.25)",
                borderRadius: 6,
                fontSize: 11,
                maxHeight: 160,
                overflow: "auto",
                whiteSpace: "pre-wrap",
                wordBreak: "break-all",
                color: "var(--text-dim)",
              }}
            >
              {result.coverage}
            </pre>
          )}
        </div>
      )}

      {cases.length > 0 && (
        <div className="software-pkg" style={{ marginTop: 12, flexDirection: "column", alignItems: "stretch" }}>
          <div style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
            <span className="software-sec-title" style={{ margin: 0 }}>用例清单</span>
            <span style={{ fontSize: 11, color: "var(--text-faint)" }}>
              已选 {Object.values(selected).filter(Boolean).length} / {cases.length}
            </span>
            <span style={{ flex: 1 }} />
            <button className="software-action" onClick={() => toggleAll(true)}>全选</button>
            <button className="software-action" onClick={() => toggleAll(false)}>全不选</button>
            <span style={{ width: 1, height: 16, background: "var(--border-soft)" }} />
            {EXPORT_FORMATS.map((f) => (
              <button
                key={f.id}
                onClick={() => setExportFmt(f.id)}
                className={exportFmt === f.id ? "software-badge ok" : "software-action"}
                title={`以 ${f.label} 格式导出`}
              >
                {f.label}
              </button>
            ))}
            <button className="software-action" onClick={() => void doExport()} title="勾选了几条就导几条；一条没勾则导出全部">
              保存…
            </button>
            <button
              className="software-primary"
              onClick={() => void runSelected()}
              disabled={execBusy}
            >
              {execBusy ? "执行中…" : "执行选中用例"}
            </button>
          </div>
          {savedPath && (
            <div style={{ marginTop: 6, display: "flex", alignItems: "center", gap: 8, fontSize: 11 }}>
              <span style={{ color: "var(--ok, #22c55e)" }}>✓ 已保存：</span>
              <span
                style={{ color: "var(--text-dim)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}
                title={savedPath}
              >
                {savedPath}
              </span>
              <button className="software-action" onClick={() => void openPath(savedPath)}>
                打开
              </button>
            </div>
          )}
          <div style={{ marginTop: 8, overflow: "auto", maxHeight: 300 }}>
            {cases.map((c, i) => {
              // 类别分节：类型变化处插入「── 类别（N 条）──」分隔标题
              const newSection = i === 0 || cases[i - 1].case_type !== c.case_type;
              return (
                <Fragment key={i}>
                  {newSection && (
                    <div
                      style={{
                        marginTop: i === 0 ? 0 : 8,
                        padding: "3px 4px",
                        fontSize: 11,
                        fontWeight: 600,
                        color: "var(--text-dim)",
                        borderTop: i > 0 ? "1px solid var(--border-soft)" : "none",
                      }}
                    >
                      ── {c.case_type}（{cases.filter((x) => x.case_type === c.case_type).length} 条）──
                    </div>
                  )}
                  <label
                    style={{
                      display: "flex",
                      gap: 8,
                      alignItems: "flex-start",
                      padding: "6px 4px",
                      borderTop: newSection ? "none" : "1px solid var(--border-soft)",
                      cursor: "pointer",
                    }}
                  >
                    <input
                      type="checkbox"
                      checked={!!selected[i]}
                      onChange={() => toggleCase(i)}
                      style={{ marginTop: 2, flexShrink: 0 }}
                    />
                    <div style={{ flex: 1, minWidth: 0 }}>
                      <div style={{ display: "flex", alignItems: "center", gap: 6, flexWrap: "wrap" }}>
                        <span style={{ fontSize: 12, color: "var(--text)" }}>{i + 1}. {c.title}</span>
                        <span className="software-badge" style={{ fontSize: 10 }}>{c.priority}</span>
                      </div>
                      <div style={{ fontSize: 11, color: "var(--text-dim)", marginTop: 2, whiteSpace: "pre-wrap", wordBreak: "break-word" }}>
                        步骤：{c.steps}
                      </div>
                      <div style={{ fontSize: 11, color: "var(--text-faint)", marginTop: 2, whiteSpace: "pre-wrap", wordBreak: "break-word" }}>
                        期望：{c.expected}
                      </div>
                    </div>
                  </label>
                </Fragment>
              );
            })}
          </div>
        </div>
      )}

      {execResult && <SelectedExecResultView result={execResult} />}
    </div>
  );
}

// ─────────── 勾选执行结果 ───────────

function kindLabel(kind: string): string {
  if (kind === "ui") return "UI";
  if (kind === "api") return "接口";
  return "未知";
}

function SelectedExecResultView({ result }: { result: TestRunSelectedResult }) {
  return (
    <div className="software-pkg" style={{ marginTop: 12, flexDirection: "column", alignItems: "stretch" }}>
      <div style={{ display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap" }}>
        <span className={`software-badge ${result.ok ? "ok" : "warn"}`}>
          {result.ok ? "全部通过" : "存在失败"}
        </span>
        <span style={{ fontSize: 12, color: "var(--text-dim)" }}>
          共 {result.total} 例 · 通过 {result.passed} · 失败 {result.failed}
        </span>
        {result.scripts_path && (
          <button
            className="software-action"
            onClick={() => void openPath(result.scripts_path!)}
            title={`脚本已落盘，可在 UI 测试/接口测试页「从文件载入」后再次执行：${result.scripts_path}`}
          >
            可复用脚本
          </button>
        )}
      </div>
      <div style={{ marginTop: 8, overflow: "auto", maxHeight: 280 }}>
        <table style={{ width: "100%", borderCollapse: "collapse", fontSize: 11 }}>
          <thead>
            <tr style={{ color: "var(--text-faint)" }}>
              <th style={th}>#</th>
              <th style={th}>类型</th>
              <th style={th}>用例</th>
              <th style={th}>结果</th>
              <th style={th}>说明</th>
            </tr>
          </thead>
          <tbody>
            {result.results.map((r: SelectedCaseResult) => (
              <tr key={r.index} style={{ borderTop: "1px solid var(--border-soft)" }}>
                <td style={td}>{r.index}</td>
                <td style={td}>{kindLabel(r.kind)}</td>
                <td style={td}>{r.title}</td>
                <td style={td}>{r.ok ? "✓" : "✕"}</td>
                <td style={td}>
                  {r.reason}
                  {(r.ui_steps ?? [])
                    .filter((s) => !s.ok)
                    .map((s, j) => (
                      <div key={`u${j}`} style={{ color: "var(--danger)", marginTop: 2 }}>
                        · 步骤{s.index} {s.detail}
                      </div>
                    ))}
                  {(r.api_cases ?? []).map((c) =>
                    c.checks
                      .filter((ch) => !ch.passed)
                      .map((ch, j) => (
                        <div key={`a${j}`} style={{ color: "var(--danger)", marginTop: 2 }}>
                          · {ch.name}：期望「{ch.expected}」实际「{ch.actual}」
                        </div>
                      ))
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

// ─────────── UI 测试 ───────────

function UiTestView() {
  const [name, setName] = useState("");
  const [steps, setSteps] = useState(UI_STEPS_EXAMPLE);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [result, setResult] = useState<UiTestResult | null>(null);
  // 从本地脚本文件载入（自动执行链路落盘的 *_scripts.json / 手写 ui_steps 文件）
  const [loaded, setLoaded] = useState<{ fileTitle: string; items: LoadableScript[] } | null>(null);
  const loadFile = async () => {
    try {
      const r = await pickAndParseScripts();
      if (!r) return;
      setLoaded(r);
      const uiIdx = r.items.findIndex((s) => s.kind !== "api" && s.ui_steps.length > 0);
      if (uiIdx >= 0) {
        applyFrom(r.items, uiIdx);
      } else if (r.items.some((s) => s.kind === "api")) {
        setError("该文件是接口脚本，请到「接口测试」页载入");
      } else {
        setError("文件中没有可执行的 UI 步骤");
      }
    } catch (e) {
      setError(`载入失败：${String(e)}`);
    }
  };
  /** 把 items[idx] 填入名称与步骤输入框 */
  const applyFrom = (items: LoadableScript[], idx: number) => {
    const it = items[idx];
    if (!it || it.ui_steps.length === 0) return;
    setName(it.title);
    setSteps(JSON.stringify(it.ui_steps, null, 2));
    setResult(null);
    setError("");
  };
  const applyScript = (idx: number) => {
    if (loaded) applyFrom(loaded.items, idx);
  };

  const run = async () => {
    let parsed: unknown;
    try {
      parsed = JSON.parse(steps.trim());
    } catch (e) {
      setError("步骤 JSON 解析失败：" + String(e));
      return;
    }
    setBusy(true);
    setError("");
    setResult(null);
    try {
      const r = await testRunUi(name.trim() || "UI 测试", parsed);
      setResult(r);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="software-col">
      <div className="software-row">
        <span className="software-kv">套件名称</span>
        <input
          className="software-search-input"
          style={{ flex: 1 }}
          placeholder="如：记事本冒烟测试"
          value={name}
          onChange={(e) => setName(e.target.value)}
        />
      </div>

      <div className="software-sec-title" style={{ marginTop: 8 }}>
        步骤（JSON 数组）
      </div>
      <textarea
        className="software-search-input"
        style={{ minHeight: 160, resize: "vertical", fontFamily: "monospace" }}
        value={steps}
        onChange={(e) => setSteps(e.target.value)}
      />
      <div style={{ display: "flex", gap: 6, marginTop: 6, flexWrap: "wrap" }}>
        <button className="software-action" onClick={() => setSteps(UI_STEPS_EXAMPLE)}>
          填入示例
        </button>
        <button className="software-action" onClick={() => setSteps(WEB_STEPS_EXAMPLE)}>
          填入 Web 示例
        </button>
        <button className="software-action" onClick={() => void loadFile()} title="载入自动执行落盘的 *_scripts.json（或手写 ui_steps 文件），解析后填入下方">
          从文件载入…
        </button>
        <span style={{ fontSize: 10, color: "var(--text-faint)", lineHeight: 2 }}>
          桌面：wait / click_element(name) / click_at(x,y) / type_text(text) / paste_text /
          key_press(keys) / window_focus(name) / confirm_dialog /
          assert(window_title|ocr_text|visual_target)；Web：open_page(url) /
          click_selector(selector) / fill_input(selector,text) / click_text(可见文字) /
          assert_page_text(text)
        </span>
      </div>

      <div style={{ display: "flex", gap: 8, marginTop: 10 }}>
        {loaded && loaded.items.some((s) => s.kind !== "api" && s.ui_steps.length > 0) && (
          <select
            className="software-search-input"
            style={{ maxWidth: 260 }}
            value=""
            onChange={(e) => {
              if (e.target.value !== "") applyScript(Number(e.target.value));
            }}
            title={loaded.fileTitle}
          >
            <option value="">已载入：{loaded.fileTitle || "脚本文件"}（选择一条填入）</option>
            {loaded.items.map((s, i) =>
              s.kind !== "api" && s.ui_steps.length > 0 ? (
                <option key={i} value={i}>
                  UI · {s.title}
                </option>
              ) : null
            )}
          </select>
        )}
        <button className="software-primary" onClick={() => void run()} disabled={busy}>
          {busy ? "执行中…" : "执行 UI 测试"}
        </button>
        <button className="software-refresh" onClick={() => setResult(null)}>
          清空结果
        </button>
      </div>

      {error && <div className="software-error" style={{ marginTop: 10 }}>{error}</div>}

      {result && <UiResultView result={result} />}
    </div>
  );
}

function UiResultView({ result }: { result: UiTestResult }) {
  return (
    <div className="software-pkg" style={{ marginTop: 12, flexDirection: "column", alignItems: "stretch" }}>
      <div style={{ display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap" }}>
        <span className={`software-badge ${result.ok ? "ok" : "warn"}`}>
          {result.ok ? "全部通过" : "存在失败"}
        </span>
        <span style={{ fontSize: 12, color: "var(--text-dim)" }}>
          共 {result.total} 步 · 通过 {result.passed} · 失败 {result.failed}
        </span>
      </div>
      <div style={{ marginTop: 8, overflow: "auto", maxHeight: 240 }}>
        <table style={{ width: "100%", borderCollapse: "collapse", fontSize: 11 }}>
          <thead>
            <tr style={{ color: "var(--text-faint)" }}>
              <th style={th}>#</th>
              <th style={th}>动作</th>
              <th style={th}>结果</th>
              <th style={th}>详情</th>
            </tr>
          </thead>
          <tbody>
            {result.steps.map((s: UiStepResult) => (
              <tr key={s.index} style={{ borderTop: "1px solid var(--border-soft)" }}>
                <td style={td}>{s.index}</td>
                <td style={td}>{s.action}</td>
                <td style={td}>{s.ok ? "✓" : "✕"}</td>
                <td style={td}>
                  {s.detail}
                  {(s.checks ?? []).map((c, i) => (
                    <div key={i} style={{ color: c.passed ? "var(--text-dim)" : "var(--danger)", marginTop: 2 }}>
                      · {c.name}：期望「{c.expected}」实际「{c.actual}」
                    </div>
                  ))}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

// ─────────── 接口测试 ───────────

function ApiTestView({ project }: { project: ProjectProfile | null }) {
  const [name, setName] = useState("");
  const [requests, setRequests] = useState(API_REQUESTS_EXAMPLE);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [result, setResult] = useState<ApiTestResult | null>(null);
  // openapi/swagger 导入：默认取当前项目的 api_doc / api_base
  const [apiDoc, setApiDoc] = useState(project?.api_doc ?? "");
  const [apiBase, setApiBase] = useState(project?.api_base ?? "");
  const [importing, setImporting] = useState(false);
  // 从本地脚本文件载入（自动执行链路落盘的 *_scripts.json / 手写 api_requests 文件）
  const [loaded, setLoaded] = useState<{ fileTitle: string; items: LoadableScript[] } | null>(null);
  const [auxHint, setAuxHint] = useState("");
  const loadFile = async () => {
    try {
      const r = await pickAndParseScripts();
      if (!r) return;
      setLoaded(r);
      const apiIdx = r.items.findIndex((s) => s.kind === "api" && s.api_requests.length > 0);
      if (apiIdx >= 0) {
        applyFrom(r.items, apiIdx);
      } else if (r.items.some((s) => s.kind !== "api")) {
        setError("该文件是 UI 脚本，请到「UI 测试」页载入");
      } else {
        setError("文件中没有可执行的接口请求");
      }
    } catch (e) {
      setError(`载入失败：${String(e)}`);
    }
  };
  /** 把 items[idx] 填入名称与请求编辑区；setup/teardown 仅在自动链路生效，这里提示条数 */
  const applyFrom = (items: LoadableScript[], idx: number) => {
    const it = items[idx];
    if (!it || it.api_requests.length === 0) return;
    setName(it.title);
    setRequests(JSON.stringify(it.api_requests, null, 2));
    setAuxHint(
      (it.setup?.length ?? 0) + (it.teardown?.length ?? 0) > 0
        ? `该脚本含数据准备 ${it.setup?.length ?? 0} 条 / 数据清理 ${it.teardown?.length ?? 0} 条，仅在用例生成页的批量执行中生效`
        : ""
    );
    setResult(null);
    setError("");
  };
  const applyScript = (idx: number) => {
    if (loaded) applyFrom(loaded.items, idx);
  };

  const run = async () => {
    let parsed: unknown;
    try {
      parsed = JSON.parse(requests.trim());
    } catch (e) {
      setError("请求 JSON 解析失败：" + String(e));
      return;
    }
    setBusy(true);
    setError("");
    setResult(null);
    try {
      const r = await testRunApi(name.trim() || "接口测试", parsed);
      setResult(r);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  // 从 openapi/swagger 文档直出接口用例，填充到请求编辑区
  const importDoc = async () => {
    if (!apiDoc.trim()) {
      setError("请填写 openapi/swagger 文档地址或本地路径");
      return;
    }
    setImporting(true);
    setError("");
    try {
      const r = await testImportOpenapi(apiDoc.trim(), apiBase.trim());
      setRequests(JSON.stringify(r.cases, null, 2));
      if (!name.trim() && project?.name) setName(`${project.name} 接口测试`);
    } catch (e) {
      setError(String(e));
    } finally {
      setImporting(false);
    }
  };

  return (
    <div className="software-col">
      <div className="software-row">
        <span className="software-kv">套件名称</span>
        <input
          className="software-search-input"
          style={{ flex: 1 }}
          placeholder="如：用户模块接口测试"
          value={name}
          onChange={(e) => setName(e.target.value)}
        />
      </div>

      {/* openapi / swagger 直出用例 */}
      <div className="software-sec-title" style={{ marginTop: 8 }}>
        文档导入（openapi / swagger）
      </div>
      <input
        className="software-search-input"
        placeholder="文档 URL 或本地路径（JSON 格式），如 http://host/v3/api-docs"
        value={apiDoc}
        onChange={(e) => setApiDoc(e.target.value)}
      />
      <input
        className="software-search-input"
        style={{ marginTop: 6 }}
        placeholder="接口 base_url（留空则使用文档内 servers 地址）"
        value={apiBase}
        onChange={(e) => setApiBase(e.target.value)}
      />
      <div style={{ display: "flex", gap: 6, marginTop: 6, alignItems: "center" }}>
        <button className="software-action" onClick={() => void importDoc()} disabled={importing}>
          {importing ? "导入中…" : "导入生成用例"}
        </button>
        <span style={{ fontSize: 10, color: "var(--text-faint)" }}>
          自动展开 paths，路径参数以 1 占位，期望状态码取首个 2xx 响应
        </span>
      </div>

      <div className="software-sec-title" style={{ marginTop: 8 }}>
        请求用例（JSON 数组）
      </div>
      <textarea
        className="software-search-input"
        style={{ minHeight: 160, resize: "vertical", fontFamily: "monospace" }}
        value={requests}
        onChange={(e) => setRequests(e.target.value)}
      />
      <div style={{ display: "flex", gap: 6, marginTop: 6, flexWrap: "wrap" }}>
        <button className="software-action" onClick={() => setRequests(API_REQUESTS_EXAMPLE)}>
          填入示例
        </button>
        <button className="software-action" onClick={() => void loadFile()} title="载入自动执行落盘的 *_scripts.json（或手写 api_requests 文件），解析后填入上方">
          从文件载入…
        </button>
        <span style={{ fontSize: 10, color: "var(--text-faint)", lineHeight: 2 }}>
          字段：method/url/headers/body/timeout_secs；断言：expect_status / expect_body_contains /
          expect_body_not_contains / expect_json
        </span>
      </div>
      {auxHint && (
        <div style={{ fontSize: 11, color: "var(--text-dim)", marginTop: 4 }}>{auxHint}</div>
      )}

      <div style={{ display: "flex", gap: 8, marginTop: 10 }}>
        {loaded && loaded.items.some((s) => s.kind === "api" && s.api_requests.length > 0) && (
          <select
            className="software-search-input"
            style={{ maxWidth: 260 }}
            value=""
            onChange={(e) => {
              if (e.target.value !== "") applyScript(Number(e.target.value));
            }}
            title={loaded.fileTitle}
          >
            <option value="">已载入：{loaded.fileTitle || "脚本文件"}（选择一条填入）</option>
            {loaded.items.map((s, i) =>
              s.kind === "api" && s.api_requests.length > 0 ? (
                <option key={i} value={i}>
                  接口 · {s.title}
                </option>
              ) : null
            )}
          </select>
        )}
        <button className="software-primary" onClick={() => void run()} disabled={busy}>
          {busy ? "执行中…" : "执行接口测试"}
        </button>
        <button className="software-refresh" onClick={() => setResult(null)}>
          清空结果
        </button>
      </div>

      {error && <div className="software-error" style={{ marginTop: 10 }}>{error}</div>}

      {result && <ApiResultView result={result} />}
    </div>
  );
}

function ApiResultView({ result }: { result: ApiTestResult }) {
  return (
    <div className="software-pkg" style={{ marginTop: 12, flexDirection: "column", alignItems: "stretch" }}>
      <div style={{ display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap" }}>
        <span className={`software-badge ${result.ok ? "ok" : "warn"}`}>
          {result.ok ? "全部通过" : "存在失败"}
        </span>
        <span style={{ fontSize: 12, color: "var(--text-dim)" }}>
          共 {result.total} 例 · 通过 {result.passed} · 失败 {result.failed}
        </span>
      </div>
      <div style={{ marginTop: 8, overflow: "auto", maxHeight: 240 }}>
        <table style={{ width: "100%", borderCollapse: "collapse", fontSize: 11 }}>
          <thead>
            <tr style={{ color: "var(--text-faint)" }}>
              <th style={th}>#</th>
              <th style={th}>用例</th>
              <th style={th}>方法</th>
              <th style={th}>状态码</th>
              <th style={th}>结果</th>
            </tr>
          </thead>
          <tbody>
            {result.cases.map((c: ApiCaseResult, i: number) => (
              <tr key={i} style={{ borderTop: "1px solid var(--border-soft)" }}>
                <td style={td}>{i + 1}</td>
                <td style={td}>
                  {c.name || c.url}
                  {c.checks.map((ch, j) => (
                    <div key={j} style={{ color: ch.passed ? "var(--text-dim)" : "var(--danger)", marginTop: 2 }}>
                      · {ch.name}：期望「{ch.expected}」实际「{ch.actual}」
                    </div>
                  ))}
                </td>
                <td style={td}>{c.method}</td>
                <td style={td}>{c.status}</td>
                <td style={td}>{c.ok ? "✓" : "✕"}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

// 表格单元格基础样式
const th: CSSProperties = {
  textAlign: "left",
  padding: "4px 8px",
  fontWeight: 500,
  whiteSpace: "nowrap",
};
const td: CSSProperties = {
  padding: "4px 8px",
  verticalAlign: "top",
  color: "var(--text-dim)",
};