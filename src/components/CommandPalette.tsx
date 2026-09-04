import { useEffect, useMemo, useRef, useState } from "react";
import { useChat } from "../stores/chat";
import { getWorkMode, getWorkModes } from "../api";

/**
 * Ctrl+K 命令面板：毛玻璃快速操作入口。
 * - 秒切九大功能面板 / 新建会话 / 切换工作模式
 * - 没有匹配命令时直接回车 → 作为自然语言任务发给白泽
 * 键盘流：↑↓ 选择 / Enter 执行 / Esc 关闭。
 */

interface Command {
  id: string;
  label: string;
  hint: string;
  icon: string;
  keywords?: string;
  action: () => void;
}

export default function CommandPalette({
  open,
  onClose,
  openPanel,
}: {
  open: boolean;
  onClose: () => void;
  openPanel: (name: string) => void;
}) {
  const send = useChat((s) => s.send);
  const busy = useChat((s) => s.busy);
  const newConversation = useChat((s) => s.newConversation);
  const [query, setQuery] = useState("");
  const [cursor, setCursor] = useState(0);
  const [modeList, setModeList] = useState<{ id: string; label: string }[]>([]);
  const [currentMode, setCurrentMode] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);

  // 打开时聚焦 + 拉取工作模式（切模式命令）
  useEffect(() => {
    if (!open) return;
    setQuery("");
    setCursor(0);
    window.setTimeout(() => inputRef.current?.focus(), 30);
    void Promise.all([getWorkModes(), getWorkMode()])
      .then(([list, cur]) => {
        setModeList(list.map((m) => ({ id: m.id, label: m.label })));
        setCurrentMode(cur.current || "");
      })
      .catch(() => {});
  }, [open]);

  const commands = useMemo<Command[]>(() => {
    const out: Command[] = [
      { id: "p-butler", label: "打开 · 软件管家", hint: "安装 / 卸载软件", icon: "📦", action: () => openPanel("butler") },
      { id: "p-schedule", label: "打开 · 计划", hint: "定时任务", icon: "⏰", action: () => openPanel("schedule") },
      { id: "p-workflow", label: "打开 · 工作流", hint: "自动化流程编排", icon: "🧩", action: () => openPanel("workflow") },
      { id: "p-plaza", label: "打开 · 任务广场", hint: "工具 / 技能聚合", icon: "🧭", action: () => openPanel("plaza") },
      { id: "p-imlog", label: "打开 · IM 消息总线", hint: "微信 / 飞书消息", icon: "📨", action: () => openPanel("imlog") },
      { id: "p-meeting", label: "打开 · 会议室", hint: "多智能体讨论", icon: "🏛", action: () => openPanel("meeting") },
      { id: "p-chrome", label: "打开 · 浏览器", hint: "受控 Chrome", icon: "🌐", action: () => openPanel("chrome") },
      { id: "p-test", label: "打开 · 测试面板", hint: "用例 / 执行 / 报告", icon: "🧪", action: () => openPanel("test") },
      { id: "p-settings", label: "打开 · 设置", hint: "模型 / API Key / 运行时", icon: "⚙", action: () => openPanel("settings") },
      { id: "act-new", label: "新建会话", hint: "开启新对话", icon: "✦", action: () => void newConversation() },
      {
        id: "act-float",
        label: "桌面悬浮球 · 开 / 关",
        hint: "水球常驻桌面，随时查看状态派任务",
        icon: "🔮",
        keywords: "float orb 悬浮",
        action: () => {
          void import("../api").then(({ toggleFloatOrb }) => toggleFloatOrb().catch(() => {}));
        },
      },
      {
        id: "act-galaxy",
        label: "记忆星图",
        hint: "全屏浏览白泽的记忆宇宙",
        icon: "🌌",
        keywords: "memory galaxy 记忆",
        action: () => window.dispatchEvent(new CustomEvent("baize:open-galaxy")),
      },
      ...(["light-glass", "dark", "dawn"] as const).map((t) => ({
        id: `theme-${t}`,
        label:
          t === "light-glass"
            ? "主题 · 白色毛玻璃"
            : t === "dawn"
              ? "主题 · 深空黎明"
              : "主题 · 暗夜深邃",
        hint: "全局背景风格切换，立即生效并记住",
        icon: t === "light-glass" ? "🧊" : t === "dawn" ? "🌅" : "🌙",
        keywords: "theme 主题 背景 毛玻璃 light dark dawn 黎明 晨昏",
        action: () => {
          if (t === "dark") delete document.documentElement.dataset.theme;
          else document.documentElement.dataset.theme = t;
          try {
            if (t === "dark") localStorage.removeItem("baize-theme");
            else localStorage.setItem("baize-theme", t);
          } catch {}
        },
      })),
    ];
    if (modeList.length > 0) {
      for (const m of modeList) {
        if (m.id === currentMode) continue;
        out.push({
          id: `mode-${m.id}`,
          label: `切换工作模式 · ${m.label}`,
          hint: "会话级身份与工具白名单随之切换",
          icon: "🎭",
          keywords: "workmode",
          action: () => {
            void import("../api").then(({ setWorkMode }) => setWorkMode(m.id).catch(() => {}));
          },
        });
      }
    }
    return out;
  }, [openPanel, newConversation, modeList, currentMode]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return commands;
    return commands.filter((c) =>
      (c.label + " " + (c.keywords || "") + " " + c.hint).toLowerCase().includes(q),
    );
  }, [commands, query]);

  // 光标越界保护
  useEffect(() => {
    setCursor((c) => Math.min(c, Math.max(0, filtered.length - 1)));
  }, [filtered.length]);

  if (!open) return null;

  const run = (cmd: Command | null) => {
    if (cmd) {
      cmd.action();
      onClose();
      return;
    }
    // 无命令匹配：自然语言直接发给白泽
    const text = query.trim();
    if (text && !busy) void send(text);
    onClose();
  };

  return (
    <div className="cmdk-mask" onMouseDown={onClose}>
      <div className="cmdk" onMouseDown={(e) => e.stopPropagation()}>
        <div className="cmdk-input-row">
          <span className="cmdk-logo">✦</span>
          <input
            ref={inputRef}
            className="cmdk-input"
            placeholder="输入命令，或直接向白泽下达任务…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "ArrowDown") {
                e.preventDefault();
                setCursor((c) => Math.min(c + 1, filtered.length - 1));
              } else if (e.key === "ArrowUp") {
                e.preventDefault();
                setCursor((c) => Math.max(c - 1, 0));
              } else if (e.key === "Enter") {
                e.preventDefault();
                run(filtered[cursor] || null);
              } else if (e.key === "Escape") {
                onClose();
              }
            }}
          />
          <span className="cmdk-esc">Esc</span>
        </div>
        <div className="cmdk-list">
          {filtered.map((c, i) => (
            <button
              key={c.id}
              className={`cmdk-item${i === cursor ? " active" : ""}`}
              onMouseEnter={() => setCursor(i)}
              onClick={() => run(c)}
            >
              <span className="cmdk-icon">{c.icon}</span>
              <span className="cmdk-label">{c.label}</span>
              <span className="cmdk-hint">{c.hint}</span>
            </button>
          ))}
          {filtered.length === 0 && query.trim() && (
            <button className="cmdk-item active" onClick={() => run(null)}>
              <span className="cmdk-icon">➤</span>
              <span className="cmdk-label">发给白泽：{query.trim()}</span>
              <span className="cmdk-hint">自然语言任务</span>
            </button>
          )}
        </div>
        <div className="cmdk-foot">↑↓ 选择 · Enter 执行 · 无匹配时回车直接派任务</div>
      </div>
    </div>
  );
}
