/**
 * 白泽·棱镜舱（Nexus Prism）· 入口
 * ----------------------------------------------------------
 * 全新启动顺序：
 *   1. applyPrism("aurora")  —— 注入第一套折射色 CSS 变量
 *   2. mountBridge()         —— Organelle 细胞器：Tauri 桥（命令上行 + 事件下行）
 *   3. React 渲染 <NexusRoot />
 * ----------------------------------------------------------
 * 本前端运行于 1423 端口，与 baize/frontend（1422 / 旧 aurora/水晶工坊）、
 * 桌面版 baize/src（1421 / 旧内核）完全隔离，无任何互相引用。
 */
import React from "react";
import ReactDOM from "react-dom/client";
import { NexusRoot } from "./index/scaffold/NexusScaffold";
import { applyPrism } from "./prism/prism.engine";
import { mountBridge } from "./organelles/bridge/tauri.bridge";
import "./prism/prism.layout.css";

applyPrism("aurora");
mountBridge();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <NexusRoot />
  </React.StrictMode>
);