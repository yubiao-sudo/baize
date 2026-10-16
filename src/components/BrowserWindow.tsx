import { useEffect, useRef, useState } from "react";
import { marked } from "marked";
import DOMPurify from "dompurify";
import { convertFileSrc } from "@tauri-apps/api/core";
import {
  browserAct,
  browserCheckEmbed,
  closeBrowserTab,
  getBrowserState,
  onBrowserUpdate,
  openUrlTab,
  switchBrowserTab,
} from "../api";
import type { Tab } from "../types";

/** 已自动转交受控浏览器的标签页（键：tabId|url）。
    模块级而非组件级：切换标签页会卸载重挂 UrlTab，组件状态会丢，否则每次切回都重开一个 Chrome 标签页 */
const handedOff = new Set<string>();

/**
 * 内置浏览器：多标签页。每个内容一个标签页（HTML / 网页 / Markdown / 文本 / 视频）。
 * 白泽往不同标签页放内容，用户可切换、关闭；
 * 网页类标签页在打开时由后端探测内嵌可用性（X-Frame-Options / CSP frame-ancestors /
 * 非 HTML 内容），确定打不开时不再显示无意义的白屏，而是给出原因并**自动改用受控
 * 桌面浏览器打开**（登录态独立、白泽可用 browser_act 继续精细操控）。
 */
export default function BrowserWindow() {
  const [tabs, setTabs] = useState<Tab[]>([]);
  const [activeId, setActiveId] = useState("");
  const [handoffStatus, setHandoffStatus] = useState("");

  useEffect(() => {
    getBrowserState().then((s) => applyTabs(s.tabs ?? []));
    const un = onBrowserUpdate((s) => applyTabs(s.tabs ?? []));
    return () => {
      un.then((f) => f());
    };
  }, []);

  // 搜索结果页（html 标签 iframe）点击卡片 → 在内置浏览器中打开新标签页；
  // 结果页自身保留为独立标签，切回即返回搜索结果
  useEffect(() => {
    const onMsg = (e: MessageEvent) => {
      const d = e.data as { type?: string; url?: string } | null;
      if (!d || d.type !== "baize-open-url" || typeof d.url !== "string") return;
      if (!/^https?:\/\//i.test(d.url)) return;
      void openUrlTab(d.url).catch(() => {});
    };
    window.addEventListener("message", onMsg);
    return () => window.removeEventListener("message", onMsg);
  }, []);

  const applyTabs = (ts: Tab[]) => {
    setTabs(ts);
    const active = ts.find((t) => t.active);
    setActiveId(active ? active.id : ts.length > 0 ? ts[ts.length - 1].id : "");
  };

  const onSwitch = (id: string) => {
    setActiveId(id);
    void switchBrowserTab(id);
  };

  const onClose = (id: string) => {
    setTabs((ts) => {
      const next = ts.filter((t) => t.id !== id);
      if (activeId === id && next.length > 0) {
        setActiveId(next[next.length - 1].id);
      }
      return next;
    });
    void closeBrowserTab(id);
  };

  const activeTab = tabs.find((t) => t.id === activeId) ?? null;

  /** 转交桌面浏览器：把当前网页标签页的 URL 交给 CDP 受控的桌面 Chrome/Edge，
      白泽可随即用 browser_act 的 19 种动作继续精细操控（登录态独立于系统默认浏览器） */
  const handoffToDesktop = async () => {
    if (!activeTab || activeTab.kind !== "url" || !activeTab.content.trim()) return;
    setHandoffStatus("正在转交桌面浏览器…");
    try {
      await browserAct({ action: "new_tab", url: activeTab.content.trim() });
      setHandoffStatus("已在桌面浏览器打开，白泽可继续操控该页面");
    } catch (e) {
      setHandoffStatus("转交失败: " + e + "（需已安装 Chrome/Edge）");
    }
    window.setTimeout(() => setHandoffStatus(""), 5000);
  };

  return (
    <div className="browser">
      <div className="browser-tabbar">
        {tabs.map((t) => (
          <div
            key={t.id}
            className={`browser-tab ${t.id === activeId ? "active" : ""}`}
            onClick={() => onSwitch(t.id)}
          >
            <span className="browser-tab-title">{t.title || t.kind}</span>
            {t.embed?.status === "blocked" && (
              <span className="browser-tab-warn" title={t.embed.reason ?? "该网站拒绝被内嵌"}>
                ⚠
              </span>
            )}
            <button
              className="browser-tab-close"
              title="关闭标签页"
              onClick={(e) => {
                e.stopPropagation();
                onClose(t.id);
              }}
            >
              ×
            </button>
          </div>
        ))}
        {activeTab?.kind === "url" &&
          activeTab.content.trim() &&
          activeTab.embed?.status !== "blocked" && (
            <button
              className="browser-handoff"
              title="在受控桌面 Chrome/Edge 中打开，白泽可继续操控该页面"
              onClick={() => void handoffToDesktop()}
            >
              桌面浏览器打开
            </button>
          )}
      </div>
      {handoffStatus && <div className="browser-handoff-status">{handoffStatus}</div>}

      <div className="browser-content">
        {activeTab ? (
          <TabView tab={activeTab} onHandoff={() => void handoffToDesktop()} />
        ) : (
          <div className="browser-empty">
            让白泽在这里演示内容吧
            <br />
            例如：「搜索今天的新闻」·「做个贪吃蛇游戏」·「播放一个视频」
          </div>
        )}
      </div>
    </div>
  );
}

function TabView({ tab, onHandoff }: { tab: Tab; onHandoff: () => void }) {
  if (tab.kind === "html") {
    return (
      <iframe
        className="browser-frame"
        sandbox="allow-scripts allow-same-origin allow-popups allow-popups-to-escape-sandbox"
        srcDoc={tab.content}
        title={tab.title}
      />
    );
  }
  if (tab.kind === "url") {
    return <UrlTab tab={tab} onHandoff={onHandoff} />;
  }
  if (tab.kind === "video") {
    const src = /^https?:\/\//i.test(tab.content)
      ? tab.content
      : convertFileSrc(tab.content);
    return (
      <div className="browser-video">
        <video src={src} controls autoPlay style={{ width: "100%", height: "100%" }} />
      </div>
    );
  }
  if (tab.kind === "markdown") {
    return (
      <div
        className="browser-markdown"
        dangerouslySetInnerHTML={{
          __html: DOMPurify.sanitize(marked.parse(tab.content) as string),
        }}
      />
    );
  }
  return <div className="browser-text">{tab.content}</div>;
}

/**
 * 网页标签页。分三种情况：
 *  - embed.status === "blocked"：后端探测已确认该站点拒绝被内嵌（X-Frame-Options /
 *    CSP frame-ancestors / 非 HTML 内容）。此时不再渲染 iframe（只会白屏），直接展示
 *    原因 + 信号原文，并**自动**在受控桌面浏览器打开，让白泽能继续操控该页面。
 *  - embed 缺省或 unknown：无法确定，仍渲染 iframe 并保留「加载慢」提示兜底
 *    （探测失败可能只是网络问题，不能据此误判站点拒绝内嵌）。
 */
function UrlTab({ tab, onHandoff }: { tab: Tab; onHandoff: () => void }) {
  const blocked = tab.embed?.status === "blocked";
  const [loaded, setLoaded] = useState(false);
  const [slow, setSlow] = useState(false);
  const [reloadKey, setReloadKey] = useState(0);
  const [urlCopied, setUrlCopied] = useState(false);
  const [autoStatus, setAutoStatus] = useState("");
  const autoDone = useRef(false);

  useEffect(() => {
    setLoaded(false);
    setSlow(false);
    const t = window.setTimeout(() => setSlow(true), 3500);
    return () => window.clearTimeout(t);
  }, [tab.id, tab.content, reloadKey]);

  // 确定被拒绝内嵌 → 自动改用受控桌面浏览器打开（每个 tab+url 只做一次）
  useEffect(() => {
    if (!blocked) return;
    const url = tab.content.trim();
    const key = `${tab.id}|${url}`;
    if (handedOff.has(key)) {
      setAutoStatus("已在受控桌面浏览器中打开，可继续让白泽操控该页面");
      return;
    }
    if (autoDone.current) return;
    autoDone.current = true;
    handedOff.add(key);
    setAutoStatus("正在自动改用受控桌面浏览器打开…");
    (async () => {
      // 受控浏览器冷启动/全局操控锁可能瞬时失败，自动重试一次再报错
      for (let attempt = 0; attempt < 2; attempt++) {
        try {
          await browserAct({ action: "new_tab", url });
          setAutoStatus("已在受控桌面浏览器中打开，可继续让白泽操控该页面");
          return;
        } catch (e) {
          if (attempt === 0) {
            await new Promise((r) => setTimeout(r, 2000));
            continue;
          }
          handedOff.delete(key); // 允许用户点「在受控浏览器打开」重试
          setAutoStatus(`自动改用受控浏览器失败：${e}（需本机已安装 Chrome/Edge）`);
        }
      }
    })();
  }, [blocked, tab.id, tab.content]);

  const copyUrl = async () => {
    try {
      await navigator.clipboard.writeText(tab.content);
      setUrlCopied(true);
      window.setTimeout(() => setUrlCopied(false), 2000);
    } catch {
      /* 忽略 */
    }
  };

  /** 复核内嵌判定：结果写回该标签页（后端广播 → 本组件随 state 更新） */
  const recheck = async () => {
    setAutoStatus("正在重新检测…");
    try {
      const r = await browserCheckEmbed(tab.content, tab.id);
      setAutoStatus(
        r.embed?.status === "blocked"
          ? "重新检测：站点仍拒绝内嵌"
          : "重新检测：当前未发现拒绝内嵌的响应头，已按可内嵌处理"
      );
    } catch (e) {
      setAutoStatus(`重新检测失败：${e}`);
    }
    window.setTimeout(() => setAutoStatus(""), 6000);
  };

  const urlbar = (
    <div className="browser-urlbar">
      <span className="browser-url-text" title={tab.content}>
        {tab.content}
      </span>
      <button className="browser-urlbtn" onClick={() => void copyUrl()} title="复制网址">
        {urlCopied ? "✓" : "📋"}
      </button>
      <button
        className="browser-urlbtn"
        onClick={() => setReloadKey((k) => k + 1)}
        title="重新加载"
      >
        ↻
      </button>
    </div>
  );

  if (blocked) {
    return (
      <div className="browser-urlwrap">
        {urlbar}
        <div className="browser-embed-blocked">
          <div className="embed-blocked-icon">⛔</div>
          <div className="embed-blocked-title">该网站拒绝被内置浏览器内嵌</div>
          <div className="embed-blocked-reason">
            {tab.embed?.reason ?? "站点安全策略不允许被 iframe 嵌入"}
          </div>
          {tab.embed?.signal && (
            <code className="embed-blocked-signal">{tab.embed.signal}</code>
          )}
          <div className="embed-blocked-hint">
            {autoStatus || "已自动改用受控桌面浏览器打开，白泽可继续操控该页面"}
          </div>
          <div className="embed-blocked-actions">
            <button className="embed-blocked-btn" onClick={onHandoff}>
              在受控浏览器打开
            </button>
            <button className="embed-blocked-btn ghost" onClick={() => void recheck()}>
              重新检测
            </button>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="browser-urlwrap">
      {urlbar}
      <div className="browser-urlframe">
        <iframe
          key={`${tab.id}-${tab.content}-${reloadKey}`}
          className="browser-frame"
          src={tab.content}
          title={tab.title}
          onLoad={() => setLoaded(true)}
        />
        {!loaded && (
          <div className="iframe-loading">
            <div className="iframe-spinner" />
            <div className="iframe-loading-text">
              {slow ? (
                <>
                  该网站加载较慢，或可能拒绝被内嵌（显示白屏属正常现象）
                  <br />
                  <button className="iframe-handoff-btn" onClick={onHandoff}>
                    在桌面浏览器打开（白泽可继续操控）
                  </button>
                </>
              ) : (
                "正在加载页面…"
              )}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
