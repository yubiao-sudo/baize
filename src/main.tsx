import React, { lazy, Suspense, useEffect } from "react";
import ReactDOM from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import App from "./App";
import "./index.css";

// 窗口组件按路由按需加载，避免主界面启动时解析 xterm / three / d3 等重型依赖
const BrowserWindow = lazy(() => import("./components/BrowserWindow"));
const MarkdownWindow = lazy(() => import("./components/MarkdownWindow"));
const StepBarrage = lazy(() => import("./components/StepBarrage"));
const HaloOverlay = lazy(() => import("./components/HaloOverlay"));
const TerminalWindow = lazy(() => import("./components/TerminalWindow"));
const OrbFloat = lazy(() => import("./components/OrbFloat"));

function Root() {
  // 启动显示时序：窗口以 visible=false 创建，等首帧真正上屏（双 rAF）后显示，
  // 消除「窗口创建 → WebView 首帧渲染」之间的黑屏；启动动画在窗口可见期间淡出
  useEffect(() => {
    let raf2 = 0;
    const raf1 = requestAnimationFrame(() => {
      raf2 = requestAnimationFrame(() => {
        const w = getCurrentWindow();
        void w.show();
        void w.setFocus();
      });
    });
    return () => {
      cancelAnimationFrame(raf1);
      cancelAnimationFrame(raf2);
    };
  }, []);

  // React 挂载完成：通知 index.html 启动脚本执行「水球飞入右侧面板」交接动画；
  // 15s 兜底强制移除，防止极端情况下启动遮罩残留
  useEffect(() => {
    (window as unknown as Record<string, unknown>).__BAIZE_READY = true;
    window.dispatchEvent(new Event("baize:app-ready"));
    const t = window.setTimeout(() => document.getElementById("splash")?.remove(), 15000);
    return () => window.clearTimeout(t);
  }, []);

  const hash = window.location.hash;
  let page: React.ReactNode;
  if (hash.startsWith("#/browser")) page = <BrowserWindow />;
  else if (hash.startsWith("#/markdown")) page = <MarkdownWindow />;
  else if (hash.startsWith("#/terminal")) page = <TerminalWindow />;
  else if (hash.startsWith("#/step")) page = <StepBarrage />;
  else if (hash.startsWith("#/halo")) page = <HaloOverlay />;
  else if (hash.startsWith("#/orb")) page = <OrbFloat />;
  else page = <App />;

  return (
    <Suspense
      fallback={
        <div style={{ padding: 24, color: "var(--text-faint)", fontSize: 13 }}>加载中…</div>
      }
    >
      {page}
    </Suspense>
  );
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <Root />
  </React.StrictMode>
);
