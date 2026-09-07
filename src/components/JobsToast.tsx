import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";

/** 后台任务进度行（与后端 jobs::JobInfo 对应） */
export interface JobInfo {
  id: string;
  kind: string;
  title: string;
  status: "running" | "done" | "failed";
  progress: number;
  total: number;
  detail: string;
  started_at: number;
  finished_at?: number | null;
}

/**
 * 后台任务浮层：右下角小卡片，实时显示运行中的后台任务（如 RAG 索引），
 * 完成后停留 5s 自动消失。数据来源：后端 jobs.rs 的 "job-update" 事件。
 */
export function JobsToast() {
  const [jobs, setJobs] = useState<Record<string, JobInfo>>({});

  useEffect(() => {
    const un = listen<JobInfo>("job-update", (e) => {
      const j = e.payload;
      setJobs((m) => {
        const next = { ...m, [j.id]: j };
        // 完成/失败任务 5 秒后从浮层移除（后端保留状态 60s 供查询）
        if (j.status !== "running") {
          setTimeout(() => {
            setJobs((m2) => {
              const c = { ...m2 };
              if (c[j.id]?.status !== "running") delete c[j.id];
              return c;
            });
          }, 5000);
        }
        return next;
      });
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  const visible = Object.values(jobs);
  if (visible.length === 0) return null;

  return (
    <div
      style={{
        position: "fixed",
        right: 16,
        bottom: 16,
        zIndex: 3000,
        display: "flex",
        flexDirection: "column",
        gap: 8,
        width: 300,
      }}
    >
      {visible.map((j) => {
        const pct = j.total > 0 ? Math.min(100, Math.round((j.progress / j.total) * 100)) : null;
        const color = j.status === "failed" ? "#e5484d" : j.status === "done" ? "#30a46c" : "var(--accent, #6c7bff)";
        return (
          <div
            key={j.id}
            style={{
              background: "var(--bg-elevated, #1c1c24)",
              border: "1px solid var(--border-soft, #2e2e3a)",
              borderRadius: 10,
              padding: "10px 12px",
              boxShadow: "0 8px 24px rgba(0,0,0,0.35)",
              fontSize: 12,
            }}
          >
            <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 6 }}>
              <span
                style={{
                  width: 8,
                  height: 8,
                  borderRadius: "50%",
                  background: color,
                  flexShrink: 0,
                  animation: j.status === "running" ? "pulse 1.2s ease-in-out infinite" : undefined,
                }}
              />
              <span style={{ color: "var(--text, #eee)", fontWeight: 600, flex: 1, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                {j.title}
              </span>
              {pct !== null && <span style={{ color: "var(--text-dim, #999)" }}>{pct}%</span>}
            </div>
            {j.status === "running" && j.total > 0 && (
              <div style={{ height: 4, borderRadius: 2, background: "var(--border-soft, #2e2e3a)", overflow: "hidden" }}>
                <div style={{ width: `${pct}%`, height: "100%", background: color, transition: "width 0.3s" }} />
              </div>
            )}
            {j.detail && (
              <div style={{ color: "var(--text-faint, #777)", marginTop: 4, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                {j.status === "failed" ? "✗ " : j.status === "done" ? "✓ " : ""}
                {j.detail}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}
