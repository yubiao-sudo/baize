import { useCallback, useEffect, useMemo, useState } from "react";
import { createPortal } from "react-dom";
import { useChat } from "../stores/chat";
import {
  exportConversation,
  getModelConfig,
  getModelHealth,
  getModelUsageDaily,
  getWorkMode,
  getWorkModes,
  onWorkModeChange,
  pickFolder,
  setWorkMode,
} from "../api";
import type { Conversation, ModelConfig, WorkModeInfo } from "../types";
import { derive } from "./AiActivity";

/**
 * 生命体征线波形（viewBox 0 0 400 56，中线 y≈29）：
 * 起伏的小波折模拟真实体征的不规则性，两处 QRS 尖峰是"心跳"，
 * 两段心跳间距不等——刻意的，生命不是节拍器。
 */
const ECG_PATH = [
  "M0,31",
  "L12,27 L22,32 L34,26 L44,31 L54,28 L62,33 L70,27", // 起步的不规则波动
  "L78,30 L86,24 L92,32 L100,28 L108,31 L116,26", // 深呼吸式下沉
  "L122,31 L128,29 L134,33 L140,28", // 心跳前的小蓄力
  "L146,30 L152,25 L158,34 L164,7 L170,50 L176,16 L182,31", // 第一次心搏（QRS 尖峰）
  "L192,28 L204,32 L216,26 L228,30 L238,27 L248,33 L258,28 L266,31", // 平稳段的起伏
  "L272,26 L278,30 L284,27 L290,32", // 短促的小波动
  "L296,29 L302,25 L308,34 L314,9 L320,47 L326,18 L332,30", // 第二次心搏
  "L342,28 L354,32 L366,27 L378,30 L388,26 L400,29", // 收尾回中线
].join(" ");

/** 侧边栏视图：对话列表 / 项目分组 */
type SidebarView = "chat" | "projects";

/** 归档菜单状态：目标会话 + 弹出位置（视口坐标，portal 渲染避免被滚动容器裁剪） */
interface MoveMenu {
  convId: string;
  x: number;
  y: number;
}

/** 文件夹小图标（归档到项目） */
const FolderGlyph = () => (
  <svg
    viewBox="0 0 24 24"
    width="13"
    height="13"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.8"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <path d="M3.5 7.5v11A1.5 1.5 0 0 0 5 20h14a1.5 1.5 0 0 0 1.5-1.5v-9A1.5 1.5 0 0 0 19 8h-7L9.8 5.5H5A1.5 1.5 0 0 0 3.5 7z" />
  </svg>
);

export default function Sidebar() {
  const [view, setView] = useState<SidebarView>("chat");
  const [modes, setModes] = useState<WorkModeInfo[]>([]);
  const [currentMode, setCurrentMode] = useState<string | null>(null);
  const conversations = useChat((s) => s.conversations);
  const currentConvId = useChat((s) => s.currentConvId);
  const busy = useChat((s) => s.busy);
  const streaming = useChat((s) => s.streaming);
  const thoughts = useChat((s) => s.thoughts);
  const projects = useChat((s) => s.projects);
  const switchConversation = useChat((s) => s.switchConversation);
  const newConversation = useChat((s) => s.newConversation);
  const removeConversation = useChat((s) => s.removeConversation);
  const addProject = useChat((s) => s.addProject);
  const removeProject = useChat((s) => s.removeProject);
  const moveConversation = useChat((s) => s.moveConversation);
  // 项目分组展开状态（"ungrouped" 表示未分组组）
  const [expanded, setExpanded] = useState<string | null>(null);
  // 「归档到项目」弹出菜单
  const [moveMenu, setMoveMenu] = useState<MoveMenu | null>(null);

  /** 工作模式快捷切换菜单（状态卡徽标点开，portal 弹出） */
  const [modeMenu, setModeMenu] = useState<MoveMenu | null>(null);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void getWorkModes().then(setModes);
    void getWorkMode().then((s) => setCurrentMode(s.current));
    void onWorkModeChange((m) => setCurrentMode(m.id)).then((f) => {
      unlisten = f;
    });
    return () => unlisten?.();
  }, []);

  /** 直接切换工作模式（乐观更新本地，后端持久化并广播同步设置页） */
  const switchMode = (id: string) => {
    setModeMenu(null);
    if (id === currentMode) return;
    setCurrentMode(id);
    void setWorkMode(id);
  };

  /** 打开模式快捷菜单（定位到徽标下方） */
  const openModeMenu = (e: React.MouseEvent<HTMLButtonElement>) => {
    const r = e.currentTarget.getBoundingClientRect();
    setModeMenu({
      convId: "",
      x: Math.max(8, Math.min(r.left, window.innerWidth - 200)),
      y: Math.min(r.bottom + 6, window.innerHeight - 260),
    });
  };

  const currentModeInfo = modes.find((m) => m.id === currentMode);

  // ── 状态卡实时数据：当前模型 + 健康探活结果 + 今日 token 用量 ──
  const [modelCfg, setModelCfg] = useState<ModelConfig | null>(null);
  // null = 尚未探活（灰点）；true/false = 探活结果（绿/红点）
  const [activeHealth, setActiveHealth] = useState<boolean | null>(null);
  const [healthDetail, setHealthDetail] = useState("");
  const [todayUsage, setTodayUsage] = useState<{ tokens: number; calls: number } | null>(null);

  const refreshLive = useCallback(() => {
    getModelConfig().then(setModelCfg).catch(() => {});
    getModelUsageDaily(1)
      .then((rows) => {
        const today = new Date();
        const key = `${today.getFullYear()}-${String(today.getMonth() + 1).padStart(2, "0")}-${String(today.getDate()).padStart(2, "0")}`;
        const r = rows.find((x) => x.day === key);
        setTodayUsage(
          r
            ? { tokens: r.prompt_tokens + r.completion_tokens, calls: r.calls }
            : { tokens: 0, calls: 0 }
        );
      })
      .catch(() => {});
  }, []);

  // 模型配置到位后对齐健康状态；此后每 60s 轮询
  useEffect(() => {
    if (!modelCfg) return;
    let disposed = false;
    getModelHealth()
      .then((h) => {
        if (disposed) return;
        const item = h[modelCfg.active];
        if (item) {
          setActiveHealth(item.ok);
          setHealthDetail(item.detail);
        } else {
          setActiveHealth(null);
          setHealthDetail("");
        }
      })
      .catch(() => {});
    return () => {
      disposed = true;
    };
  }, [modelCfg]);

  useEffect(() => {
    refreshLive();
    const t = setInterval(refreshLive, 60_000);
    return () => clearInterval(t);
  }, [refreshLive]);

  // 任务结束瞬间立即刷新用量（不必等下一轮询）
  useEffect(() => {
    if (!busy) refreshLive();
  }, [busy, refreshLive]);

  const activeProfile = modelCfg?.profiles.find((p) => p.id === modelCfg.active);

  // 从思考流 + busy/streaming 派生当前 AI 活动状态（空闲/思考中/调用工具…），与「对话」导航项合并展示
  const activity = useMemo(() => derive(thoughts, busy, streaming), [thoughts, busy, streaming]);

  // 会话按项目分组（未设置项目的进入「未分组」），顺序保持后端的最近优先
  const grouped = useMemo(() => {
    const map = new Map<string, Conversation[]>();
    const ungrouped: Conversation[] = [];
    for (const c of conversations) {
      if (c.project_id) {
        const arr = map.get(c.project_id) ?? [];
        arr.push(c);
        map.set(c.project_id, arr);
      } else {
        ungrouped.push(c);
      }
    }
    return { map, ungrouped };
  }, [conversations]);

  /** 新建项目：选择一个工作目录，名称默认取文件夹名 */
  const onCreateProject = async () => {
    const path = await pickFolder();
    if (!path) return;
    const name = path.split(/[\\/]/).filter(Boolean).pop() || "新项目";
    await addProject(name, path);
    setView("projects");
  };

  /** 会话条目：对话 / 项目两个视图共用；项目视图额外提供「归档到项目」 */
  const renderConvItem = (c: Conversation, inProjects: boolean) => (
    <div
      key={c.id}
      className={`conv-item ${c.id === currentConvId ? "active" : ""} ${inProjects ? "in-group" : ""}`}
      onClick={() => {
        if (!busy) void switchConversation(c.id);
      }}
    >
      <span className="conv-title" title={c.title}>
        {c.title || "新会话"}
      </span>
      {inProjects && (
        <button
          className="conv-exp"
          title="归档到项目"
          onClick={(e) => {
            e.stopPropagation();
            const r = e.currentTarget.getBoundingClientRect();
            setMoveMenu({
              convId: c.id,
              x: Math.max(8, Math.min(r.right - 148, window.innerWidth - 160)),
              y: Math.min(r.bottom + 4, window.innerHeight - 220),
            });
          }}
        >
          <FolderGlyph />
        </button>
      )}
      <button
        className="conv-exp"
        title="导出对话（Markdown / JSON）"
        onClick={(e) => {
          e.stopPropagation();
          void exportConversation(c.id);
        }}
      >
        ⤓
      </button>
      <button
        className="conv-del"
        title="删除会话"
        onClick={(e) => {
          e.stopPropagation();
          void removeConversation(c.id);
        }}
      >
        ×
      </button>
    </div>
  );

  const moveTarget = moveMenu ? conversations.find((c) => c.id === moveMenu.convId) : null;

  /** token 数格式化：1234 → 1.2K，1234567 → 1.2M */
  const fmtTokens = (n: number) =>
    n >= 1_000_000
      ? `${(n / 1_000_000).toFixed(1)}M`
      : n >= 1_000
        ? `${(n / 1_000).toFixed(1)}K`
        : String(n);

  return (
    <aside className="sidebar">
      {/* 白泽状态卡：生命体征线（随状态变色）+ 模式/模型/用量实时信息 */}
      <div
        className={`baize-card tone-${activeHealth === false ? "bad" : activity.tone} ${
          activity.tone !== "idle" ? "executing" : ""
        }`}
      >
        <svg className="baize-card-ecg" viewBox="0 0 400 56" preserveAspectRatio="none" aria-hidden>
          <path className="baize-ecg-base" d={ECG_PATH} />
          <path className="baize-ecg-pulse" d={ECG_PATH} pathLength={400} />
        </svg>
        <button
          type="button"
          className="baize-card-mode"
          title="切换工作模式"
          onClick={openModeMenu}
        >
          <span className="baize-card-mode-label">{currentModeInfo?.label ?? "通用模式"}</span>
          <span className="baize-card-mode-arrow">›</span>
        </button>
        <div className="baize-card-meta">
          <div className="baize-card-row">
            <span
              className={`baize-dot ${activeHealth === null ? "unknown" : activeHealth ? "ok" : "bad"}`}
              title={activeHealth === null ? "尚未探活" : activeHealth ? "连接正常" : healthDetail || "连接异常"}
            />
            <span className="baize-card-row-main" title={activeProfile ? `${activeProfile.name} · ${activeProfile.model}` : ""}>
              {activeProfile?.name ?? "未配置模型"}
            </span>
            <span className="baize-card-row-side" title="今日消耗 token">
              {todayUsage ? fmtTokens(todayUsage.tokens) : "—"}
            </span>
          </div>
          <div className="baize-card-row dim">
            <span>{conversations.length} 会话</span>
            <span className="baize-card-sep" />
            <span>{projects.length} 项目</span>
            <span className="baize-card-sep" />
            <span>{todayUsage ? `${todayUsage.calls} 次调用` : "今日暂无调用"}</span>
          </div>
        </div>
      </div>

      {/* 导航：对话 / 项目（对话项合并展示 AI 活动状态，项目项展示数量徽标） */}
      <nav className="nav">
        <button
          type="button"
          className={`nav-item ${view === "chat" ? "active" : ""}`}
          onClick={() => setView("chat")}
        >
          <span className="nav-dot" />
          <span>对话</span>
          <span className="nav-activity" title={activity.detail || activity.label}>
            {activity.label}
          </span>
        </button>
        <button
          type="button"
          className={`nav-item ${view === "projects" ? "active" : ""}`}
          onClick={() => setView("projects")}
        >
          <span className="nav-dot" />
          <span>项目</span>
          <span className="nav-count" title={`${projects.length} 个项目`}>
            {projects.length}
          </span>
        </button>
      </nav>

      {view === "chat" ? (
        // ---- 对话视图：全部会话平铺（与原有行为一致） ----
        <div className="conv-section">
          <div className="conv-head">
            <span>会话</span>
            <button
              className="conv-add"
              title="新建会话"
              onClick={() => void newConversation()}
            >
              +
            </button>
          </div>
          <div className="conv-list">
            {conversations.map((c) => renderConvItem(c, false))}
          </div>
        </div>
      ) : (
        // ---- 项目视图：按项目分组展示会话，支持新建项目 / 项目内建会话 / 归档 ----
        <div className="conv-section">
          <div className="conv-head">
            <span>项目</span>
            <button className="conv-add" title="新建项目（选择工作目录）" onClick={() => void onCreateProject()}>
              +
            </button>
          </div>
          <div className="conv-list">
            {projects.map((p) => {
              const items = grouped.map.get(p.id) ?? [];
              const open = expanded === p.id;
              return (
                <div className="proj-group" key={p.id}>
                  <div
                    className={`proj-row ${open ? "open" : ""}`}
                    onClick={() => setExpanded(open ? null : p.id)}
                  >
                    <span className="proj-caret">▸</span>
                    <span className="proj-name" title={p.path}>
                      {p.name}
                    </span>
                    <span className="proj-count">{items.length}</span>
                    <button
                      className="proj-new"
                      title="在此项目新建会话"
                      onClick={(e) => {
                        e.stopPropagation();
                        setExpanded(p.id);
                        void newConversation(p.id);
                      }}
                    >
                      ＋
                    </button>
                    <button
                      className="proj-del"
                      title="删除项目（会话回到未分组，消息保留）"
                      onClick={(e) => {
                        e.stopPropagation();
                        void removeProject(p.id);
                      }}
                    >
                      ×
                    </button>
                  </div>
                  {open && items.map((c) => renderConvItem(c, true))}
                </div>
              );
            })}
            <div className="proj-group">
              <div
                className={`proj-row ungrouped ${expanded === "ungrouped" ? "open" : ""}`}
                onClick={() => setExpanded(expanded === "ungrouped" ? null : "ungrouped")}
              >
                <span className="proj-caret">▸</span>
                <span className="proj-name">未分组</span>
                <span className="proj-count">{grouped.ungrouped.length}</span>
              </div>
              {expanded === "ungrouped" && grouped.ungrouped.map((c) => renderConvItem(c, true))}
            </div>
          </div>
        </div>
      )}

      <div className="sidebar-chips">
        <span className="chip">本地优先 · 云端回退 · 安全 · 只读默认</span>
      </div>

      <div className="agent-status">
        <div className="status-row">
          <span className="pulse-dot" />
          <span>常驻运行中</span>
        </div>
        {currentMode && (
          <div className="status-row" style={{ color: "var(--text-faint)" }}>
            <span>工作模式 · {modes.find((m) => m.id === currentMode)?.label ?? currentMode}</span>
          </div>
        )}
      </div>

      {/* 「归档到项目」弹出菜单（portal 挂到 body，fixed 定位，避免被侧边栏裁剪） */}
      {moveMenu && moveTarget && createPortal(
        <>
          <div className="move-backdrop" onClick={() => setMoveMenu(null)} />
          <div className="move-menu" style={{ left: moveMenu.x, top: moveMenu.y }}>
            <div className="move-menu-title">归档到项目</div>
            {projects.length === 0 && (
              <div className="move-menu-empty">还没有项目，先在项目页右上角「＋」新建</div>
            )}
            {projects.map((p) => (
              <button
                key={p.id}
                title={p.path}
                onClick={() => {
                  setMoveMenu(null);
                  void moveConversation(moveTarget.id, p.id);
                }}
              >
                {p.name}
              </button>
            ))}
            {moveTarget.project_id && (
              <button
                className="move-menu-clear"
                onClick={() => {
                  setMoveMenu(null);
                  void moveConversation(moveTarget.id, null);
                }}
              >
                移出项目
              </button>
            )}
          </div>
        </>,
        document.body
      )}

      {/* 工作模式快捷切换菜单（状态卡徽标点开；复用 move-menu 弹层样式） */}
      {modeMenu && createPortal(
        <>
          <div className="move-backdrop" onClick={() => setModeMenu(null)} />
          <div className="move-menu" style={{ left: modeMenu.x, top: modeMenu.y }}>
            <div className="move-menu-title">切换工作模式</div>
            {[null, ...modes].map((m) => {
              const id = m?.id ?? "";
              const label = m?.label ?? "通用模式";
              const active = id === (currentMode ?? "");
              return (
                <button
                  key={id || "__general"}
                  className={active ? "move-menu-active" : undefined}
                  title={m?.description}
                  onClick={() => switchMode(id)}
                >
                  {label}
                  {active ? "  ·使用中" : ""}
                </button>
              );
            })}
          </div>
        </>,
        document.body
      )}
    </aside>
  );
}
