import { useCallback, useEffect, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { browserAct } from "../api";
import type { ChromeTabInfo, ChromeTabsSummary } from "../types";

type LogEntry = { ts: number; text: string; ok: boolean };

interface LookResult {
  ok?: boolean;
  url?: string;
  title?: string;
  screenshot?: string;
  text?: string;
}

/** 毫秒时间戳 → HH:MM:SS */
function fmtTime(ms: number): string {
  const d = new Date(ms);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

/**
 * 桌面 Chrome 操控面板：实时展示标签页列表 / 页面截图 / OCR 文字，
 * 支持手动控制桌面谷歌浏览器（goto / back / forward / reload / new_tab / 截图 / click_text）。
 * 通过 browser_act 命令与后端 BrowserActTool 共用同一动作集。
 */
export default function ChromePanel({ onClose }: { onClose: () => void }) {
  const [tabs, setTabs] = useState<ChromeTabInfo[]>([]);
  const [activeId, setActiveId] = useState("");
  const [url, setUrl] = useState("");
  const [target, setTarget] = useState("");
  const [shot, setShot] = useState<{ src: string; text: string; title: string; url: string } | null>(
    null
  );
  const [log, setLog] = useState<LogEntry[]>([]);
  /** scan 结果：页面上已用编号线框标出的可交互元素（点编号即 click_mark） */
  const [marks, setMarks] = useState<{ n: number; tag: string; text: string }[]>([]);
  const [markScope, setMarkScope] = useState("");
  /** 视口外还有多少可交互元素——提示「要不要滚下去找」，而不是盲滚一遍 */
  const [offView, setOffView] = useState<{ above: number; below: number } | null>(null);
  /** 标签页概览：白泽自己开的 / 机主自己开的 / 空白可清理 */
  const [tabSummary, setTabSummary] = useState<ChromeTabsSummary | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [auto, setAuto] = useState(false);
  const logBoxRef = useRef<HTMLDivElement | null>(null);
  const autoTimerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const pushLog = useCallback((text: string, ok = true) => {
    setLog((prev) => [...prev.slice(-99), { ts: Date.now(), text, ok }]);
  }, []);

  useEffect(() => {
    logBoxRef.current?.scrollTo({ top: logBoxRef.current.scrollHeight });
  }, [log]);

  const run = useCallback(
    async (args: Record<string, unknown>, logText: string) => {
      setBusy(true);
      setError("");
      try {
        const r = await browserAct(args);
        pushLog(logText, true);
        return r;
      } catch (e) {
        pushLog(`${logText}：${String(e)}`, false);
        setError(String(e));
        return null;
      } finally {
        setBusy(false);
      }
    },
    [pushLog]
  );

  const loadTabs = useCallback(async () => {
    const r = await run({ action: "tabs" }, "列出标签页");
    if (!r) return;
    const list = (r as { tabs?: ChromeTabInfo[] }).tabs ?? [];
    setTabs(list);
    setTabSummary((r as { summary?: ChromeTabsSummary }).summary ?? null);
    const active = list.find((t) => t.active);
    setActiveId(active ? active.id : list.length > 0 ? list[list.length - 1].id : "");
    return list;
  }, [run]);

  const look = useCallback(async () => {
    const r = await browserAct<LookResult>({ action: "look" });
    if (r?.screenshot) {
      setShot({
        src: convertFileSrc(r.screenshot),
        text: r.text ?? "",
        title: r.title ?? "",
        url: r.url ?? "",
      });
      if (r.url) setUrl(r.url);
    }
    return r;
  }, []);

  const refresh = useCallback(async () => {
    await loadTabs();
    await look();
  }, [loadTabs, look]);

  useEffect(() => {
    void refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 自动刷新：定时重新截屏，实现「实时」预览
  useEffect(() => {
    if (auto) {
      autoTimerRef.current = setInterval(() => {
        void look();
      }, 3000);
    } else if (autoTimerRef.current) {
      clearInterval(autoTimerRef.current);
      autoTimerRef.current = null;
    }
    return () => {
      if (autoTimerRef.current) clearInterval(autoTimerRef.current);
    };
  }, [auto, look]);

  const gotoUrl = async () => {
    const u = url.trim();
    if (!u) return;
    await run({ action: "goto", url: u }, `打开 ${u}`);
    await refresh();
  };

  const back = async () => {
    await run({ action: "back" }, "后退");
    await refresh();
  };
  const forward = async () => {
    await run({ action: "forward" }, "前进");
    await refresh();
  };
  const reload = async () => {
    await run({ action: "reload" }, "刷新页面");
    await refresh();
  };
  const newTab = async () => {
    await run({ action: "new_tab" }, "新建标签页");
    await refresh();
  };
  const screenshot = async () => {
    await look();
    pushLog("截图", true);
  };
  const clickText = async () => {
    const t = target.trim();
    if (!t) return;
    await run({ action: "click_text", target: t }, `按文字点击「${t}」`);
    await new Promise((r) => setTimeout(r, 300));
    await refresh();
  };
  /** 只定位不点击：在网页里把目标元素用一条实线框出来，确认「指的就是它」 */
  const highlight = async () => {
    const t = target.trim();
    if (!t) return;
    await run({ action: "highlight", target: t }, `定位「${t}」（已在网页里框出）`);
    await new Promise((r) => setTimeout(r, 300));
    await refresh();
  };
  /**
   * 扫描当前页面（或指定模块）的可交互元素：后端在网页里一次性标出编号线框并回传清单。
   * 纯 DOM 枚举 + 页内画框，不截图、不跑 OCR，通常几十毫秒。
   */
  const scan = async () => {
    const scope = target.trim();
    const r = await run(
      scope ? { action: "scan", scope } : { action: "scan" },
      scope ? `扫描「${scope}」内的可交互元素` : "扫描当前视口内可交互元素"
    );
    if (!r) return;
    const items = (r as { items?: { n: number; tag: string; text: string }[] }).items ?? [];
    setMarks(items);
    setMarkScope(String((r as { scope?: string }).scope ?? ""));
    const off = (r as { off_view?: { above: number; below: number } }).off_view ?? null;
    setOffView(off);
    await refresh();
  };
  /** 按编号点击（编号与元素绑定，不会点错） */
  const clickMark = async (n: number) => {
    await run({ action: "click_mark", index: n }, `点击编号 ${n}`);
    await new Promise((r) => setTimeout(r, 300));
    // 页面可能已经变了：旧编号作废，清掉清单免得误点
    setMarks([]);
    setOffView(null);
    await refresh();
  };
  const switchTab = async (id: string) => {
    await run({ action: "switch_tab", tab_id: id }, `切换标签页`);
    setActiveId(id);
    await refresh();
  };
  const closeTab = async (id: string) => {
    await run({ action: "close_tab", tab_id: id }, "关闭标签页");
    await loadTabs();
    await look();
  };
  /**
   * 收尾清理：一次关掉「白泽自己开的空白标签页」，给浏览器减负。
   * 机主自己开的页面（ours=false）后端一律不动，所以这里不需要二次确认。
   */
  const tidyTabs = async () => {
    const r = await run({ action: "tidy_tabs" }, "清理空白标签页");
    const n = (r as { closed_count?: number } | null)?.closed_count ?? 0;
    pushLog(n > 0 ? `已清理 ${n} 个空白标签页` : "没有可清理的空白标签页", true);
    await loadTabs();
    await look();
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
          <h3 style={{ margin: 0, fontSize: 15, letterSpacing: 1 }}>桌面 Chrome 操控</h3>
          <span style={{ fontSize: 11, color: "var(--text-faint)" }}>
            {tabs.length} 个标签页 · 持久化登录态
          </span>
          <span style={{ flex: 1 }} />
          <button
            className="software-action"
            style={{
              background: auto ? "var(--cyan)" : "transparent",
              color: auto ? "#000" : "var(--text-dim)",
              minWidth: 64,
            }}
            onClick={() => setAuto((v) => !v)}
            title="每 3 秒自动截屏刷新预览"
          >
            {auto ? "实时中" : "实时预览"}
          </button>
          <button className="software-close" onClick={onClose} title="关闭">
            ×
          </button>
        </div>

        {/* 控制栏 */}
        <div style={{ padding: "10px 14px", borderBottom: "1px solid var(--border-soft)" }}>
          <div style={{ display: "flex", gap: 6, alignItems: "center" }}>
            <input
              className="software-search-input"
              style={{ flex: 1 }}
              placeholder="输入网址，如 https://example.com"
              value={url}
              onChange={(e) => setUrl(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void gotoUrl();
              }}
            />
            <button className="software-primary" onClick={() => void gotoUrl()} disabled={busy}>
              打开
            </button>
            <button className="software-action" onClick={() => void back()} disabled={busy} title="后退">
              ←
            </button>
            <button className="software-action" onClick={() => void forward()} disabled={busy} title="前进">
              →
            </button>
            <button className="software-action" onClick={() => void reload()} disabled={busy} title="刷新">
              ↻
            </button>
            <button className="software-action" onClick={() => void newTab()} disabled={busy} title="新建标签页">
              ＋ 标签
            </button>
            <button className="software-action" onClick={() => void screenshot()} disabled={busy} title="截图并识别文字">
              截图
            </button>
          </div>
          <div style={{ display: "flex", gap: 6, alignItems: "center", marginTop: 8 }}>
            <span className="software-kv" style={{ whiteSpace: "nowrap" }}>
              按文字点击
            </span>
            <input
              className="software-search-input"
              style={{ flex: 1 }}
              placeholder="控件可见文字 / 描述，如：登录、搜索、下一步"
              value={target}
              onChange={(e) => setTarget(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void clickText();
              }}
            />
            <button className="software-primary" onClick={() => void clickText()} disabled={busy}>
              点击控件
            </button>
            <button
              className="software-action"
              onClick={() => void highlight()}
              disabled={busy}
              title="只把目标元素用一条线框在网页里，不点击"
            >
              仅定位
            </button>
            <button
              className="software-action"
              onClick={() => void scan()}
              disabled={busy}
              title="把页面（或输入框里那个模块）的可交互元素全部编号框出，点编号即点击"
            >
              扫描元素
            </button>
            <span style={{ fontSize: 10, color: "var(--text-faint)", whiteSpace: "nowrap" }}>
              看 → 点闭环 · 定位在网页内画线
            </span>
          </div>

          {/* 扫描结果：页面上已标出编号线框，点这里任一编号即点击对应元素 */}
          {marks.length > 0 && (
            <div style={{ marginTop: 8 }}>
              <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 4 }}>
                <span className="software-kv" style={{ whiteSpace: "nowrap" }}>
                  可交互元素 {marks.length} 个
                </span>
                <span style={{ fontSize: 10, color: "var(--text-faint)" }}>{markScope}</span>
                {offView && (offView.above > 0 || offView.below > 0) && (
                  <span style={{ fontSize: 10, color: "var(--text-faint)" }}>
                    视口外还有 ↑{offView.above} ↓{offView.below}
                  </span>
                )}
                <span style={{ flex: 1 }} />
                <button
                  className="software-action"
                  style={{ minWidth: 0, padding: "1px 6px" }}
                  onClick={() => {
                    setMarks([]);
                    setOffView(null);
                  }}
                  title="只清掉这里的清单；页面上的线框会自己消失"
                >
                  收起
                </button>
              </div>
              <div
                style={{
                  display: "flex",
                  flexWrap: "wrap",
                  gap: 4,
                  maxHeight: 96,
                  overflowY: "auto",
                }}
              >
                {marks.map((m) => (
                  <button
                    key={m.n}
                    className="software-action"
                    style={{ minWidth: 0, padding: "2px 6px", fontSize: 11 }}
                    disabled={busy}
                    onClick={() => void clickMark(m.n)}
                    title={`${m.tag}${m.text ? `：${m.text}` : ""}（点击即按编号点击该元素）`}
                  >
                    <span style={{ color: "var(--cyan)", fontFamily: "monospace" }}>{m.n}</span>
                    {m.text ? ` ${m.text.slice(0, 12)}` : ` ${m.tag}`}
                  </button>
                ))}
              </div>
            </div>
          )}
        </div>

        {/* 主体 */}
        <div style={{ flex: 1, display: "flex", minHeight: 0 }}>
          {/* 左侧：标签页列表 */}
          <div
            style={{
              width: 230,
              flexShrink: 0,
              borderRight: "1px solid var(--border-soft)",
              display: "flex",
              flexDirection: "column",
              overflow: "hidden",
            }}
          >
            <div
              style={{
                display: "flex",
                alignItems: "center",
                gap: 6,
                padding: "8px 10px",
                borderBottom: "1px solid var(--border-soft)",
              }}
            >
              <span className="software-sec-title" style={{ margin: 0, flex: 1 }}>
                标签页列表
              </span>
              <button
                className="software-action"
                onClick={() => void tidyTabs()}
                disabled={busy || !tabSummary || tabSummary.cleanable === 0}
                title="一次关掉白泽自己开的空白标签页（机主自己开的页面不会被关）"
              >
                清理空白
              </button>
              <button className="software-action" onClick={() => void loadTabs()} disabled={busy} title="刷新列表">
                刷新
              </button>
            </div>
            {tabSummary && (
              <div
                style={{
                  padding: "5px 10px",
                  fontSize: 10,
                  color: "var(--text-faint)",
                  borderBottom: "1px solid var(--border-soft)",
                }}
              >
                共 {tabSummary.total} 个 · 白泽开的 {tabSummary.ours} · 机主的 {tabSummary.user}
                {tabSummary.cleanable > 0 && (
                  <span style={{ color: "var(--cyan)" }}> · 可清理 {tabSummary.cleanable}</span>
                )}
              </div>
            )}
            <div style={{ flex: 1, overflow: "auto", padding: 6 }}>
              {tabs.length === 0 ? (
                <div style={{ color: "var(--text-faint)", fontSize: 12, padding: 10 }}>
                  暂无标签页。点击「＋ 标签」或「打开」网址。
                </div>
              ) : (
                tabs.map((t) => (
                  <div
                    key={t.id}
                    style={{
                      display: "flex",
                      alignItems: "center",
                      gap: 6,
                      padding: "7px 8px",
                      borderRadius: 8,
                      cursor: "pointer",
                      marginBottom: 4,
                      background: t.id === activeId ? "rgba(34,211,238,0.12)" : "transparent",
                      border:
                        t.id === activeId ? "1px solid rgba(34,211,238,0.4)" : "1px solid transparent",
                    }}
                    onClick={() => void switchTab(t.id)}
                  >
                    <span style={{ fontSize: 13 }}>{t.id === activeId ? "●" : "○"}</span>
                    <span
                      style={{
                        flex: 1,
                        fontSize: 12,
                        color: "var(--text)",
                        overflow: "hidden",
                        textOverflow: "ellipsis",
                        whiteSpace: "nowrap",
                      }}
                      title={t.url}
                    >
                      {t.title || t.url || "(空)"}
                    </span>
                    {t.dialog && (
                      <span
                        title="这个标签页上有一个未处理的对话框（alert/confirm），会挡住后续操作"
                        style={{
                          fontSize: 9,
                          padding: "0 4px",
                          borderRadius: 3,
                          background: "rgba(248,113,113,0.18)",
                          color: "var(--danger)",
                          flexShrink: 0,
                        }}
                      >
                        对话框
                      </span>
                    )}
                    {t.blank && (
                      <span
                        title="空白页：白泽开的可以直接清理"
                        style={{
                          fontSize: 9,
                          padding: "0 4px",
                          borderRadius: 3,
                          background: "rgba(148,163,184,0.18)",
                          color: "var(--text-faint)",
                          flexShrink: 0,
                        }}
                      >
                        空
                      </span>
                    )}
                    {t.ours === false && (
                      <span
                        title="机主自己开的页面：白泽不会操作也不会关闭它"
                        style={{
                          fontSize: 9,
                          padding: "0 4px",
                          borderRadius: 3,
                          background: "rgba(250,204,21,0.16)",
                          color: "#eab308",
                          flexShrink: 0,
                        }}
                      >
                        机主
                      </span>
                    )}
                    <button
                      className="browser-tab-close"
                      style={{ fontSize: 14 }}
                      onClick={(e) => {
                        e.stopPropagation();
                        void closeTab(t.id);
                      }}
                      title="关闭标签页"
                    >
                      ×
                    </button>
                  </div>
                ))
              )}
            </div>
          </div>

          {/* 右侧：截图 + OCR 文本 */}
          <div style={{ flex: 1, display: "flex", flexDirection: "column", minWidth: 0 }}>
            <div style={{ flex: 1, overflow: "auto", background: "#0d1117", position: "relative" }}>
              {shot ? (
                <img
                  src={shot.src}
                  alt="Chrome 页面截图"
                  style={{ width: "100%", display: "block" }}
                />
              ) : (
                <div
                  style={{
                    height: "100%",
                    display: "flex",
                    alignItems: "center",
                    justifyContent: "center",
                    color: "var(--text-faint)",
                    fontSize: 12,
                  }}
                >
                  点击「截图」或「实时预览」查看页面
                </div>
              )}
            </div>
            {shot && (
              <div
                style={{
                  borderTop: "1px solid var(--border-soft)",
                  maxHeight: 120,
                  overflow: "auto",
                  padding: "8px 12px",
                  fontSize: 11,
                  color: "var(--text-dim)",
                  fontFamily: "monospace",
                  whiteSpace: "pre-wrap",
                  wordBreak: "break-all",
                }}
              >
                <div style={{ color: "var(--text-faint)", marginBottom: 4 }}>
                  标题：{shot.title || "—"} · 网址：{shot.url || "—"}
                </div>
                {shot.text ? shot.text.slice(0, 600) : "（OCR 文字识别未就绪或无文本）"}
              </div>
            )}
          </div>
        </div>

        {/* 底部：操作日志 */}
        <div
          style={{
            borderTop: "1px solid var(--border-soft)",
            display: "flex",
            flexDirection: "column",
            maxHeight: 120,
          }}
        >
          <div
            style={{
              display: "flex",
              alignItems: "center",
              padding: "6px 12px",
              gap: 8,
            }}
          >
            <span className="software-sec-title" style={{ margin: 0 }}>
              操作日志
            </span>
            {error && <span style={{ flex: 1, fontSize: 11, color: "var(--danger)" }}>{error}</span>}
            {!error && <span style={{ flex: 1 }} />}
            <button
              className="software-action"
              onClick={() => setLog([])}
              title="清空日志"
            >
              清空
            </button>
          </div>
          <div ref={logBoxRef} style={{ overflow: "auto", padding: "0 12px 8px", fontSize: 11 }}>
            {log.length === 0 ? (
              <div style={{ color: "var(--text-faint)" }}>暂无操作记录。</div>
            ) : (
              log.map((l, i) => (
                <div key={i} style={{ fontFamily: "monospace", lineHeight: 1.6 }}>
                  <span style={{ color: "var(--text-faint)" }}>{fmtTime(l.ts)}</span>{" "}
                  <span style={{ color: l.ok ? "var(--text-dim)" : "var(--danger)" }}>{l.text}</span>
                </div>
              ))
            )}
          </div>
        </div>
      </div>
    </div>
  );
}