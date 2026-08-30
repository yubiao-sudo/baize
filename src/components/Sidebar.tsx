import { useEffect, useMemo, useState } from "react";
import { createPortal } from "react-dom";
import { useChat } from "../stores/chat";
import {
  exportConversation,
  getWorkMode,
  getWorkModes,
  onWorkModeChange,
  pickFolder,
  setWorkMode,
} from "../api";
import type { Conversation, WorkModeInfo } from "../types";
import { derive } from "./AiActivity";

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

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void getWorkModes().then(setModes);
    void getWorkMode().then((s) => setCurrentMode(s.current));
    void onWorkModeChange((m) => setCurrentMode(m.id)).then((f) => {
      unlisten = f;
    });
    return () => unlisten?.();
  }, []);

  const onSelectMode = (id: string) => {
    setCurrentMode(id || null);
    void setWorkMode(id);
  };

  const currentModeInfo = modes.find((m) => m.id === currentMode);

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

  return (
    <aside className="sidebar">
      <div className="mode-section" style={{ padding: "0 16px 12px" }}>
        <select
          className="mode-select"
          value={currentMode ?? ""}
          onChange={(e) => onSelectMode(e.target.value)}
          title="选择工作模式"
        >
          <option value="">🧭 通用模式</option>
          {modes.map((m) => (
            <option key={m.id} value={m.id}>
              {m.label}
            </option>
          ))}
        </select>

        {currentModeInfo && (
          <div className="mode-detail">
            <div className="mode-detail-desc">{currentModeInfo.description}</div>

            {currentModeInfo.doc_templates.length > 0 && (
              <div className="mode-detail-group">
                <div className="mode-detail-title">产出文档</div>
                {currentModeInfo.doc_templates.map((d) => (
                  <div className="mode-detail-item" key={d.id} title={d.outline.join(" ／ ")}>
                    {d.title}
                  </div>
                ))}
              </div>
            )}

            {currentModeInfo.tool_templates.length > 0 && (
              <div className="mode-detail-group">
                <div className="mode-detail-title">可自研工具</div>
                {currentModeInfo.tool_templates.map((t) => (
                  <div className="mode-detail-item" key={t.name} title={t.description}>
                    {t.name}
                  </div>
                ))}
              </div>
            )}
          </div>
        )}
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
        <span className="chip">本地优先 · 云端回退</span>
        <span className="chip">安全 · 只读默认</span>
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
    </aside>
  );
}
