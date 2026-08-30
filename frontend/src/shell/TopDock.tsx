import { useEffect, useState } from "react";
import { useDock } from "../kernel/store/dock";
import { useChat } from "../kernel/store/chat";
import { getWechatStatus, getFeishuStatus } from "../kernel/api";
import type { FeishuStatus, WeChatStatus } from "../kernel/types";

/**
 * 顶部胶囊 Dock：品牌 · 会话 Chip · 快捷 Launcher · 链路状态 · IM · 时钟
 */
export function TopDock() {
  const { conversations, currentConvId } = useChat();
  const {
    toggleArchive, toggleSettings, compareMode, setCompareMode,
    settingsOpen, archiveOpen, pushNotice,
  } = useDock();
  const [wx, setWx] = useState<WeChatStatus | null>(null);
  const [fs, setFs] = useState<FeishuStatus | null>(null);
  const [now, setNow] = useState(() => new Date());
  const active = conversations.find((c) => c.id === currentConvId);
  const status = useChat((s) => ({ busy: s.busy, comparing: s.comparing }));

  useEffect(() => {
    const t = setInterval(() => setNow(new Date()), 1000);
    return () => clearInterval(t);
  }, []);

  useEffect(() => {
    let cancel = false;
    const refresh = () => {
      getWechatStatus()
        .then((s) => { if (!cancel) setWx(s as any); })
        .catch(() => {});
      getFeishuStatus()
        .then((s) => { if (!cancel) setFs(s as any); })
        .catch(() => {});
    };
    refresh();
    const t = setInterval(refresh, 8000);
    return () => { cancel = true; clearInterval(t); };
  }, [status.busy, status.comparing]);

  const hh = String(now.getHours()).padStart(2, "0");
  const mm = String(now.getMinutes()).padStart(2, "0");
  const ss = String(now.getSeconds()).padStart(2, "0");

  const wxOn = wx?.connected;
  const fsOn = fs?.connected;

  return (
    <header className="glass topdock">
      <div className="topdock-brand">
        <span className="logo-gem">◈</span>
        <span className="brand-word">白泽 · 水晶工坊</span>
        <span className="brand-sub">CRYSTAL WORKSHOP</span>
      </div>

      <button
        className="session-chip"
        onClick={() => toggleArchive(true)}
        title="切换/管理会话"
      >
        <span>会话</span>
        <strong>{active?.title ?? "新建草稿"}</strong>
      </button>

      <nav className="launcherbar">
        <button
          className={`launcher-btn ${archiveOpen ? "on" : ""}`}
          onClick={() => toggleArchive()}
        >
          ◳ 档案
        </button>
        <button
          className={`launcher-btn ${compareMode ? "on" : ""}`}
          onClick={() => {
            setCompareMode(!compareMode);
            pushNotice({
              tier: "info",
              title: compareMode ? "关闭棱镜对比" : "进入棱镜对比",
              body: compareMode ? "已退出多模型并排对比" : "下次发射时会同时调用所有启用模型",
            });
          }}
        >
          ◈ 对比
        </button>
        <button
          className={`launcher-btn ${settingsOpen ? "on" : ""}`}
          onClick={() => toggleSettings()}
        >
          ⚙ 控制台
        </button>
      </nav>

      <div className="topdock-right">
        <span className={`conn-status ${status.busy || status.comparing ? "pending" : ""}`}>
          <span className="conn-dot" />
          {status.busy ? "推理中" : status.comparing ? "多模型对比中" : "链路就绪"}
        </span>
        <span className={`imdot ${wxOn ? "on" : ""}`}><i>微</i>{wxOn ? "已连" : wx?.status ?? "微信"}</span>
        <span className={`imdot ${fsOn ? "on" : ""}`}><i>飞</i>{fsOn ? "已连" : fs?.status ?? "飞书"}</span>
        <span className="chronos">{hh}∶{mm}∶{ss}</span>
      </div>
    </header>
  );
}