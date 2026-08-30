import { useMemo, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { useChat } from "../stores/chat";
import { openTerminalWithCommand } from "../api";
import type { ThoughtEvent, Todo } from "../types";

// 合并同一安装产生的多条 tool_progress 为最新一条：后端把每条进度节流都写进了 trace，
// trace 回放（安装完成后的总结回复）时若不合并，会渲染出几十条进度条。
function compactProgress(thoughts: ThoughtEvent[]): ThoughtEvent[] {
  const out: ThoughtEvent[] = [];
  for (const t of thoughts) {
    if (t.kind === "tool_progress") {
      const last = out[out.length - 1];
      if (last && last.kind === "tool_progress" && last.label === t.label) {
        out[out.length - 1] = t;
        continue;
      }
    }
    out.push(t);
  }
  return out;
}

// 「执行」类事件（显示为执行）：工具调用/结果/权限审批；其余（阶段/记忆/模型/计划/思考等）显示为「思考」
const EXEC_KINDS = new Set(["tool_call", "tool_result", "permission"]);

/** 「执行」阶段行（每轮重复的「执行 · 调用工具执行任务」）与随后的工具行信息重复：
    该阶段之后若跟了工具调用，则整行丢弃（工具行自身已带「执行」标签与状态勾叉） */
function dropRedundantExecPhase<T extends ThoughtEvent>(thoughts: T[]): T[] {
  const out: T[] = [];
  for (let i = 0; i < thoughts.length; i++) {
    const t = thoughts[i];
    if (t.kind === "phase" && t.label === "执行") {
      let j = i + 1;
      let hasTool = false;
      while (j < thoughts.length && thoughts[j].kind !== "phase") {
        if (thoughts[j].kind === "tool_call") {
          hasTool = true;
          break;
        }
        j++;
      }
      if (hasTool) continue;
    }
    out.push(t);
  }
  return out;
}

// 合并「调用工具 · x」「工具完成 · x」为一条，状态用 √/✕ 展示；夹在中间的 phase/审批等事件保持原位
type MergedThought = ThoughtEvent & { status?: "success" | "failed"; callArgs?: string };

/** 命令执行类工具：执行流条目提供「终端查看」入口 */
const COMMAND_TOOLS = new Set(["ps_exec", "run_command"]);

/** 从工具调用参数 JSON 里提取命令文本（兼容 command/cmd/executable 字段与纯字符串） */
function extractCommand(callArgs?: string): string | null {
  if (!callArgs) return null;
  try {
    const o = JSON.parse(callArgs);
    if (typeof o === "string") return o.trim() || null;
    if (o && typeof o === "object") {
      const cmd = o.command ?? o.cmd ?? o.executable;
      if (typeof cmd === "string" && cmd.trim()) return cmd.trim();
    }
  } catch {
    /* 非 JSON 忽略 */
  }
  return null;
}

function toolName(label: string): string {
  return label
    .replace(/^调用工具\s*·\s*/, "")
    .replace(/^工具完成\s*·\s*/, "")
    .replace(/^任务广场\s*·\s*运行\s*/, "")
    .replace(/^任务广场\s*·\s*/, "")
    .trim();
}

/** 判断工具结果是否失败：结果 JSON 含 error 字段，或 exit_code 非 0 */
function toolFailed(detail: string): boolean {
  if (!detail) return false;
  try {
    const o = JSON.parse(detail);
    if (o && typeof o === "object" && !Array.isArray(o)) {
      if (o.error) return true;
      if (typeof o.exit_code === "number" && o.exit_code !== 0) return true;
      return false;
    }
  } catch {
    /* 非 JSON，忽略 */
  }
  return false;
}

/** 把连续的 tool_call + tool_result 合并为带 status 的单条（允许中间夹着 phase/permission 等事件） */
function mergeToolEvents(thoughts: ThoughtEvent[]): MergedThought[] {
  const out: MergedThought[] = [];
  let i = 0;
  while (i < thoughts.length) {
    const t = thoughts[i];
    if (t.kind === "tool_call") {
      let j = i + 1;
      const between: ThoughtEvent[] = [];
      while (
        j < thoughts.length &&
        thoughts[j].kind !== "tool_result" &&
        thoughts[j].kind !== "tool_call"
      ) {
        between.push(thoughts[j]);
        j++;
      }
      if (j < thoughts.length && thoughts[j].kind === "tool_result") {
        const result = thoughts[j];
        const merged: MergedThought = {
          ...result,
          kind: "tool_call",
          label: toolName(t.label),
          status: toolFailed(result.detail) ? "failed" : "success",
          detail: result.detail,
          callArgs: t.detail, // 调用参数（命令类工具的「终端查看」依据）
        };
        for (const b of between) out.push(b);
        out.push(merged);
        i = j + 1;
        continue;
      }
    }
    out.push(t);
    i++;
  }
  return out;
}

// 事件类型 → 状态节点颜色（科幻风格的轨迹点，替代传统 emoji）
const NODE_COLOR: Record<string, string> = {
  tool_call: "cyan",
  tool_result: "green",
  permission: "amber",
  memory: "violet",
  model: "cyan",
  focus: "amber",
  plan: "cyan",
  thinking: "violet",
  critic: "pink",
  rag: "violet",
  subagent: "violet",
  mode: "violet",
  author_tool: "cyan",
  test_pipeline: "cyan",
};

/** 常见图片扩展名（用于识别关键帧截图路径） */
const IMAGE_EXT = /\.(png|jpe?g|bmp|webp|gif)$/i;

/** 关键帧（与后端 replay::KeyframeEntry 对齐） */
interface Keyframe {
  seq: number;
  label: string;
  path: string;
  ts: number;
}

/** 解析 replay_keyframes 工具结果里的 frames 数组；非该工具或解析失败返回 null */
function parseKeyframes(detail: string): Keyframe[] | null {
  try {
    const o = JSON.parse(detail);
    if (o && Array.isArray(o.frames)) {
      return (o.frames as any[])
        .filter((f) => f && typeof f.path === "string" && IMAGE_EXT.test(f.path))
        .map((f) => ({
          seq: Number(f.seq ?? 0),
          label: typeof f.label === "string" ? f.label : "",
          path: f.path as string,
          ts: Number(f.ts ?? 0),
        }));
    }
  } catch {
    /* 非 JSON，忽略 */
  }
  return null;
}

/** 关键帧回看条：横向滚动展示每个写操作后的截图，标注序号与触发步骤 */
function KeyframeStrip({ frames }: { frames: Keyframe[] }) {
  return (
    <div
      className="flow-keyframes"
      style={{
        display: "flex",
        gap: 10,
        overflowX: "auto",
        padding: "10px 12px",
        marginTop: 6,
      }}
    >
      {frames.map((f) => (
        <figure
          key={f.seq}
          style={{
            flex: "0 0 auto",
            width: 240,
            margin: 0,
            display: "flex",
            flexDirection: "column",
            gap: 6,
          }}
        >
          <img
            src={convertFileSrc(f.path)}
            alt={`第 ${f.seq} 步 ${f.label}`}
            loading="lazy"
            style={{
              width: 240,
              height: 135,
              objectFit: "cover",
              borderRadius: 8,
              border: "1px solid rgba(255,255,255,0.12)",
              background: "var(--box-solid)",
              display: "block",
            }}
          />
          <figcaption
            style={{
              fontSize: 11,
              color: "#8b93a7",
              display: "flex",
              justifyContent: "space-between",
              gap: 6,
              lineHeight: 1.4,
            }}
          >
            <span style={{ color: "#22d3ee", flex: "0 0 auto" }}>#{f.seq}</span>
            <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
              {f.label}
            </span>
          </figcaption>
        </figure>
      ))}
    </div>
  );
}

/** 执行流中单条轨迹：一级只显示标签行，点击展开二级展示完整命令/分析/输出 */
function FlowLine({ t }: { t: MergedThought }) {
  const [open, setOpen] = useState(false);
  const isExec = EXEC_KINDS.has(t.kind);
  const hasDetail = !!t.detail && t.detail.trim().length > 0;
  const status = t.status;
  const node = status === "failed" ? "red" : status === "success" ? "green" : (NODE_COLOR[t.kind] ?? "default");
  // GUI 关键帧回看：replay_keyframes 的结果直接渲染为截图条，而非原始 JSON
  const replayFrames = t.label === "replay_keyframes" ? parseKeyframes(t.detail) : null;
  // 命令执行类工具：提供「白泽终端实时查看」入口
  const termCmd = COMMAND_TOOLS.has(toolName(t.label)) ? extractCommand(t.callArgs) : null;
  const [termErr, setTermErr] = useState(false);
  return (
    <div className="flow-line">
      <div
        className={`think-line flow-${node}${hasDetail ? " flow-clickable" : ""}`}
        onClick={() => hasDetail && setOpen((v) => !v)}
        title={hasDetail ? (open ? "收起详情" : "展开详情") : undefined}
      >
        <span className="flow-node" />
        <span className={`exec-tag ${isExec ? "exec" : "think"}`}>
          {isExec ? "执行" : "思考"}
        </span>
        <span className="think-label">{t.label}</span>
        {status && (
          <span className={`tool-status ${status}`}>{status === "success" ? "✓" : "✕"}</span>
        )}
        {termCmd && (
          <span
            className="flow-term-btn"
            onClick={(e) => {
              e.stopPropagation();
              void openTerminalWithCommand(termCmd)
                .then(() => setTermErr(false))
                .catch(() => setTermErr(true));
            }}
            title={`在白泽终端中实时查看：${termCmd}`}
          >
            终端
          </span>
        )}
        {hasDetail && <span className={`flow-caret${open ? " open" : ""}`}>▾</span>}
      </div>
      {termErr && (
        <div className="flow-term-err">终端打开失败（会话未就绪或窗口被禁用）</div>
      )}
      {open && hasDetail &&
        (replayFrames && replayFrames.length > 0 ? (
          <KeyframeStrip frames={replayFrames} />
        ) : (
          <pre className="flow-detail">{t.detail}</pre>
        ))}
    </div>
  );
}

/** 应用头像：优先按官网域名加载真实 favicon，失败或缺失回退首字母色块 */
function AppAvatar({ t }: { t: ThoughtEvent }) {
  const [err, setErr] = useState(false);
  const name = (t.label || "").replace(/^安装 · /, "").trim() || "?";
  const iconUrl = useMemo(() => {
    if (!t.homepage) return null;
    try {
      const host = new URL(t.homepage).hostname;
      return host ? `https://icons.duckduckgo.com/ip3/${host}.ico` : null;
    } catch {
      return null;
    }
  }, [t.homepage]);
  if (iconUrl && !err) {
    return (
      <img
        className="flow-progress-avatar"
        src={iconUrl}
        alt={name}
        onError={() => setErr(true)}
      />
    );
  }
  return <span className="flow-progress-avatar fallback">{name.slice(0, 1).toUpperCase()}</span>;
}

/** 圆形进度环：进行中显示百分比数字，完成显示 √，失败显示 ✕ */
function ProgressRing({ pct, phase }: { pct: number; phase?: string }) {
  const done = phase === "done";
  const failed = phase === "failed";
  const radius = 15;
  const circumference = 2 * Math.PI * radius;
  const dash = circumference * (1 - pct / 100);
  const color = failed ? "#f59e0b" : done ? "#16a34a" : "#22d3ee";
  return (
    <svg className="flow-ring" width="36" height="36" viewBox="0 0 36 36" aria-label={`${pct}%`}>
      <circle className="flow-ring-bg" cx="18" cy="18" r={radius} />
      <circle
        className="flow-ring-fill"
        cx="18"
        cy="18"
        r={radius}
        stroke={color}
        strokeDasharray={circumference}
        strokeDashoffset={dash}
        transform="rotate(-90 18 18)"
      />
      <text
        x="18"
        y={done || failed ? "22.5" : "21"}
        textAnchor="middle"
        className={`flow-ring-glyph${done ? " done" : failed ? " failed" : ""}`}
      >
        {done ? "✓" : failed ? "✕" : pct}
      </text>
    </svg>
  );
}

/** 安装进度轨迹：头像 + 名称/厂商/版本 + 圆形进度环 + 实时输出行（软件管家安装时流式渲染） */
function ProgressLine({ t }: { t: ThoughtEvent }) {
  const pct = Math.max(0, Math.min(100, Math.round(t.progress ?? 0)));
  const failed = t.phase === "failed";
  const done = t.phase === "done";
  const meta = [t.vendor, t.version ? `v${t.version}` : null].filter(Boolean).join(" · ");
  return (
    <div className={`flow-progress${failed ? " failed" : ""}${done ? " done" : ""}`}>
      <div className="flow-progress-head">
        <AppAvatar t={t} />
        <span className="flow-progress-title">
          <span className="flow-progress-name">{t.label}</span>
          {meta && <span className="flow-progress-meta">{meta}</span>}
        </span>
        <ProgressRing pct={pct} phase={t.phase} />
      </div>
      {t.detail && <div className="flow-progress-msg">{t.detail}</div>}
    </div>
  );
}

interface ExecutionFlowProps {
  /** 提供则渲染「历史执行流」；不提供则读取 live store（进行中的执行流） */
  thoughts?: ThoughtEvent[];
  todos?: Todo[];
  /** 历史执行流默认折叠 */
  defaultOpen?: boolean;
  /** 已结束：不显示「思考中…」与脉冲 */
  done?: boolean;
  /** 提供则把「回放执行流」入口并入头部一行（纪录片式逐步回放） */
  onReplay?: () => void;
}

/**
 * 执行流：把任务进度（todos）与思考过程（thoughts）合并到对话框内，
 * 类似 Trae CN 的对话执行流 —— 阶段切换 + 工具/记忆/模型轨迹 + 任务清单进度。
 * 支持两种模式：
 *  - live：进行中实时渲染（读 store）
 *  - frozen：任务结束后，从消息 trace 里展开回看（传 props）
 * 折叠态在头部以单行摘要展示整个流程（工具名 + ✓/✕），回放入口也并入该行。
 */
export default function ExecutionFlow({
  thoughts: frozenThoughts,
  todos: frozenTodos,
  defaultOpen = true,
  done = false,
  onReplay,
}: ExecutionFlowProps) {
  const liveThoughts = useChat((s) => s.thoughts);
  const liveTodos = useChat((s) => s.todos);
  const currentConvId = useChat((s) => s.currentConvId);
  const [open, setOpen] = useState(defaultOpen);

  const thoughts = useMemo(() => {
    const raw =
      frozenThoughts ??
      liveThoughts.filter((t) => t.kind !== "awaken" && t.convId === currentConvId);
    return dropRedundantExecPhase(mergeToolEvents(compactProgress(raw)));
  }, [frozenThoughts, liveThoughts, currentConvId]);
  const todos = frozenTodos ?? liveTodos;

  // 折叠态头部的单行流程摘要：规划 → markdown_append ✓ → 反思 → 完成
  const summary = useMemo(() => {
    const tokens: string[] = [];
    for (const t of thoughts) {
      if (t.kind === "phase") {
        tokens.push(t.label);
      } else if (t.kind === "tool_call") {
        const mark = t.status === "failed" ? " ✕" : t.status === "success" ? " ✓" : "";
        tokens.push(toolName(t.label) + mark);
      } else if (t.kind === "subagent") {
        tokens.push(t.label);
      }
      // 其余（memory/model/thinking 等信息性事件）不进单行摘要，保持精炼
    }
    return tokens.join(" → ");
  }, [thoughts]);

  const doneCount = todos.filter((t) => t.status === "completed").length;
  const pct = todos.length ? Math.round((doneCount / todos.length) * 100) : 0;

  // 已结束且没有任何内容时不渲染（避免空执行流占位）
  if (done && todos.length === 0 && thoughts.length === 0) return null;

  return (
    <div className="think-block">
      <div className="think-head" onClick={() => setOpen((v) => !v)}>
        <span className={`think-caret ${open ? "open" : ""}`}>▸</span>
        <span className="think-title">执行流</span>
        {todos.length > 0 && (
          <span className="think-count">
            {doneCount}/{todos.length}
          </span>
        )}
        {!open && summary && <span className="flow-summary">{summary}</span>}
        {!done && <span className="think-pulse" />}
        {onReplay && done && (
          <button
            className="flow-replay"
            title="回放执行流（逐步纪录片）"
            onClick={(e) => {
              e.stopPropagation();
              onReplay();
            }}
          >
            ▶ 回放
          </button>
        )}
      </div>

      {open && (
        <div className="think-body">
          {todos.length > 0 && (
            <div className="exec-todos">
              <div className="todo-bar">
                <div className="todo-bar-fill" style={{ width: `${pct}%` }} />
              </div>
              <ul className="todo-list">
                {todos.map((t) => (
                  <li key={t.id} className={`todo-item ${t.status}`}>
                    <span className="todo-icon">
                      {t.status === "completed" ? "✓" : t.status === "in_progress" ? "▶" : "·"}
                    </span>
                    <span className="todo-text">{t.title}</span>
                  </li>
                ))}
              </ul>
            </div>
          )}

          {thoughts.map((t, i) =>
            t.kind === "phase" ? (
              <div key={`${t.ts}-${i}`} className="exec-phase">
                <span className="exec-phase-icon">🧭</span>
                <span className="exec-phase-label">{t.label}</span>
                {t.detail && <span className="exec-phase-detail">{t.detail}</span>}
              </div>
            ) : t.kind === "saying" ? (
              <div key={`${t.ts}-${i}`} className="exec-saying">
                <span className="exec-saying-mark">“</span>
                <span className="exec-saying-text">{t.detail || t.label}</span>
                <span className="exec-saying-mark">”</span>
              </div>
            ) : t.kind === "tool_progress" ? (
              <ProgressLine key={`${t.ts}-${i}`} t={t} />
            ) : (
              <FlowLine key={`${t.ts}-${i}`} t={t} />
            )
          )}

          {!done && (
            <div className="think-line think-current">
              <span className="flow-node" />
              <span className="think-label">执行中…</span>
            </div>
          )}
        </div>
      )}
    </div>
  );
}