import { useCallback, useEffect, useRef, useState } from "react";
import { getImLog } from "../api";
import type { ImLogEntry } from "../types";

/** 通道 id → 标识色 */
const CHANNEL_COLORS: Record<string, string> = { wechat: "#22c55e", feishu: "#38bdf8" };

/** 毫秒时间戳 → 时分秒 */
function fmtTime(ms: number): string {
  const d = new Date(ms);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

/**
 * IM 消息收发日志面板：回看「手机发来的指令」与「白泽回传的审批/结果」。
 * 后端 im_log 返回内存环形缓冲日志（最多 500 条，时间正序）；此面板倒序显示最新在前。
 */
export default function ImLogPanel({ onClose }: { onClose: () => void }) {
  const [logs, setLogs] = useState<ImLogEntry[]>([]);
  const [busy, setBusy] = useState(true);
  const [error, setError] = useState("");
  const [auto, setAuto] = useState(true);
  const mounted = useRef(true);

  const load = useCallback(async () => {
    try {
      const list = await getImLog();
      if (mounted.current) {
        setLogs(list);
        setError("");
      }
    } catch (e) {
      if (mounted.current) setError(String(e));
    } finally {
      if (mounted.current) setBusy(false);
    }
  }, []);

  useEffect(() => {
    mounted.current = true;
    void load();
    return () => {
      mounted.current = false;
    };
  }, [load]);

  // 自动刷新（可暂停）
  useEffect(() => {
    if (!auto) return;
    const t = setInterval(() => void load(), 3000);
    return () => clearInterval(t);
  }, [auto, load]);

  const inCount = logs.filter((l) => l.direction === "in").length;
  const outCount = logs.filter((l) => l.direction === "out").length;
  const newestFirst = [...logs].reverse();

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
          <h3 style={{ margin: 0, fontSize: 15, letterSpacing: 1 }}>IM 消息总线</h3>
          <span style={{ fontSize: 11, color: "var(--text-faint)" }}>
            收 {inCount} · 发 {outCount}
          </span>
          <span style={{ flex: 1 }} />
          <button
            className="software-action"
            style={{ fontSize: 11 }}
            onClick={() => setAuto((v) => !v)}
            title={auto ? "暂停自动刷新" : "恢复自动刷新"}
          >
            {auto ? "暂停刷新" : "自动刷新"}
          </button>
          <button className="software-action" style={{ fontSize: 11 }} onClick={() => void load()}>
            刷新
          </button>
          <button className="software-close" onClick={onClose} title="关闭">
            ×
          </button>
        </div>

        {/* 主体 */}
        <div className="software-body" style={{ overflowY: "auto" }}>
          {error && <div className="software-error">{error}</div>}

          {busy && logs.length === 0 ? (
            <div style={{ color: "var(--text-dim)" }}>加载中…</div>
          ) : newestFirst.length === 0 ? (
            <div style={{ color: "var(--text-faint)" }}>
              暂无消息记录。手机通过微信 / 飞书发来指令，或白泽回传审批 / 结果后会在此显示。
            </div>
          ) : (
            <div className="software-pkg-list">
              {newestFirst.map((l, i) => {
                const isIn = l.direction === "in";
                const chColor = CHANNEL_COLORS[l.channel] ?? "#64748b";
                return (
                  <div
                    key={`${l.ts}-${i}`}
                    className="software-pkg"
                    style={{ flexDirection: "column", alignItems: "stretch" }}
                  >
                    <div style={{ display: "flex", alignItems: "center", gap: 8, width: "100%" }}>
                      <span
                        className="software-badge"
                        style={{
                          fontSize: 10,
                          background: isIn ? "rgba(52,211,153,0.15)" : "rgba(245,158,11,0.15)",
                          color: isIn ? "#34d399" : "#f59e0b",
                        }}
                      >
                        {isIn ? "收到" : "发出"}
                      </span>
                      <span
                        className="software-badge"
                        style={{ fontSize: 10, borderColor: chColor, color: chColor }}
                      >
                        {l.channel_label}
                      </span>
                      <span className="software-pkg-name" style={{ flex: 1, minWidth: 0 }}>
                        {l.text}
                      </span>
                      <span className="software-pkg-ver">{fmtTime(l.ts)}</span>
                    </div>
                    {l.peer && (
                      <div
                        style={{
                          fontSize: 10,
                          color: "var(--text-faint)",
                          marginTop: 4,
                          overflow: "hidden",
                          textOverflow: "ellipsis",
                          whiteSpace: "nowrap",
                        }}
                        title={l.peer}
                      >
                        对端：{l.peer}
                      </div>
                    )}
                  </div>
                );
              })}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}