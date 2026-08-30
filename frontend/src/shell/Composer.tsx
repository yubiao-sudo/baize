import { useEffect, useRef, useState } from "react";
import { useChat } from "../kernel/store/chat";
import { useDock } from "../kernel/store/dock";
import {
  pickFiles,
  pickFolder,
  setWorkspace,
  getModelConfig,
  setActiveModel,
  compareModels as runCompare,
} from "../kernel/api";
import type { ModelConfig, ModelProfile } from "../kernel/types";

/**
 * 底部作曲家 Dock：
 * 模型芯片 · 附件 · 输入（+ / 🗁）· 玫瑰-紫晶发射球
 */
export function Composer() {
  const send = useChat((s) => s.send);
  const stop = useChat((s) => s.stop);
  const history = useChat((s) => s.history);
  const busy = useChat((s) => s.busy);
  const comparing = useChat((s) => s.comparing);
  const compare = useChat((s) => s.compare);
  const pushNotice = useDock((s) => s.pushNotice);
  const {
    compareMode, setCompareMode,
    modelMenuAnchor, setModelMenuAnchor,
    activeModels, currentModelId, setCurrentModelId, setActiveModels,
  } = useDock();

  const [text, setText] = useState("");
  const [attach, setAttach] = useState<string[]>([]);
  const taRef = useRef<HTMLTextAreaElement | null>(null);
  const menuRef = useRef<HTMLDivElement | null>(null);

  // 外部模型配置变更热同步
  const refreshModels = () =>
    getModelConfig()
      .then((cfg: ModelConfig) => {
        setActiveModels(cfg.profiles as ModelProfile[]);
        if (cfg.active) setCurrentModelId(cfg.active);
      })
      .catch(() => {});
  useEffect(() => { void refreshModels(); /* eslint-disable-next-line */ }, []);

  // autosize
  useEffect(() => {
    const el = taRef.current; if (!el) return;
    el.style.height = "auto";
    el.style.height = Math.min(el.scrollHeight, 180) + "px";
  }, [text]);

  // 点击菜单外部关闭
  useEffect(() => {
    if (!modelMenuAnchor) return;
    const fn = (e: MouseEvent) => {
      const t = e.target as Node;
      if (menuRef.current?.contains(t)) return;
      setModelMenuAnchor(null);
    };
    window.addEventListener("mousedown", fn);
    return () => window.removeEventListener("mousedown", fn);
  }, [modelMenuAnchor, setModelMenuAnchor]);

  const handleSend = async () => {
    const t = text.trim();
    if (!t || busy || comparing) return;
    const payload = t;
    const at = [...attach];
    setText("");
    setAttach([]);
    if (compareMode) {
      // compareModels 返回各模型应答数组 → 直接塞到 compare() 后端封装，后端可能有自处理
      try { await compare(payload); } catch (e) { pushNotice({ tier: "alert", title: "对比失败", body: String(e) }); }
    } else {
      await send(payload, at);
    }
  };

  const onKey = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      void handleSend();
    }
  };

  const upload = async () => {
    try {
      const p = await pickFiles();
      if (Array.isArray(p)) setAttach((a) => [...a, ...p]);
    } catch {}
  };

  const pickWs = async () => {
    try {
      const p = await pickFolder();
      if (p) {
        setAttach((a) => [...a, `[ws]${p}`]);
        await setWorkspace(p);
        pushNotice({ tier: "info", title: "已绑定工作目录", body: p.split(/[\\/]/).slice(-1)[0] });
      }
    } catch {}
  };

  const switchModel = async (id: string) => {
    setCurrentModelId(id);
    setModelMenuAnchor(null);
    await setActiveModel(id).then((cfg: ModelConfig) => {
      setActiveModels(cfg.profiles as ModelProfile[]);
      pushNotice({ tier: "info", title: "切换模型", body: `已激活 ${cfg.profiles.find((x) => x.id === id)?.name ?? id}` });
    }).catch(() => {});
  };

  const activeModel = activeModels.find((m) => m.id === currentModelId) ?? activeModels.find((m) => m.enabled) ?? null;
  const rect = modelMenuAnchor?.getBoundingClientRect();

  return (
    <footer className="glass composer">
      <div className="composer-toprow">
        <button
          className="model-chip2"
          onClick={(e) => setModelMenuAnchor(modelMenuAnchor ? null : (e.currentTarget as HTMLElement))}
          disabled={busy || comparing}
        >
          <span>◈ {activeModel?.name ?? "选择模型"}</span>
          <i>{activeModel ? `${activeModel.tier === "cloud" ? "☁" : "⌂"} · ${activeModel.model}` : "未启用模型"}</i>
        </button>

        {attach.map((p, i) => (
          <span className="attach-pill" key={p + i}>
            {p.startsWith("[ws]") ? "🗁" : "📄"}
            {p.replace(/^\[ws\]/, "").split(/[\\/]/).slice(-1)[0]}
            <i onClick={() => setAttach((a) => a.filter((_, j) => j !== i))}>✕</i>
          </span>
        ))}

        <button
          className={`launcher-btn ${compareMode ? "on" : ""}`}
          style={{ marginLeft: "auto" }}
          onClick={() => setCompareMode(!compareMode)}
          disabled={busy || comparing}
        >
          ◈ 对比发射
        </button>
      </div>

      <div className="composer-input-wrap">
        <div className="auxbuttons">
          <button className="circlebtn" onClick={upload} disabled={busy || comparing} title="上传附件">＋</button>
          <button className="circlebtn" onClick={pickWs} disabled={busy || comparing} title="绑定工作目录">🗁</button>
        </div>
        <textarea
          ref={taRef}
          className="composer-input"
          value={text}
          placeholder={busy ? "推理中… 可再输入或点击珊瑚球中止" : "向水晶工坊输入想法 · Enter 发射 / Shift+Enter 换行"}
          rows={1}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={onKey}
        />
        <button
          className={`sendorb ${busy || comparing ? "stop" : ""}`}
          onClick={() => (busy || comparing ? stop() : void handleSend())}
          disabled={!busy && !comparing && !text.trim()}
          title={busy || comparing ? "中止生成" : "发射"}
        >
          {busy || comparing ? "✕" : "▶"}
        </button>
      </div>

      {modelMenuAnchor && rect && (
        <div
          ref={menuRef}
          className="popup-menu"
          style={{ position: "fixed", bottom: `calc(100vh - ${rect.top}px + 8px)`, left: rect.left }}
        >
          {activeModels.length === 0 && <div className="mirow" style={{ color: "var(--faint)" }}>暂无模型档案（控制台内新增）</div>}
          {activeModels.map((m) => (
            <button
              key={m.id}
              className={`mirow ${m.id === currentModelId ? "on" : ""} ${m.enabled ? "" : "off"}`}
              onClick={() => switchModel(m.id)}
            >
              <span style={{ width: 5, height: 5, borderRadius: 999, background: m.enabled ? (m.tier === "cloud" ? "var(--orchid)" : "var(--violet-hi)") : "var(--faint)" }} />
              <span>{m.name} <small style={{ color: "var(--faint)", fontSize: 10 }}>({m.tier})</small></span>
              <code>{m.model}</code>
            </button>
          ))}
        </div>
      )}
    </footer>
  );
}