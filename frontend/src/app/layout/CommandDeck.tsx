import { useEffect, useRef, useState } from "react";
import type { KeyboardEvent as ReactKeyboardEvent } from "react";
import { pickFiles, setActiveModel } from "../../core/api";
import { useChat } from "../../core/store/chat";
import { useHud } from "../../core/store/hud";

const basename = (p: string) => p.split(/[\\/]/).pop() ?? p;

/** 指令台：模型切换 + 指令输入 + 附件 + 分支对比 + 发射/中止 */
export default function CommandDeck() {
  const send = useChat((s) => s.send);
  const compare = useChat((s) => s.compare);
  const stop = useChat((s) => s.stop);
  const busy = useChat((s) => s.busy);
  const comparing = useChat((s) => s.comparing);
  const models = useHud((s) => s.models);
  const setModels = useHud((s) => s.setModels);
  const draft = useHud((s) => s.draft);
  const consumeDraft = useHud((s) => s.consumeDraft);

  const [text, setText] = useState("");
  const [attachments, setAttachments] = useState<string[]>([]);
  const [menuOpen, setMenuOpen] = useState(false);
  const taRef = useRef<HTMLTextAreaElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);

  const active = models?.profiles.find((p) => p.id === models.active);
  const running = busy || comparing;

  // 首页点选的指令种子注入输入舱
  useEffect(() => {
    if (draft) {
      setText(draft);
      consumeDraft();
      taRef.current?.focus();
    }
  }, [draft, consumeDraft]);

  // 点击外部关闭模型菜单
  useEffect(() => {
    if (!menuOpen) return;
    const onDown = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) setMenuOpen(false);
    };
    window.addEventListener("mousedown", onDown);
    return () => window.removeEventListener("mousedown", onDown);
  }, [menuOpen]);

  const autoGrow = () => {
    const el = taRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 168)}px`;
  };

  const launch = () => {
    const msg = text.trim();
    if (!msg || running) return;
    setText("");
    setAttachments([]);
    requestAnimationFrame(() => {
      const el = taRef.current;
      if (el) el.style.height = "auto";
    });
    void send(msg, attachments);
  };

  const branch = () => {
    const msg = text.trim();
    if (!msg || running) return;
    setText("");
    setAttachments([]);
    void compare(msg);
  };

  const attach = async () => {
    const files = await pickFiles();
    if (files) setAttachments((a) => [...a, ...files]);
  };

  const pickModel = async (id: string) => {
    setMenuOpen(false);
    try {
      const c = await setActiveModel(id);
      setModels(c);
    } catch {
      /* 切换失败保持原样 */
    }
  };

  const onKeyDown = (e: ReactKeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      launch();
    }
  };

  return (
    <footer className="deck">
      <div className="deck-left">
        <div className="deck-model" ref={menuRef}>
          <button className="model-chip" onClick={() => setMenuOpen((v) => !v)}>
            <i className={`tier ${active?.tier ?? "cloud"}`}>
              {active?.tier === "local" ? "本地" : "云端"}
            </i>
            <span>{active?.name ?? "未配置"}</span>
            <em>▾</em>
          </button>
          {menuOpen && (
            <div className="model-menu">
              {models?.profiles.map((p) => (
                <button
                  key={p.id}
                  className={`model-item ${p.id === models.active ? "on" : ""} ${p.enabled ? "" : "off"}`}
                  onClick={() => void pickModel(p.id)}
                >
                  <i className={`tier ${p.tier}`}>{p.tier === "local" ? "本地" : "云端"}</i>
                  <span>{p.name}</span>
                  <code>{p.model}</code>
                </button>
              ))}
            </div>
          )}
        </div>
        <button className="deck-btn" onClick={() => void attach()} title="附加文件">
          ✚
        </button>
        <button
          className="deck-btn"
          onClick={branch}
          disabled={running || !text.trim()}
          title="多模型分支对比"
        >
          ⇄
        </button>
      </div>

      <div className="deck-core">
        {attachments.length > 0 && (
          <div className="deck-attach">
            {attachments.map((a) => (
              <span key={a} className="chip" title={a}>
                {basename(a)}
                <i onClick={() => setAttachments((xs) => xs.filter((x) => x !== a))}>✕</i>
              </span>
            ))}
          </div>
        )}
        <textarea
          ref={taRef}
          className="deck-input"
          value={text}
          placeholder="输入指令协议 · Enter 发射 · Shift+Enter 换行"
          onChange={(e) => {
            setText(e.target.value);
            autoGrow();
          }}
          onKeyDown={onKeyDown}
          rows={1}
        />
      </div>

      <div className="deck-launch">
        {running ? (
          <button className="hexbtn stop" onClick={stop} title="中止">
            ■
          </button>
        ) : (
          <button className="hexbtn" onClick={launch} disabled={!text.trim()} title="发射">
            ▲
          </button>
        )}
      </div>
    </footer>
  );
}