// ==========================================================================
// 对话细胞 · ConversationThread
//   listens:  chat.token / chat.turn.done / cmd.chat.send(回环观察)
//   emits:    cmd.chat.send / cmd.files.pick / cmd.folder.pick
// 特点：
//   - 信号驱动：每次收到 chat.token 只做「当前 tail 拼接」，不扫描整个历史
//   - 流式光标：bubble.cursor 闪烁
// ==========================================================================
import { useEffect, useMemo, useRef, useState } from "react";
import { PrismCell } from "../PrismCell";
import { useCell } from "../cell.hooks";
import type { ChatTurn, ModelProfileLite } from "../../bus/prism.types";

const MODELS: ModelProfileLite[] = [
  { id: "deepseek-r1",     name: "DeepSeek R1",     vendor: "DeepSeek", tier: "cloud", ready: true },
  { id: "qwen2.5-32b",     name: "Qwen 2.5 32B",    vendor: "Aliyun",   tier: "local", ready: true },
  { id: "claude-sonnet",   name: "Claude Sonnet",   vendor: "Anthropic",tier: "proxy", ready: true },
  { id: "llama-3.1-70b",   name: "Llama 3.1 70B",   vendor: "Meta",     tier: "local", ready: false },
];

export function ConversationThread() {
  const [turns, setTurns] = useState<ChatTurn[]>(() => [
    {
      turnId: "t-welcome",
      role: "assistant",
      text:
        "欢迎来到 **棱镜舱 · Prism Nexus** 🌈\n\n这是一套与旧前端完全隔离的全新架构：所有 UI 组件（Cell）通过「信号流总线」通信，不共享任何 store。\n\n试着发一条消息，我会流式回复，并在中途模拟工具调用、审批请求。点击右上角「切换主题」试试四套折射光配色。",
      sealed: true,
    },
  ]);
  const [input, setInput] = useState("");
  const [attachments, setAttachments] = useState<string[]>([]);
  const [activeModelId, setActiveModelId] = useState<string>(MODELS[0].id);
  const [modelMenuOpen, setModelMenuOpen] = useState(false);
  const [sending, setSending] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);
  const tailRef = useRef<string>("");

  const activeModel = useMemo(() => MODELS.find((m) => m.id === activeModelId) ?? MODELS[0], [activeModelId]);

  const { emit } = useCell(
    {
      name: "对话·折射流",
      category: "conversation",
      emits: ["cmd.chat.send", "cmd.files.pick", "cmd.folder.pick", "cmd.model.set"],
    },
    {
      "chat.token": (env) => {
        const payload = env.payload as { convId?: string; token: string };
        tailRef.current += payload.token;
        setTurns((prev) => {
          if (prev.length === 0) return prev;
          const last = prev[prev.length - 1];
          if (last.role === "assistant" && !last.sealed) {
            const next = [...prev];
            next[next.length - 1] = { ...last, text: tailRef.current };
            return next;
          }
          // 新 assistant 轮次
          tailRef.current = payload.token;
          return [
            ...prev,
            { turnId: "t-" + env.sid, role: "assistant", text: payload.token, sealed: false },
          ];
        });
      },
      "chat.turn.done": (env) => {
        const payload = env.payload as { text: string; role: "assistant" | "user" };
        tailRef.current = "";
        setTurns((prev) => {
          if (prev.length === 0) return prev;
          const next = [...prev];
          const last = next[next.length - 1];
          if (last.role === payload.role && !last.sealed) {
            next[next.length - 1] = { ...last, text: payload.text, sealed: true };
          } else {
            next.push({ turnId: "t-" + env.sid, role: payload.role, text: payload.text, sealed: true });
          }
          return next;
        });
        setSending(false);
      },
      "files.picked": (env) => {
        const paths = (env.payload as { paths: string[] }).paths ?? [];
        setAttachments((prev) => [...prev, ...paths]);
      },
      "folder.picked": (env) => {
        const p = (env.payload as { path?: string }).path;
        if (p) setAttachments((prev) => [...prev, `📁 ${p}`]);
      },
      "model.changed": (env) => {
        const id = (env.payload as { modelId: string }).modelId;
        setActiveModelId(id);
      },
    }
  );

  useEffect(() => {
    scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight, behavior: "smooth" });
  }, [turns]);

  function handleSend() {
    const text = input.trim();
    if (!text || sending) return;
    const userTurn: ChatTurn = {
      turnId: "t-u-" + Date.now(),
      role: "user",
      text,
      sealed: true,
      attachments: attachments.length ? [...attachments] : undefined,
    };
    setTurns((prev) => [...prev, userTurn]);
    setInput("");
    setAttachments([]);
    setSending(true);
    tailRef.current = "";
    // 占一个待填充的 assistant 位
    setTurns((prev) => [
      ...prev,
      { turnId: "t-a-streaming-" + Date.now(), role: "assistant", text: "", sealed: false },
    ]);
    emit(
      "cmd.chat.send",
      {
        convId: "main",
        message: text,
        history: turns
          .filter((t) => t.sealed)
          .map((t) => ({ role: t.role, content: t.text })),
        attachments: userTurn.attachments ?? [],
      },
      { dedupKey: text, prio: 9 }
    );
  }

  return (
    <PrismCell
      className="stretch"
      title="对话·折射流"
      subtitle="Conversation · Signal Driven"
      bodyClassName=""
      tools={
        <>
          <div className="model-pick">
            <button className="prism-btn ghost" onClick={() => setModelMenuOpen((v) => !v)}>
              <span
                style={{
                  width: 8,
                  height: 8,
                  borderRadius: 4,
                  background: activeModel.ready ? "var(--prism-success)" : "var(--prism-ink-3)",
                  boxShadow: activeModel.ready ? "0 0 6px var(--prism-success)" : undefined,
                }}
              />
              <span>{activeModel.name}</span>
              <span style={{ fontSize: 10, opacity: 0.7 }}>▼</span>
            </button>
            {modelMenuOpen ? (
              <div className="model-menu" onMouseLeave={() => setModelMenuOpen(false)}>
                {MODELS.map((m) => (
                  <div
                    key={m.id}
                    className={`mm-item ${m.id === activeModelId ? "active" : ""}`}
                    onClick={() => {
                      setActiveModelId(m.id);
                      emit("cmd.model.set", { modelId: m.id });
                      setModelMenuOpen(false);
                    }}
                  >
                    <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                      <span
                        style={{
                          width: 8,
                          height: 8,
                          borderRadius: 4,
                          background: m.ready ? "var(--prism-success)" : "var(--prism-ink-3)",
                        }}
                      />
                      <span>{m.name}</span>
                    </div>
                    <span className="mm-tag">{m.tier} · {m.vendor}</span>
                  </div>
                ))}
              </div>
            ) : null}
          </div>
        </>
      }
    >
      <div ref={scrollRef} className="scroll-area">
        {turns.map((t) => (
          <div key={t.turnId} className="bubble-row">
            <div className="meta">
              {t.role === "user" ? "你 · Sender" : "棱镜模型 · LLM"} • {t.attachments?.length ? `${t.attachments.length} 附件  · ` : ""}
              {t.sealed ? "已封包" : "流式输出中"}
            </div>
            <div className={`bubble ${t.role}`}>
              {t.text}
              {!t.sealed ? <span className="cursor" /> : null}
            </div>
            {t.attachments?.length ? (
              <div style={{ alignSelf: t.role === "user" ? "flex-end" : "flex-start", display: "flex", flexWrap: "wrap", gap: 6 }}>
                {t.attachments.map((a, i) => (
                  <span
                    key={i}
                    className="nexus-chip warn"
                    style={{ maxWidth: 260, textOverflow: "ellipsis", overflow: "hidden", whiteSpace: "nowrap" }}
                    title={a}
                  >
                    {a}
                  </span>
                ))}
              </div>
            ) : null}
          </div>
        ))}
      </div>

      <div className="composer" style={{ marginTop: 0, borderTop: "1px dashed var(--prism-cell-edge)" }}>
        {attachments.length ? (
          <div style={{ display: "flex", flexWrap: "wrap", gap: 6 }}>
            {attachments.map((a, i) => (
              <span key={i} className="nexus-chip warn">
                {a}
                <button
                  onClick={() => setAttachments((prev) => prev.filter((_, k) => k !== i))}
                  style={{ marginLeft: 6, color: "var(--prism-danger)" }}
                  aria-label="remove attachment"
                >
                  ✕
                </button>
              </span>
            ))}
          </div>
        ) : null}
        <div className="composer-row">
          <button className="prism-btn ghost" title="选择文件" onClick={() => emit("cmd.files.pick", {}, { dedupKey: "pick-files" })}>
            ＋ 文件
          </button>
          <button className="prism-btn ghost" title="选择文件夹" onClick={() => emit("cmd.folder.pick", {}, { dedupKey: "pick-folder" })}>
            📁 工作区
          </button>
          <textarea
            className="prism-textarea"
            value={input}
            placeholder="在棱镜舱内输入指令…  (Enter 发送, Shift+Enter 换行)"
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                handleSend();
              }
            }}
          />
          <button className="prism-btn" onClick={handleSend} disabled={sending || !input.trim()}>
            {sending ? "折射中…" : "发送 ◂"}
          </button>
        </div>
      </div>
    </PrismCell>
  );
}