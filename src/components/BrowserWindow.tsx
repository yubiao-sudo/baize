import { useEffect, useState } from "react";
import { marked } from "marked";
import DOMPurify from "dompurify";
import { convertFileSrc } from "@tauri-apps/api/core";
import {
  browserAct,
  closeBrowserTab,
  getBrowserState,
  onBrowserUpdate,
  switchBrowserTab,
} from "../api";
import type { Tab } from "../types";

/**
 * 内置浏览器：多标签页。每个内容一个标签页（HTML / 网页 / Markdown / 文本 / 视频）。
 * 白泽往不同标签页放内容，用户可切换、关闭；
 * 网页类标签页带加载态与内嵌失败引导（X-Frame-Options 拒绝内嵌时引导转交桌面浏览器），
 * 由白泽的 CDP 通道继续精细操控。
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
        {activeTab?.kind === "url" && activeTab.content.trim() && (
          <button
            className="browser-handoff"
            title="在桌面 Chrome/Edge 中打开，白泽可继续操控该页面"
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
 * 网页标签：iframe 内嵌 + 加载态 + 内嵌失败引导。
 * 大多数网站带 X-Frame-Options/CSP 会拒绝被 iframe 内嵌（表现为白屏），
 * 跨源下无法直接探测，因此 3.5s 未完成 load 即显示引导条，一键转交桌面浏览器。
 */
function UrlTab({ tab, onHandoff }: { tab: Tab; onHandoff: () => void }) {
  const [loaded, setLoaded] = useState(false);
  const [slow, setSlow] = useState(false);
  const [reloadKey, setReloadKey] = useState(0);
  const [urlCopied, setUrlCopied] = useState(false);

  useEffect(() => {
    setLoaded(false);
    setSlow(false);
    const t = window.setTimeout(() => setSlow(true), 3500);
    return () => window.clearTimeout(t);
  }, [tab.id, tab.content, reloadKey]);

  const copyUrl = async () => {
    try {
      await navigator.clipboard.writeText(tab.content);
      setUrlCopied(true);
      window.setTimeout(() => setUrlCopied(false), 2000);
    } catch {
      /* 忽略 */
    }
  };

  return (
    <div className="browser-urlwrap">
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
                  该网站可能拒绝被内嵌（显示白屏属正常现象）
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
