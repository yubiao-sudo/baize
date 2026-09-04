import { lazy, Suspense, useEffect, useRef, useState } from "react";
import Sidebar from "./components/Sidebar";
import ChatView from "./components/ChatView";
import ConsciousnessNetwork from "./components/ConsciousnessNetwork";
import { reactiveSpeak, speakWithCloud, stopSpeaking } from "./voiceReactive";
import ThoughtStream from "./components/ThoughtStream";

// 按需弹出的面板/卡片使用懒加载，减小主界面首屏 JS 解析量
const AcuiCard = lazy(() => import("./components/AcuiCard"));
const SettingsModal = lazy(() => import("./components/SettingsModal"));
const CommandPalette = lazy(() => import("./components/CommandPalette"));
const MemoryGalaxy = lazy(() => import("./components/MemoryGalaxy"));
const SoftwareButler = lazy(() => import("./components/SoftwareButler"));
const SchedulePanel = lazy(() => import("./components/SchedulePanel"));
const WorkflowPanel = lazy(() => import("./components/WorkflowPanel"));
const PlazaPanel = lazy(() => import("./components/PlazaPanel"));
const ImLogPanel = lazy(() => import("./components/ImLogPanel"));
const MessageCenterPanel = lazy(() => import("./components/MessageCenterPanel"));
const MeetingRoomPanel = lazy(() => import("./components/MeetingRoomPanel"));
const ChromePanel = lazy(() => import("./components/ChromePanel"));
const UiTestPanel = lazy(() => import("./components/UiTestPanel"));
const Galaxy = lazy(() => import("./components/Galaxy"));
// 首次启动环境自检（全屏引导层）+ 非首次启动提示卡
const Onboarding = lazy(() => import("./components/Onboarding"));
import { EnvNotice } from "./components/Onboarding";
import {
  envGetState,
  getPendingPermissions,
  getWorkMode,
  onChatRoundReset,
  onChatToken,
  onEscalationCancelled,
  onEscalationLevel,
  onEscalationUpdate,
  onFeishuStatus,
  onPermissionRequest,
  onThought,
  onTodoList,
  onTodoUpdate,
  onWechatStatus,
  onWorkModeChange,
  onPanelControl,
  openTerminalWindow,
} from "./api";
import { emit as tauriEmit, listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useChat } from "./stores/chat";
import { derive } from "./components/AiActivity";
import { playSfx } from "./utils/sound";

// —— 标题栏图标（细线描边风格，参照 ZCode 单条式顶栏）——

/** 终端：圆角方框 + 提示符 `>_` */
const TerminalGlyph = () => (
  <svg
    viewBox="0 0 24 24"
    width="17"
    height="17"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.5"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <rect x="3.75" y="4.75" width="16.5" height="14.5" rx="3" />
    <path d="M7.5 9.75l2.75 2.25-2.75 2.25" />
    <path d="M12.75 14.5h3.75" />
  </svg>
);

/** 面板收纳：圆角方框 + 竖分隔线 + 箭头（展开时 ‹ 收起，收起时 › 展开） */
const PanelGlyph = ({ collapsed }: { collapsed: boolean }) => (
  <svg
    viewBox="0 0 24 24"
    width="17"
    height="17"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.5"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <rect x="3.75" y="4.75" width="16.5" height="14.5" rx="3" />
    <path d="M14.75 4.75v14.5" />
    {collapsed ? (
      <path d="M9.25 9.75L11.5 12l-2.25 2.25" />
    ) : (
      <path d="M11.5 9.75L9.25 12l2.25 2.25" />
    )}
  </svg>
);

/** 最小化：一条细横线 */
const MinGlyph = () => (
  <svg
    viewBox="0 0 24 24"
    width="15"
    height="15"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.5"
    strokeLinecap="round"
    aria-hidden="true"
  >
    <path d="M6 12.25h12" />
  </svg>
);

/** 最大化：细线方框 */
const MaxGlyph = () => (
  <svg
    viewBox="0 0 24 24"
    width="15"
    height="15"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.5"
    aria-hidden="true"
  >
    <rect x="6.25" y="6.25" width="11.5" height="11.5" rx="1.5" />
  </svg>
);

/** 向下还原：前后两个叠放的细线方框 */
const RestoreGlyph = () => (
  <svg
    viewBox="0 0 24 24"
    width="15"
    height="15"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.5"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <rect x="5.25" y="8.25" width="10.5" height="10.5" rx="1.5" />
    <path d="M8.75 5.75h9a1 1 0 0 1 1 1v9" />
  </svg>
);

/** 关闭：细线 ✕ */
const CloseGlyph = () => (
  <svg
    viewBox="0 0 24 24"
    width="15"
    height="15"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.5"
    strokeLinecap="round"
    aria-hidden="true"
  >
    <path d="M6.5 6.5l11 11M17.5 6.5l-11 11" />
  </svg>
);

/** 功能菜单：三个圆点 */
const MoreGlyph = () => (
  <svg viewBox="0 0 24 24" width="17" height="17" fill="currentColor" aria-hidden="true">
    <circle cx="6" cy="12" r="1.4" />
    <circle cx="12" cy="12" r="1.4" />
    <circle cx="18" cy="12" r="1.4" />
  </svg>
);

/** 标题栏功能菜单条目（⋯ 下拉） */
const TB_MENU_ITEMS: Array<{ label: string; panel: string }> = [
  { label: "软件管家", panel: "butler" },
  { label: "定时任务", panel: "schedule" },
  { label: "可编排工作流", panel: "workflow" },
  { label: "任务广场", panel: "plaza" },
  { label: "IM 消息总线", panel: "imlog" },
  { label: "多 Agent 会议室", panel: "meeting" },
  { label: "Chrome 操控", panel: "chrome" },
];

export default function App() {
  const addPending = useChat((s) => s.addPending);
  const pendingCount = useChat((s) => s.pending.length);
  const addThought = useChat((s) => s.addThought);
  const setTodos = useChat((s) => s.setTodos);
  const appendStream = useChat((s) => s.appendStream);
  const resetStream = useChat((s) => s.resetStream);
  const loadConversations = useChat((s) => s.loadConversations);
  const loadProjects = useChat((s) => s.loadProjects);
  // 标题栏居中显示当前会话话题（首条用户消息命名）
  const conversations = useChat((s) => s.conversations);
  const currentConvId = useChat((s) => s.currentConvId);
  const convTitle =
    conversations.find((c) => c.id === currentConvId)?.title?.trim() || "新会话";
  // 右侧面板当前显示的内嵌面板（null 表示默认「意识网络 + 思考流」）
  const [activePanel, setActivePanel] = useState<string | null>(null);
  // Ctrl+K 命令面板
  const [cmdOpen, setCmdOpen] = useState(false);
  // 记忆星图（点击水球展开）
  const [galaxyOpen, setGalaxyOpen] = useState(false);
  // 无边框窗口：跟踪最大化状态（最大化 ⇆ 还原图标切换）
  const [maximized, setMaximized] = useState(false);
  // 标题栏「⋯」功能菜单
  const [menuOpen, setMenuOpen] = useState(false);
  // 首次启动环境自检：null = 未判定；true = 显示全屏引导；false = 正常进入（EnvNotice 自查提示）
  const [onboarding, setOnboarding] = useState<boolean | null>(null);
  useEffect(() => {
    envGetState()
      .then((st) => setOnboarding(!st.onboarding_done))
      .catch(() => setOnboarding(false));
  }, []);
  useEffect(() => {
    const win = getCurrentWindow();
    let disposed = false;
    let unlisten: (() => void) | undefined;
    const sync = () => {
      void win.isMaximized().then((v) => {
        if (!disposed) setMaximized(v);
      });
    };
    sync();
    void win.onResized(sync).then((f) => {
      if (disposed) f();
      else unlisten = f;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  // 订阅「打开记忆星图」（水球点击）
  useEffect(() => {
    const open = () => setGalaxyOpen(true);
    window.addEventListener("baize:open-galaxy", open);
    return () => window.removeEventListener("baize:open-galaxy", open);
  }, []);

  // 启动音：觉醒涟漪（AudioContext 被自动播放策略挂起时静默跳过）
  const startupPlayedRef = useRef(false);
  useEffect(() => {
    if (startupPlayedRef.current) return;
    startupPlayedRef.current = true;
    playSfx("startup");
  }, []);

  // 桌面悬浮球：活动状态广播 + 迷你面板输入转发 + 回主窗
  const busy = useChat((s) => s.busy);
  const streaming = useChat((s) => s.streaming);
  const thoughts = useChat((s) => s.thoughts);
  const send = useChat((s) => s.send);
  const activity = derive(thoughts, busy, streaming);
  useEffect(() => {
    void tauriEmit("baize-orb-status", { ...activity, busy }).catch(() => {});
  }, [activity.label, activity.detail, activity.tone, busy]);

  useEffect(() => {
    const unlisteners = [
      listen<{ text: string }>("baize-float-send", (e) => {
        const text = e.payload.text.trim();
        if (text && !useChat.getState().busy) void send(text);
      }),
      listen("baize-float-focus", () => {
        void getCurrentWindow().show();
        void getCurrentWindow().setFocus();
      }),
    ];
    return () => {
      void Promise.all(unlisteners).then((fs) => fs.forEach((f) => f()));
    };
  }, [send]);

  // 全局快捷键：Ctrl/Cmd+K 唤起命令面板
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setCmdOpen((v) => !v);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
  const [imBanner, setImBanner] = useState<string | null>(null);
  const [showRight, setShowRight] = useState(true);
  // 当前工作模式 id（"qa-engineer" 时显示「测试」入口）
  const [workMode, setWorkMode] = useState<string | null>(null);
  // 语音循环控制
  const voiceTimerRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const audioRef = useRef<HTMLAudioElement | null>(null);
  const voiceActiveRef = useRef(false);

  /** 停止所有语音播报（TTS + 音频文件） */
  const stopVoice = () => {
    voiceActiveRef.current = false;
    if (voiceTimerRef.current) {
      clearInterval(voiceTimerRef.current);
      voiceTimerRef.current = null;
    }
    if ("speechSynthesis" in window) {
      // 统一停止入口：本地 speechSynthesis + 云端音频一起停，并广播停止状态让水球恢复平静
      stopSpeaking();
    }
    if (audioRef.current) {
      audioRef.current.pause();
      audioRef.current.currentTime = 0;
      audioRef.current = null;
    }
  };

  /** 打开某内嵌面板：右侧面板列宽平滑过渡，面板内容淡入显示 */
  const openPanel = (name: string) => {
    setActivePanel(name);
    setShowRight(true);
  };

  /** 关闭内嵌面板：回到默认视图并保持侧边栏展开 */
  const closePanel = () => {
    setActivePanel(null);
    setShowRight(true);
  };

  // 预热懒加载面板：首屏挂载后空闲时预拉取各面板 chunk，避免首次点击打开时
  // Suspense 的 null 占位造成一闪而过的空白（闪烁）。import() 命中缓存，零副作用。
  useEffect(() => {
    const id = window.setTimeout(() => {
      void Promise.all([
        import("./components/SettingsModal"),
        import("./components/SoftwareButler"),
        import("./components/SchedulePanel"),
        import("./components/WorkflowPanel"),
        import("./components/PlazaPanel"),
        import("./components/ImLogPanel"),
        import("./components/MeetingRoomPanel"),
        import("./components/ChromePanel"),
        import("./components/UiTestPanel"),
      ]);
    }, 600);
    return () => window.clearTimeout(id);
  }, []);

  // 订阅工作模式切换：仅「软件测试工程师」显示测试入口，切换走时自动关闭测试面板
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void getWorkMode().then((s) => {
      if (!disposed) setWorkMode(s.current);
    });
    void onWorkModeChange((m) => {
      if (disposed) return;
      setWorkMode(m.id || null);
      if (m.id !== "qa-engineer") {
        setActivePanel((p) => (p === "test" ? null : p));
      }
    }).then((f) => {
      if (!disposed) unlisten = f;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  // 订阅 agent 面板控制：panel_control 工具打开/关闭顶栏功能页面（模型自主决策）
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void onPanelControl((p) => {
      if (disposed) return;
      if (p.action === "open" && p.panel) openPanel(p.panel);
      else closePanel();
    }).then((f) => {
      if (!disposed) unlisten = f;
      else f();
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    let disposed = false;
    const unlisteners: Array<() => void> = [];

    const setup = async () => {
      const regs = await Promise.all([
        onPermissionRequest((req) => !disposed && addPending(req)),
        onThought((t) => !disposed && addThought(t)),
        onTodoList((todos) => !disposed && setTodos(todos)),
        onTodoUpdate((todos) => !disposed && setTodos(todos)),
        onChatToken((token) => !disposed && appendStream(token)),
        onChatRoundReset(() => !disposed && resetStream()),
        // 通知升级：系统通知 + 语音播报（循环）
        onEscalationLevel((e) => {
          if (disposed) return;
          if (e.action === "system_notify") {
            // 系统通知 + 提示音
            if ("Notification" in window && Notification.permission === "granted") {
              new Notification(e.title, { body: e.body, icon: "/icon.png" });
            } else if ("Notification" in window && Notification.permission !== "denied") {
              Notification.requestPermission().then((p) => {
                if (p === "granted") {
                  new Notification(e.title, { body: e.body, icon: "/icon.png" });
                }
              });
            }
            // 升级警报音：三声渐强的上行脉冲（音效引擎合成）
            playSfx("escalation");
          }
          if (e.action === "voice") {
            // 停止之前的语音
            stopVoice();
            voiceActiveRef.current = true;

            const playVoice = () => {
              if (!voiceActiveRef.current) return;
              // 播放音频文件（歌曲/音效）
              if (e.audio_file) {
                const audio = new Audio(e.audio_file);
                audio.loop = false;
                audio.volume = 0.8;
                audio.play().catch(() => {});
                audioRef.current = audio;
              }
              // TTS 语音播报：跟随设置页的语音模型配置（本地/云端/豆包均走 speakWithCloud），
              // 全部合成失败时回退浏览器 Web Speech，保证审批提醒一定能出声
              const ttsText = e.tts_text;
              if (ttsText) {
                void speakWithCloud(ttsText).catch(() => {
                  if (!voiceActiveRef.current) return;
                  reactiveSpeak(ttsText, { lang: "zh-CN", rate: 1.0, volume: 1.0 });
                });
              }
            };

            // 立即播放一次
            playVoice();
            // 循环播放：音频文件每 8s 重播，TTS 每 10s 重播
            if (e.repeat) {
              voiceTimerRef.current = setInterval(() => {
                if (!voiceActiveRef.current) {
                  if (voiceTimerRef.current) clearInterval(voiceTimerRef.current);
                  return;
                }
                playVoice();
              }, 10000);
            }
          }
        }),
        // 当升级状态变化或用户响应时，停止语音
        onEscalationUpdate((e) => {
          if (disposed) return;
          if (e.max_level && e.level < 2) {
            stopVoice();
          }
        }),
        // 用户响应审批后，后端取消升级，前端停止语音
        onEscalationCancelled(() => {
          if (!disposed) stopVoice();
        }),
      ]);
      if (disposed) {
        regs.forEach((fn) => fn());
      } else {
        unlisteners.push(...regs);
      }
    };
    void setup();

    getPendingPermissions().then((list) => {
      if (!disposed) list.forEach(addPending);
    });
    void loadConversations();
    void loadProjects();

    return () => {
      disposed = true;
      unlisteners.forEach((fn) => fn());
    };
  }, [addPending, addThought, setTodos, appendStream, resetStream, loadConversations, loadProjects]);

  // IM 连接实时横幅：微信 / 飞书状态变化时短暂提示
  useEffect(() => {
    let disposed = false;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const show = (text: string) => {
      if (!text || disposed) return;
      setImBanner(text);
      if (timer) clearTimeout(timer);
      timer = setTimeout(() => setImBanner(null), 4000);
    };
    let offWx: () => void = () => {};
    let offFs: () => void = () => {};
    onWechatStatus((s) => {
      show(
        s.status === "connected"
          ? "微信已连接"
          : s.status === "disconnected"
            ? "微信已断开"
            : s.status === "qr_pending"
              ? "微信等待扫码"
              : "",
      );
    }).then((f) => (offWx = f));
    onFeishuStatus((s) => {
      show(
        s.status === "connected"
          ? "飞书已连接"
          : s.status === "connecting"
            ? "飞书连接中"
            : s.status === "reconnecting"
              ? "飞书重连中"
              : s.status === "disconnected"
                ? "飞书已断开"
                : "",
      );
    }).then((f) => (offFs = f));
    return () => {
      disposed = true;
      offWx();
      offFs();
      if (timer) clearTimeout(timer);
    };
  }, []);

  return (
    <div
      className={`console ${
        !showRight
          ? "right-collapsed"
          : activePanel
          ? activePanel === "meeting" || activePanel === "chrome"
            ? "panel-open-wide"
            : "panel-open"
          : ""
      }`}
    >
      {imBanner && (
        <div
          style={{
            position: "fixed",
            top: 16,
            left: "50%",
            transform: "translateX(-50%)",
            zIndex: 200,
            padding: "8px 16px",
            borderRadius: 999,
            background: "rgba(34,211,238,0.12)",
            border: "1px solid rgba(34,211,238,0.4)",
            color: "#e0f2fe",
            fontSize: 13,
            boxShadow: "0 0 12px rgba(34,211,238,0.35)",
            backdropFilter: "blur(6px)",
            pointerEvents: "none",
            whiteSpace: "nowrap",
          }}
        >
          {imBanner}
        </div>
      )}
      {/* 无边框单条式标题栏（参照 ZCode 顶栏）：logo + 居中会话标题 + 功能入口 + 窗口控制 */}
      <header className="titlebar" data-tauri-drag-region>
        <span className="brand-mark tb-logo" aria-hidden="true">
          泽
        </span>
        <div className="tb-title" data-tauri-drag-region title={convTitle}>
          {convTitle}
        </div>
        <div className="tb-right">
          <div className="tb-menu-wrap">
            <button
              className={`tb-icon-btn ${menuOpen ? "open" : ""}`}
              title="功能菜单"
              onClick={() => setMenuOpen((v) => !v)}
            >
              <MoreGlyph />
            </button>
            {menuOpen && (
              <>
                <div className="tb-menu-backdrop" onClick={() => setMenuOpen(false)} />
                <div className="tb-menu">
                  {TB_MENU_ITEMS.map((item) => (
                    <button
                      key={item.panel}
                      onClick={() => {
                        setMenuOpen(false);
                        openPanel(item.panel);
                      }}
                    >
                      {item.label}
                      {item.panel === "messages" && pendingCount > 0 && (
                        <span className="tb-menu-badge">{pendingCount}</span>
                      )}
                    </button>
                  ))}
                  {workMode === "qa-engineer" && (
                    <button
                      onClick={() => {
                        setMenuOpen(false);
                        openPanel("test");
                      }}
                    >
                      自动化测试
                    </button>
                  )}
                  <button
                    onClick={() => {
                      setMenuOpen(false);
                      openPanel("settings");
                    }}
                  >
                    设置
                  </button>
                </div>
              </>
            )}
          </div>
          <button
            className="tb-icon-btn"
            title="打开终端"
            onClick={() => void openTerminalWindow()}
          >
            <TerminalGlyph />
          </button>
          <button
            className="tb-icon-btn"
            title={showRight ? "收起右侧面板" : "展开右侧面板"}
            onClick={() => setShowRight((v) => !v)}
          >
            <PanelGlyph collapsed={!showRight} />
          </button>
          <div className="tb-controls">
            <button
              className="tb-win-btn"
              title="最小化"
              onClick={() => void getCurrentWindow().minimize()}
            >
              <MinGlyph />
            </button>
            <button
              className="tb-win-btn"
              title={maximized ? "向下还原" : "最大化"}
              onClick={() => void getCurrentWindow().toggleMaximize()}
            >
              {maximized ? <RestoreGlyph /> : <MaxGlyph />}
            </button>
            <button
              className="tb-win-btn close"
              title="关闭"
              onClick={() => void getCurrentWindow().close()}
            >
              <CloseGlyph />
            </button>
          </div>
        </div>
      </header>
      {/* 银河背景铺满整个窗口（根层），侧边栏/右侧面板的毛玻璃才能透出星河，
          缝隙与收起区域也不再是死黑断层 */}
      <div className="app-bg" aria-hidden>
        <Suspense fallback={null}>
          <Galaxy />
        </Suspense>
      </div>

      <Sidebar />

      <main className="main">
        <ChatView />
      </main>

      {/* 非首次启动：必需环境仍缺失时的非阻塞提示卡（自查缓存报告，通过则不渲染） */}
      {onboarding === false && <EnvNotice />}

      {/* 首次安装启动：全屏环境自检引导层 */}
      {onboarding === true && (
        <Suspense fallback={null}>
          <Onboarding onFinish={() => setOnboarding(false)} />
        </Suspense>
      )}

      <aside className={`right-panel ${showRight ? "" : "collapsed"}`}>
        <Suspense fallback={null}>
          {activePanel === "settings" ? (
            <SettingsModal onClose={closePanel} />
          ) : activePanel === "butler" ? (
            <SoftwareButler onClose={closePanel} />
          ) : activePanel === "schedule" ? (
            <SchedulePanel onClose={closePanel} />
          ) : activePanel === "workflow" ? (
            <WorkflowPanel onClose={closePanel} />
          ) : activePanel === "plaza" ? (
            <PlazaPanel onClose={closePanel} />
          ) : activePanel === "imlog" ? (
            <ImLogPanel onClose={closePanel} />
          ) : activePanel === "messages" ? (
            <MessageCenterPanel onClose={closePanel} />
          ) : activePanel === "meeting" ? (
            <MeetingRoomPanel onClose={closePanel} />
          ) : activePanel === "chrome" ? (
            <ChromePanel onClose={closePanel} />
          ) : activePanel === "test" ? (
            <UiTestPanel onClose={closePanel} />
          ) : (
            <>
              <ConsciousnessNetwork />
              <ThoughtStream />
            </>
          )}
        </Suspense>
      </aside>

      <Suspense fallback={null}>
        <AcuiCard />
      </Suspense>

      {/* Ctrl+K 命令面板 */}
      <Suspense fallback={null}>
        <CommandPalette open={cmdOpen} onClose={() => setCmdOpen(false)} openPanel={openPanel} />
      </Suspense>

      {/* 记忆星图 */}
      {galaxyOpen && (
        <Suspense fallback={null}>
          <MemoryGalaxy onClose={() => setGalaxyOpen(false)} />
        </Suspense>
      )}
    </div>
  );
}
