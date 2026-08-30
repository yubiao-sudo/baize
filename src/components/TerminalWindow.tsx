import { useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { onTermData, termClose, termResize, termSpawn, termWrite } from "../api";

// Windows Terminal「Campbell」配色（贴合系统终端风格）
const CAMPBELL = {
  background: "#0c0c0c",
  foreground: "#cccccc",
  cursor: "#ffffff",
  cursorAccent: "#0c0c0c",
  selectionBackground: "rgba(255,255,255,0.28)",
  black: "#0c0c0c",
  red: "#e74856",
  green: "#16c60c",
  yellow: "#f9f1a5",
  blue: "#3b78ff",
  magenta: "#b4009e",
  cyan: "#61d6d6",
  white: "#cccccc",
  brightBlack: "#767676",
  brightRed: "#f78383",
  brightGreen: "#8dff8d",
  brightYellow: "#ffffb5",
  brightBlue: "#9fb4ff",
  brightMagenta: "#e89fff",
  brightCyan: "#7fffff",
  brightWhite: "#f2f2f2",
};

export default function TerminalWindow() {
  const containerRef = useRef<HTMLDivElement>(null);
  const [status, setStatus] = useState<"loading" | "ready" | "error">("loading");
  const [errorMsg, setErrorMsg] = useState<string>("");

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    let term: Terminal | null = null;
    try {
      term = new Terminal({
        convertEol: true,
        cursorBlink: true,
        cursorStyle: "block",
        fontSize: 14,
        fontFamily: "Cascadia Mono, Consolas, 'Courier New', monospace",
        scrollback: 5000,
        theme: CAMPBELL,
      });
      const fit = new FitAddon();
      term.loadAddon(fit);
      term.open(container);

      // 用户按键 → 后端 PTY
      term.onData((data) => void termWrite(data));

      let unlisten: (() => void) | null = null;
      let disposed = false;

      // 先订阅后端输出，再启动会话，避免漏掉初始提示符
      (async () => {
        try {
          unlisten = await onTermData((data) => term!.write(data));
          if (disposed) {
            unlisten();
            return;
          }
          fit.fit();
          void termResize(term!.rows, term!.cols);
          await termSpawn();
          if (!disposed) setStatus("ready");
        } catch (e) {
          if (!disposed) {
            setStatus("error");
            setErrorMsg(String(e));
          }
        }
      })();

      const onResize = () => {
        fit.fit();
        void termResize(term!.rows, term!.cols);
      };
      window.addEventListener("resize", onResize);

      return () => {
        disposed = true;
        window.removeEventListener("resize", onResize);
        if (unlisten) unlisten();
        void termClose();
        term!.dispose();
      };
    } catch (e) {
      setStatus("error");
      setErrorMsg(String(e));
      return;
    }
  }, []);

  return (
    <div className="terminal-window">
      {/* 诊断横幅：确认 React 是否渲染、路由 hash 是否正确（定位白屏用） */}
      <div
        style={{
          padding: "6px 12px",
          fontSize: "11px",
          fontFamily: "Consolas, monospace",
          background: "#1e1e1e",
          color: "#9cdcfe",
          borderBottom: "1px solid #333",
          flexShrink: 0,
        }}
      >
        hash={JSON.stringify(window.location.hash)} | href={window.location.href}
        {" | "}
        {status === "loading"
          ? "加载中…"
          : status === "ready"
            ? "会话已启动"
            : `错误: ${errorMsg}`}
      </div>
      <div className="terminal-titlebar">
        <span className="terminal-title">PowerShell</span>
      </div>
      <div className="terminal-container" ref={containerRef} />
    </div>
  );
}