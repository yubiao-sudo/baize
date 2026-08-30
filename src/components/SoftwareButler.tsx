import { useCallback, useEffect, useState } from "react";
import { checkDocumentDeps, diskInfo, envCheck, softwareList, softwareSearch, systemGet } from "../api";
import type { DiskInfo, DocumentDepsReport, EnvCheckResult, SoftwarePackage, SoftwareSearchResult, SystemConfig } from "../types";
import { useChat } from "../stores/chat";

type Tab = "env" | "search" | "installed" | "system";

const TABS: { id: Tab; label: string }[] = [
  { id: "env", label: "环境探测" },
  { id: "search", label: "找软件" },
  { id: "installed", label: "已装软件" },
  { id: "system", label: "系统配置" },
];

export default function SoftwareButler({ onClose }: { onClose: () => void }) {
  const [tab, setTab] = useState<Tab>("env");
  const [env, setEnv] = useState<EnvCheckResult | null>(null);
  const [disk, setDisk] = useState<DiskInfo | null>(null);
  const [searchQ, setSearchQ] = useState("");
  const [search, setSearch] = useState<SoftwareSearchResult | null>(null);
  const [installed, setInstalled] = useState<SoftwareSearchResult | null>(null);
  const [sys, setSys] = useState<SystemConfig | null>(null);
  const [docDeps, setDocDeps] = useState<DocumentDepsReport | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  const loadEnv = useCallback(async () => {
    setBusy(true);
    setError("");
    try {
      const [e, d, deps] = await Promise.all([
        envCheck(),
        diskInfo(),
        checkDocumentDeps(),
      ]);
      setEnv(e);
      setDisk(d);
      setDocDeps(deps);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, []);

  const loadInstalled = useCallback(async () => {
    setBusy(true);
    setError("");
    try {
      setInstalled(await softwareList());
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, []);

  const loadSystem = useCallback(async () => {
    setBusy(true);
    setError("");
    try {
      setSys(await systemGet());
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, []);

  const doSearch = async () => {
    const q = searchQ.trim();
    if (!q) return;
    setBusy(true);
    setError("");
    try {
      setSearch(await softwareSearch(q));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  // 切换到对应标签时按需加载（懒加载，避免无谓调用命令行）
  useEffect(() => {
    if (tab === "env" && !env) void loadEnv();
    if (tab === "installed" && !installed) void loadInstalled();
    if (tab === "system" && !sys) void loadSystem();
  }, [tab, env, installed, sys, loadEnv, loadInstalled, loadSystem]);

  /** 安装 / 卸载走聊天 → 智能体（软件管家工具集 + 高危审批链路） */
  const request = (verb: "install" | "uninstall", pkg: SoftwarePackage) => {
    const msg =
      verb === "install"
        ? `请用软件管家安装 ${pkg.name}（id: ${pkg.id}）`
        : `请用软件管家卸载 ${pkg.name}（id: ${pkg.id}）`;
    onClose();
    void useChat.getState().send(msg);
  };

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
          <h3 style={{ margin: 0, fontSize: 15, letterSpacing: 1 }}>软件管家</h3>
          <span style={{ fontSize: 11, color: "var(--text-faint)" }}>
            找软件 · 装软件 · 配置系统
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

        {/* 主体（可滚动） */}
        <div className="software-body">
          {error && <div className="software-error">{error}</div>}

          {tab === "env" && (
            <EnvPanel env={env} disk={disk} docDeps={docDeps} busy={busy} onRefresh={() => void loadEnv()} />
          )}

          {tab === "search" && (
            <SearchPanel
              query={searchQ}
              setQuery={setSearchQ}
              result={search}
              busy={busy}
              onSearch={() => void doSearch()}
              onInstall={(p) => request("install", p)}
            />
          )}

          {tab === "installed" && (
            <PackageList
              result={installed}
              busy={busy}
              empty="尚未通过包管理器安装软件，或未检测到包管理器"
              onRefresh={() => void loadInstalled()}
              actionLabel="卸载"
              onAction={(p) => request("uninstall", p)}
            />
          )}

          {tab === "system" && (
            <SystemPanel sys={sys} busy={busy} onRefresh={() => void loadSystem()} />
          )}
        </div>
      </div>
    </div>
  );
}

// ─────────── 环境探测 ───────────
function EnvPanel({
  env,
  disk,
  docDeps,
  busy,
  onRefresh,
}: {
  env: EnvCheckResult | null;
  disk: DiskInfo | null;
  docDeps: DocumentDepsReport | null;
  busy: boolean;
  onRefresh: () => void;
}) {
  if (!env)
    return <div style={{ color: "var(--text-dim)" }}>{busy ? "探测中…" : "尚未探测"}</div>;

  const pmList = env.package_managers ?? [];
  const runtimes = env.runtimes ?? {};
  const disks = disk?.disks ?? [];
  const root = disk?.install_root;

  return (
    <div className="software-col">
      <div className="software-row">
        <span className="software-kv">操作系统</span>
        <span className="software-val">{env.os}</span>
      </div>
      <div className="software-row">
        <span className="software-kv">管理员权限</span>
        <span className={`software-badge ${env.is_admin ? "ok" : "warn"}`}>
          {env.is_admin ? "已提升" : "普通用户"}
        </span>
      </div>

      <div className="software-sec-title">包管理器</div>
      {pmList.length === 0 ? (
        <div style={{ color: "var(--text-faint)", fontSize: 12 }}>未检测到可用包管理器</div>
      ) : (
        pmList.map((pm) => (
          <div className="software-row" key={pm.id}>
            <span className="software-kv">{pm.label}</span>
            <span className={`software-badge ${pm.available ? "ok" : ""}`}>
              {pm.available ? "可用" : "未安装"}
            </span>
          </div>
        ))
      )}

      <div className="software-hint">
        winget 是 Windows 自带的包管理器，已足够搜索与安装软件；choco、scoop 是可选替代品，未安装不影响使用。
      </div>

      <div className="software-sec-title">磁盘空间</div>
      {disks.length === 0 ? (
        <div style={{ color: "var(--text-faint)", fontSize: 12 }}>暂无磁盘信息</div>
      ) : (
        disks.map((d) => (
          <div className="software-row" key={d.drive}>
            <span className="software-kv">{d.drive}</span>
            <span className="software-val monotone">
              剩余 {d.free_gb ?? 0} GB / 共 {d.total_gb ?? 0} GB
            </span>
          </div>
        ))
      )}

      {root && (
        <>
          <div className="software-sec-title">推荐安装位置</div>
          <div className="software-row">
            <span className="software-kv">安装盘</span>
            <span className="software-val monotone" style={{ color: "var(--cyan)" }}>
              {root.path}
            </span>
          </div>
          <div className="software-reason">{root.reason}</div>
        </>
      )}

      <div className="software-sec-title">运行时</div>
      {Object.keys(runtimes).length === 0 ? (
        <div style={{ color: "var(--text-faint)", fontSize: 12 }}>无</div>
      ) : (
        Object.entries(runtimes).map(([k, v]) => (
          <div className="software-row" key={k}>
            <span className="software-kv">{k}</span>
            <span className="software-val" style={{ fontFamily: "monospace" }}>
              {v ?? "—"}
            </span>
          </div>
        ))
      )}

      <div className="software-sec-title">文档解析依赖</div>
      {docDeps ? (
        <DocDepsBlock deps={docDeps} />
      ) : (
        <div style={{ color: "var(--text-faint)", fontSize: 12 }}>检测中…</div>
      )}

      <button className="software-refresh" onClick={onRefresh}>
        重新探测
      </button>
    </div>
  );
}

// ─────────── 文档解析依赖（read_document 的 Python 运行环境） ───────────
function DocDepsBlock({ deps }: { deps: DocumentDepsReport }) {
  const [copied, setCopied] = useState(false);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(deps.install_command);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      /* 忽略剪贴板失败，命令已明文展示 */
    }
  };

  return (
    <div className="software-col">
      <div className="software-row">
        <span className="software-kv">Python</span>
        <span className="software-val monotone">{deps.python ?? "未检测到"}</span>
        <span className={`software-badge ${deps.ready ? "ok" : "warn"}`}>
          {deps.ready ? "已就绪" : deps.python ? "缺解析库" : "未安装"}
        </span>
      </div>

      {deps.missing.length > 0 && (
        <div className="software-row">
          <span className="software-kv">缺失库</span>
          <span className="software-val monotone">{deps.missing.join(" · ")}</span>
        </div>
      )}

      {!deps.ready && (
        <>
          <div className="software-row">
            <span className="software-kv">安装命令</span>
            <span className="software-action" onClick={copy} role="button">
              {copied ? "已复制" : "复制"}
            </span>
          </div>
          <code className="software-cmd">{deps.install_command}</code>
          <div className="software-hint">
            文档解析（read_document）依赖 Python 与上述库；安装后即可读取 PDF / Word / Excel / PPT 并导出表格与图片。
          </div>
        </>
      )}
    </div>
  );
}

// ─────────── 找软件 ───────────
function SearchPanel({
  query,
  setQuery,
  result,
  busy,
  onSearch,
  onInstall,
}: {
  query: string;
  setQuery: (s: string) => void;
  result: SoftwareSearchResult | null;
  busy: boolean;
  onSearch: () => void;
  onInstall: (p: SoftwarePackage) => void;
}) {
  return (
    <div className="software-col">
      <div className="software-searchbar">
        <input
          className="software-search-input"
          placeholder="输入软件名或关键词，如 vscode / chrome / python"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && !busy && onSearch()}
        />
        <button className="software-primary" onClick={onSearch} disabled={busy || !query.trim()}>
          搜索
        </button>
      </div>
      {result && (
        <div style={{ fontSize: 11, color: "var(--text-faint)", marginTop: 4 }}>
          来源包管理器 · {result.pm}，共 {result.packages.length} 条
        </div>
      )}
      <PackageList
        result={result}
        busy={busy}
        empty="输入关键词开始搜索"
        actionLabel="安装"
        onAction={onInstall}
      />
    </div>
  );
}

// ─────────── 软件包列表（搜索 / 已装共用） ───────────
function PackageList({
  result,
  busy,
  empty,
  actionLabel,
  onAction,
  onRefresh,
}: {
  result: SoftwareSearchResult | null;
  busy: boolean;
  empty: string;
  actionLabel: string;
  onAction: (p: SoftwarePackage) => void;
  onRefresh?: () => void;
}) {
  if (busy) return <div style={{ color: "var(--text-dim)", marginTop: 8 }}>查询中…</div>;
  if (!result) return <div style={{ color: "var(--text-faint)", marginTop: 8 }}>{empty}</div>;
  if (result.packages.length === 0)
    return (
      <div style={{ color: "var(--text-faint)", marginTop: 8 }}>
        无结果。
        {onRefresh && (
          <button className="software-refresh" onClick={onRefresh} style={{ marginLeft: 8 }}>
            刷新
          </button>
        )}
      </div>
    );

  return (
    <div className="software-pkg-list">
      {result.packages.map((p, i) => {
        const sub = p.publisher || (p.id && p.id !== p.name ? p.id : "");
        return (
          <div className="software-pkg" key={`${p.id}-${i}`}>
            <div className="software-pkg-main">
              <span className="software-pkg-name" title={p.name}>
                {p.name}
              </span>
              {sub && (
                <span className="software-pkg-id" title={sub}>
                  {sub}
                </span>
              )}
            </div>
            <div className="software-pkg-side">
              {p.version && (
                <span className="software-pkg-ver" title={p.version}>
                  {p.version}
                </span>
              )}
              <button className="software-action" onClick={() => onAction(p)}>
                {actionLabel}
              </button>
            </div>
          </div>
        );
      })}
    </div>
  );
}

// ─────────── 系统配置 ───────────
function SystemPanel({
  sys,
  busy,
  onRefresh,
}: {
  sys: SystemConfig | null;
  busy: boolean;
  onRefresh: () => void;
}) {
  if (!sys)
    return <div style={{ color: "var(--text-dim)" }}>{busy ? "读取中…" : "尚未读取"}</div>;

  const userEnv = sys.env ?? {};
  const machineEnv = sys.machine_env ?? {};
  const userPath = sys.path ?? [];
  const machinePath = sys.machine_path ?? [];
  const userStartup = sys.startup ?? {};
  const machineStartup = sys.machine_startup ?? {};

  const kvList = (obj: Record<string, string | undefined>) =>
    Object.keys(obj).map((k) => (
      <div className="software-row" key={k}>
        <span className="software-kv">{k}</span>
        <span className="software-val monotone" title={obj[k] ?? ""}>
          {obj[k] ?? ""}
        </span>
      </div>
    ));

  const pathList = (paths: string[]) =>
    paths.map((p, i) => (
      <li key={`${p}-${i}`} title={p}>
        {p}
      </li>
    ));

  return (
    <div className="software-col">
      <div className="software-row">
        <span className="software-kv">系统</span>
        <span className="software-val">
          {sys.os} · {sys.os_version || "—"}
        </span>
      </div>

      <div className="software-sec-title">用户环境变量（{Object.keys(userEnv).length}）</div>
      {Object.keys(userEnv).length === 0 ? (
        <div style={{ color: "var(--text-faint)", fontSize: 12 }}>无</div>
      ) : (
        <div className="software-kvlist">{kvList(userEnv)}</div>
      )}

      <div className="software-sec-title">系统环境变量（{Object.keys(machineEnv).length}）</div>
      {Object.keys(machineEnv).length === 0 ? (
        <div style={{ color: "var(--text-faint)", fontSize: 12 }}>无</div>
      ) : (
        <div className="software-kvlist">{kvList(machineEnv)}</div>
      )}

      <div className="software-sec-title">用户 PATH（{userPath.length}）</div>
      {userPath.length === 0 ? (
        <div style={{ color: "var(--text-faint)", fontSize: 12 }}>无</div>
      ) : (
        <ul className="software-path">{pathList(userPath)}</ul>
      )}

      <div className="software-sec-title">系统 PATH（{machinePath.length}）</div>
      {machinePath.length === 0 ? (
        <div style={{ color: "var(--text-faint)", fontSize: 12 }}>无</div>
      ) : (
        <ul className="software-path">{pathList(machinePath)}</ul>
      )}

      <div className="software-sec-title">用户启动项（{Object.keys(userStartup).length}）</div>
      {Object.keys(userStartup).length === 0 ? (
        <div style={{ color: "var(--text-faint)", fontSize: 12 }}>无</div>
      ) : (
        <div className="software-kvlist">{kvList(userStartup)}</div>
      )}

      <div className="software-sec-title">系统启动项（{Object.keys(machineStartup).length}）</div>
      {Object.keys(machineStartup).length === 0 ? (
        <div style={{ color: "var(--text-faint)", fontSize: 12 }}>无</div>
      ) : (
        <div className="software-kvlist">{kvList(machineStartup)}</div>
      )}

      <div style={{ fontSize: 11, color: "var(--text-faint)", marginTop: 4 }}>
        配置环境变量 / PATH / 启动项可在对话中让白泽用 system_set 修改（需授权）。
      </div>
      <button className="software-refresh" onClick={onRefresh}>
        ↻ 重新读取
      </button>
    </div>
  );
}