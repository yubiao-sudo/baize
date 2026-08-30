import { useCallback, useEffect, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { browserAct } from "../api";
import type { ChromeTabInfo } from "../types";

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
            <span style={{ fontSize: 10, color: "var(--text-faint)", whiteSpace: "nowrap" }}>
              看 → 点闭环
            </span>
          </div>
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
              <button className="software-action" onClick={() => void loadTabs()} disabled={busy} title="刷新列表">
                刷新
              </button>
            </div>
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