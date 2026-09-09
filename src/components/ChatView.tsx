import { memo, useDeferredValue, useEffect, useMemo, useRef, useState } from "react";
import type { ClipboardEvent as ReactClipboardEvent } from "react";
import DOMPurify from "dompurify";
import { convertFileSrc } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { useChat } from "../stores/chat";
import { useVoice } from "../hooks/useVoice";
import { useVoiceConversation } from "../hooks/useVoiceConversation";
import VoiceOrb from "./VoiceOrb";
import ExecutionFlow from "./ExecutionFlow";
import ReplayView from "./ReplayView";
import { pickFiles, pickFolder, openPath, setWorkspace as setWorkspaceApi, detectImageModel, generateImage, getModelConfig, setActiveModel, onDocReady, saveUploadedImage, listQuickCommands, captureScreenForChat } from "../api";
import type { QuickCommand } from "../api";
import { KOKORO_VOICES } from "../api";
import { renderMarkdown } from "../utils/markdown";
import type { ChatMsg, ThoughtEvent, Todo, ImageCapability, ModelConfig } from "../types";

const SUGGESTIONS = [
  "查看 D 盘有哪些文件夹和文件",
  "写一篇周报总结",
  "搜索今天的新闻",
  "帮我整理下载文件夹",
];

/** 常见图片扩展名（用于上传预览与消息内图片渲染） */
const IMAGE_EXT = /\.(png|jpe?g|bmp|webp|gif)$/i;

/** 任务结束后的「阅读保持」兜底时长：鼠标一直没进入会话区时，超时自动收起 */
const READING_HOLD_MS = 15000;

/** 取路径的纯文件名（含扩展名） */
const basename = (p: string) => p.split(/[\\/]/).filter(Boolean).pop() ?? p;

/** 取扩展名大写标签（无扩展名回落 FILE） */
const fileExt = (p: string) => {
  const s = basename(p);
  const i = s.lastIndexOf(".");
  return i > 0 && i < s.length - 1 ? s.slice(i + 1).toUpperCase() : "FILE";
};

interface TraceData {
  thoughts: ThoughtEvent[];
  todos: Todo[];
}

/** 解析消息上持久化的执行流 JSON；失败返回 null */
function parseTrace(raw?: string): TraceData | null {
  if (!raw) return null;
  try {
    const o = JSON.parse(raw);
    return {
      thoughts: Array.isArray(o.thoughts) ? (o.thoughts as ThoughtEvent[]) : [],
      todos: Array.isArray(o.todos) ? (o.todos as Todo[]) : [],
    };
  } catch {
    return null;
  }
}

/** 提取消息里的本地图片路径（如 browser_act 截图） */
function extractImages(content: string): string[] {
  const re = /([A-Za-z]:[\\/][^\s"']+\.(?:png|jpg|jpeg))/g;
  const out: string[] = [];
  let m: RegExpExecArray | null;
  while ((m = re.exec(content)) !== null) {
    out.push(m[1]);
  }
  return out;
}

/** 消息内容的 Markdown 渲染（代码块语法高亮 + ==重点== 高亮 + chat_card 万能卡片） */
const Markdown = memo(function Markdown({ text }: { text: string }) {
  const parts = useMemo(
    () => (text.includes("```chat_card") ? splitChatCards(text) : null),
    [text]
  );
  if (!parts) {
    return <div className="msg-md" dangerouslySetInnerHTML={{ __html: renderMarkdown(text) }} />;
  }
  return (
    <div className="msg-md">
      {parts.map((p, i) =>
        p.type === "card" ? (
          <ChatCard key={i} card={p.card} />
        ) : p.text.trim() ? (
          <div key={i} dangerouslySetInnerHTML={{ __html: renderMarkdown(p.text) }} />
        ) : null
      )}
    </div>
  );
});

// ---------- chat_card 万能卡片 ----------

type CardSeg = { type: "text"; text: string } | { type: "card"; card: Record<string, unknown> };

/** 把回复按 ```chat_card 围栏块拆分为文本段与卡片段 */
function splitChatCards(text: string): CardSeg[] {
  const re = /```chat_card\s*\n([\s\S]*?)```/g;
  const parts: CardSeg[] = [];
  let last = 0;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text)) !== null) {
    if (m.index > last) parts.push({ type: "text", text: text.slice(last, m.index) });
    let card: Record<string, unknown> | null = null;
    try {
      card = JSON.parse(m[1]);
    } catch {
      /* 坏块当普通文本 */
    }
    if (card && typeof card.html === "string") {
      parts.push({ type: "card", card });
    } else {
      parts.push({ type: "text", text: m[0] });
    }
    last = m.index + m[0].length;
  }
  if (last < text.length) parts.push({ type: "text", text: text.slice(last) });
  return parts;
}

/** 万能卡片：模型推送的 HTML 片段经 DOMPurify 消毒后渲染，宽高可由模型自调 */
function ChatCard({ card }: { card: Record<string, unknown> }) {
  const html = useMemo(
    () => DOMPurify.sanitize(String(card.html ?? ""), { ADD_ATTR: ["target"] }),
    [card]
  );
  const bodyRef = useRef<HTMLDivElement>(null);

  // 本地文件图片路径 → asset 协议（模型直接写绝对路径也能显示）
  useEffect(() => {
    const el = bodyRef.current;
    if (!el) return;
    el.querySelectorAll("img").forEach((img) => {
      const src = img.getAttribute("src") ?? "";
      if (src && !/^(https?:|data:|asset:)/i.test(src)) {
        img.src = convertFileSrc(src);
      }
    });
  }, [html]);

  const style: React.CSSProperties = {
    width: typeof card.width === "string" && card.width ? card.width : "100%",
  };
  if (typeof card.height === "string" && card.height) style.height = card.height;

  return (
    <div className="chat-card" style={style}>
      {typeof card.title === "string" && card.title && (
        <div className="chat-card-title">{card.title}</div>
      )}
      <div className="chat-card-body" ref={bodyRef} dangerouslySetInnerHTML={{ __html: html }} />
    </div>
  );
}

/** 去掉高亮标记，供 TTS 朗读等纯文本场景使用 */
function stripHl(text: string) {
  return text.replace(/==([^=]+)==/g, "$1");
}

// ---------- 消息条目组件（memo 化：流式输出时历史消息不重复解析/渲染） ----------

const UserMessage = memo(function UserMessage({
  m,
  onEdit,
  secrete,
}: {
  m: ChatMsg;
  onEdit?: () => void;
  secrete?: boolean;
}) {
  const atts = m.attachments ?? [];
  const imgs = atts.filter((p) => IMAGE_EXT.test(p));
  const docs = atts.filter((p) => !IMAGE_EXT.test(p));
  const [hovered, setHovered] = useState(false);
  return (
    <div
      className={`msg user${secrete ? " secrete" : ""}`}
      style={{ position: "relative" }}
      onMouseEnter={() => setHovered(true)}
      onMouseLeave={() => setHovered(false)}
    >
      {m.content}
      {onEdit && hovered && (
        <button
          title="编辑并重发（该消息之后的内容将被替换）"
          onClick={onEdit}
          style={{
            position: "absolute",
            right: 0,
            top: -18,
            fontSize: 10,
            padding: "1px 8px",
            borderRadius: 8,
            border: "1px solid var(--border-soft)",
            background: "var(--bg-elevated)",
            color: "var(--text-dim)",
            cursor: "pointer",
          }}
        >
          ✎ 编辑重发
        </button>
      )}
      {imgs.map((p, j) => (
        <img
          key={j}
          src={convertFileSrc(p)}
          alt="附件图片"
          className="chat-img"
          style={{ maxWidth: "100%", borderRadius: 8, marginTop: 8, display: "block" }}
        />
      ))}
      {docs.length > 0 && (
        <div className="msg-files">
          {docs.map((p) => (
            <span
              key={p}
              className="msg-file-chip"
              title={`${p}（点击打开）`}
              onClick={() => void openPath(p).catch(() => {})}
            >
              <span className="msg-file-ext">{fileExt(p)}</span>
              <span className="msg-file-name">{basename(p)}</span>
            </span>
          ))}
        </div>
      )}
    </div>
  );
});

const BranchesMessage = memo(function BranchesMessage({ m }: { m: ChatMsg }) {
  const branches = m.branches ?? [];
  return (
    <div className="msg assistant branches-wrap">
      <div className="branches-head">⚖ 模型对比 · {branches.length} 个模型</div>
      <div className="branches">
        {branches.map((b, j) => (
          <div key={j} className={`branch${b.error ? " branch-error" : ""}`}>
            <div className="branch-title">
              <span className="branch-name">{b.name}</span>
              <span className="branch-model">{b.model}</span>
              <span className={`branch-tier ${b.tier}`}>
                {b.tier === "local" ? "本地" : "云端"}
              </span>
            </div>
            <div className="branch-body">
              {b.error ? (
                <span className="branch-err">{b.error}</span>
              ) : (
                <Markdown text={b.content ?? "（无输出）"} />
              )}
            </div>
          </div>
        ))}
      </div>
      <div className="ai-notice">内容由AI生成，请自行甄别</div>
    </div>
  );
});

const AssistantMessage = memo(function AssistantMessage({ m, onBranch }: { m: ChatMsg; onBranch?: () => void }) {
  const isError = m.content.startsWith("出错了");
  const trace = parseTrace(m.trace);
  const images = extractImages(m.content);
  // 执行回放：把该消息的思考流变成可播放的行动纪录片
  const [showReplay, setShowReplay] = useState(false);
  const [hovered, setHovered] = useState(false);
  return (
    <div
      className={`msg assistant${isError ? " error" : ""}`}
      style={{ position: "relative" }}
      onMouseEnter={() => setHovered(true)}
      onMouseLeave={() => setHovered(false)}
    >
      {onBranch && hovered && (
        <button
          title="从此处分支：以这条回复之前的内容为基础开一个新会话"
          onClick={onBranch}
          style={{
            position: "absolute",
            left: 0,
            top: -18,
            fontSize: 10,
            padding: "1px 8px",
            borderRadius: 8,
            border: "1px solid var(--border-soft)",
            background: "var(--bg-elevated)",
            color: "var(--text-dim)",
            cursor: "pointer",
          }}
        >
          ⑂ 从此分支
        </button>
      )}
      <Markdown text={m.content} />
      {images.map((p, j) => (
        <img
          key={j}
          src={convertFileSrc(p)}
          alt="截图"
          className="chat-img"
          style={{ maxWidth: "100%", borderRadius: 8, marginTop: 8, display: "block" }}
        />
      ))}
      {trace && (
        <>
          {/* 执行流：折叠态头部即单行流程摘要，回放入口并入同一行 */}
          <ExecutionFlow
            thoughts={trace.thoughts}
            todos={trace.todos}
            defaultOpen={false}
            done
            onReplay={trace.thoughts.length > 1 ? () => setShowReplay(true) : undefined}
          />
          {showReplay && (
            <ReplayView thoughts={trace.thoughts} onClose={() => setShowReplay(false)} />
          )}
        </>
      )}
      <div className="ai-notice">内容由AI生成，请自行甄别</div>
    </div>
  );
});

export default function ChatView() {
  const history = useChat((s) => s.history);
  const busy = useChat((s) => s.busy);
  const comparing = useChat((s) => s.comparing);
  const streaming = useChat((s) => s.streaming);
  const thoughts = useChat((s) => s.thoughts);
  // 流式内容延迟到渲染空档更新，避免每个 token 阻塞主线程（历史消息已 memo 化，不受影响）
  const deferredStreaming = useDeferredValue(streaming);
  const send = useChat((s) => s.send);
  const editResend = useChat((s) => s.editResend);
  const forkFrom = useChat((s) => s.forkFrom);
  const compare = useChat((s) => s.compare);
  const stop = useChat((s) => s.stop);
  // 编辑重发状态：记录正在编辑的用户消息索引（发送时替换该消息及其后内容）
  const [editingIdx, setEditingIdx] = useState<number | null>(null);
  // 快捷指令 "/" 提示：输入 / 开头时列出匹配指令
  const [quickCmds, setQuickCmds] = useState<QuickCommand[]>([]);
  useEffect(() => {
    listQuickCommands().then(setQuickCmds).catch(() => {});
  }, []);
  const [input, setInput] = useState("");
  const slashQuery = (() => {
    if (!input.startsWith("/") || input.includes("\n")) return null;
    const head = input.split(/\s/, 1)[0];
    return head.slice(1).toLowerCase();
  })();
  const slashMatches =
    slashQuery === null ? [] : quickCmds.filter((c) => c.name.toLowerCase().startsWith(slashQuery)).slice(0, 6);
  const [attachments, setAttachments] = useState<string[]>([]);
  const [workspace, setWorkspace] = useState<string | null>(null);
  const [modelCfg, setModelCfg] = useState<ModelConfig | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  // 文生图：能力检测 + 生成面板状态
  const [imgCap, setImgCap] = useState<ImageCapability | null>(null);
  const [imgOpen, setImgOpen] = useState(false);
  const [imgPrompt, setImgPrompt] = useState("");
  const [imgResult, setImgResult] = useState<string | null>(null);
  const [imgBusy, setImgBusy] = useState(false);
  const [imgHint, setImgHint] = useState("");
  const imgHintTimer = useRef<number | null>(null);

  useEffect(() => {
    void detectImageModel()
      .then(setImgCap)
      .catch((e) => setImgCap({ supported: false, model: "", tier: "", source: "none", hint: String(e) }));
  }, []);

  // 模型列表 + 当前激活模型（供输入框下拉切换，全局生效）
  // 监听「模型配置已保存」事件，添加/删除/切换模型后实时刷新下拉框
  useEffect(() => {
    let disposed = false;
    const refresh = () => {
      getModelConfig().then((c) => !disposed && setModelCfg(c)).catch(() => {});
    };
    refresh();
    window.addEventListener("model-config-changed", refresh);
    return () => {
      disposed = true;
      window.removeEventListener("model-config-changed", refresh);
    };
  }, []);

  const switchModel = async (id: string) => {
    const prev = modelCfg;
    setModelCfg((c) => (c ? { ...c, active: id } : c)); // 乐观更新
    try {
      setModelCfg(await setActiveModel(id));
    } catch {
      setModelCfg(prev); // 失败回滚
    }
  };

  const {
    sttSupported,
    ttsSupported,
    listening,
    audioLevel,
    transcript,
    ttsEnabled,
    setTtsEnabled,
    voices,
    voiceIndex,
    setVoiceIndex,
    ttsMode,
    setTtsMode,
    ttsCfg,
    setCloudVoice,
    startListening,
    stopListening,
    speak,
    resetTranscript,
  } = useVoice();

  // 连续语音对话模式：唤醒词「白泽」→ 说需求 → 打断插话（barge-in）。
  // 用语音下达指令时自动开启 TTS，形成「说 → 答 → 朗读」闭环
  const voiceConv = useVoiceConversation((t) => {
    setTtsEnabled(true);
    secretePendingRef.current = Date.now();
    void send(t);
  });
  // 问句交回用的引用（自动朗读 effect 依赖少，避免闭包过期）
  const voiceConvActiveRef = useRef(voiceConv.active);
  const wakeForAnswerRef = useRef(voiceConv.wakeForAnswer);
  useEffect(() => {
    voiceConvActiveRef.current = voiceConv.active;
    wakeForAnswerRef.current = voiceConv.wakeForAnswer;
  }, [voiceConv.active, voiceConv.wakeForAnswer]);
  const toggleVoiceConv = () => {
    if (voiceConv.active) {
      voiceConv.stop();
      localStorage.setItem("voice_conv_autostart", "0"); // 手动退出：下次启动不再自动待机
    } else {
      voiceConv.start();
      localStorage.setItem("voice_conv_autostart", "1");
    }
  };

  // 启动即待机聆听：等应用稳定后自动进入，免点击直接喊「白泽」。
  // 上次手动退出（localStorage=0）则尊重偏好不自动启动。
  useEffect(() => {
    if (!voiceConv.sttSupported) return;
    if (localStorage.getItem("voice_conv_autostart") === "0") return;
    const t = window.setTimeout(() => voiceConv.start(), 1500);
    return () => window.clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 多行输入框自动调整高度
  useEffect(() => {
    const el = textareaRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = Math.min(el.scrollHeight, 160) + "px";
  }, [input]);

  // 语音识别完成（listening 结束且有文字）→ 自动发送
  const wasListeningRef = useRef(false);
  useEffect(() => {
    if (wasListeningRef.current && !listening && transcript.trim() && !busy) {
      const text = transcript.trim();
      resetTranscript();
      secretePendingRef.current = Date.now();
      void send(text);
    }
    wasListeningRef.current = listening;
  }, [listening, transcript, busy, send, resetTranscript]);

  // TTS：新 assistant 消息自动朗读
  const lastSpokenRef = useRef("");
  // 文档出现即朗读：总结/报告写入文档窗口的瞬间就开始读（不等聊天回复生成落地），
  // 朗读过的这轮回复落定后不再重复朗读
  const suppressReplyReadRef = useRef(false);
  const ttsEnabledRef = useRef(ttsEnabled);
  const ttsSupportedRef = useRef(ttsSupported);
  const speakRef = useRef(speak);
  // 交回标记：当前朗读结束后是否把话筒交还用户（问句收尾/被抑制回复含问句时置位）
  const handoffRef = useRef(false);
  useEffect(() => {
    ttsEnabledRef.current = ttsEnabled;
    ttsSupportedRef.current = ttsSupported;
    speakRef.current = speak;
  }, [ttsEnabled, ttsSupported, speak]);
  /** 所有朗读共用的收尾：标记为问句时，朗读完自动唤醒聆听（连续语音对话模式） */
  const speakWithHandoff = (text: string) => {
    speakRef.current(text, () => {
      if (handoffRef.current && voiceConvActiveRef.current) {
        handoffRef.current = false;
        wakeForAnswerRef.current();
      }
    });
  };
  /** 问句判定：？/？结尾，或尾部含「是否/要不要/需要我/可以吗/行不行/好不好/吗」 */
  const looksLikeQuestion = (text: string) => {
    const tail = text.replace(/[。．.！!～~\s]+$/g, "").slice(-40);
    return (
      /[？?]\s*$/.test(tail) ||
      /(?:是否|要不要|需要我|需要吗|可以吗|行不行|好不好)/.test(tail) ||
      /吗\s*$/.test(tail)
    );
  };
  useEffect(() => {
    let off: (() => void) | undefined;
    void onDocReady(({ title, content }) => {
      if (!ttsEnabledRef.current || !ttsSupportedRef.current) return;
      // 保留换行/分段结构：stripHl 只去高亮标记，分块朗读器按行/段产生停顿
      const body = stripHl(content);
      if (!body.trim()) return;
      suppressReplyReadRef.current = true;
      const docTitle = title?.trim() ? `《${title.trim()}》。` : "";
      handoffRef.current = looksLikeQuestion(body);
      speakWithHandoff(docTitle + body);
    }).then((f) => {
      off = f;
    });
    return () => off?.();
  }, []);
  // 新的用户消息到来说明进入新一轮：文档朗读抑制解除，并武装本轮朗读
  const ttsRoundArmedRef = useRef(false);
  useEffect(() => {
    const last = history[history.length - 1];
    if (last?.role === "user") {
      suppressReplyReadRef.current = false;
      ttsRoundArmedRef.current = true;
    }
  }, [history]);
  // TTS 开启瞬间：以当前最后一条 assistant 消息为基线——恢复的历史 / 切换会话
  // 带来的旧回复一律不朗读，只读「开启之后用户新发出消息」得到的回复
  useEffect(() => {
    if (!ttsEnabled || !ttsSupported) return;
    const last = history[history.length - 1];
    if (last?.role === "assistant") {
      lastSpokenRef.current = last.content;
      ttsRoundArmedRef.current = false;
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ttsEnabled, ttsSupported]);
  useEffect(() => {
    const last = history[history.length - 1];
    if (
      ttsEnabled &&
      ttsSupported &&
      ttsRoundArmedRef.current &&
      last &&
      last.role === "assistant" &&
      last.content &&
      !last.content.startsWith("出错了") &&
      last.content !== lastSpokenRef.current
    ) {
      lastSpokenRef.current = last.content;
      ttsRoundArmedRef.current = false;
      // 文档出现时已经朗读过本轮内容：落定的回复只作展示，不再重复朗读。
      // 但若这条被抑制的回复以提问收尾，仍要把话筒交回（挂到进行中朗读的收尾上）
      if (suppressReplyReadRef.current) {
        suppressReplyReadRef.current = false;
        const monoSup = stripHl(last.content);
        if (monoSup && looksLikeQuestion(monoSup)) handoffRef.current = true;
        return;
      }
      // 只朗读独白：跳过代码块/表格/列表等内容结构，纯内容型回复整条静音
      const mono = stripHl(last.content); // 保留换行/分段结构，交给分块朗读器产生停顿
      if (mono) {
        // 连续语音对话：白泽以提问结尾（是否需要…？/…吗？）时，朗读完自动交回话筒——
        // 播放交回提示音并跳过唤醒词直接进入聆听，用户直接说即可
        handoffRef.current = looksLikeQuestion(mono);
        speakWithHandoff(mono);
      }
    }
  }, [history, ttsEnabled, ttsSupported, speak]);

  // 智能滚动：只有「用户主动向上滚」才解除贴底跟随；内容增长本身（执行流单步
  // 上百像素、独白流式 token）不产生 scroll 事件，不会误翻 stick —— 旧实现用
  // 「距底 <60px」的瞬时状态判定，展开动画中间帧 / 输入区高度突变时的 clamp
  // 都会算出假「离底」，从此钉底停用，新内容全部堆在输入框下面（看起来像被遮挡）。
  // rAF 二次滚动：大图/图标等异步资源加载后再补一次，避免次帧又冒出新高度。
  const stickRef = useRef(true);
  const [stick, setStick] = useState(true);
  const lastScrollTopRef = useRef(0);
  const onScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 60;
    const goingUp = el.scrollTop < lastScrollTopRef.current - 2;
    lastScrollTopRef.current = el.scrollTop;
    const s = nearBottom ? true : goingUp ? false : stickRef.current;
    stickRef.current = s;
    setStick(s);
  };
  const scrollToBottom = (force = false) => {
    const el = scrollRef.current;
    if (!el || (!force && !stickRef.current)) return;
    el.scrollTop = el.scrollHeight;
    requestAnimationFrame(() => {
      el.scrollTop = el.scrollHeight;
    });
  };

  // 内容高度观察：执行流单步展开 / 流式 markdown / 图标加载导致内容在两次提交之间
  // 静默长高时，scroll 事件不会触发，最后一行会卡在语音状态条与输入框下面。
  // 直接子元素是动态增删的（新消息、执行中的执行流块都是挂载后才追加），
  // 只观察首帧子元素会让「展开折叠执行流 / 话语到达」后的长高永远不被感知，
  // 最新话语会一直压在输入框下面 —— 所以用 MutationObserver 给新子元素补挂观察。
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => {
      if (stickRef.current) el.scrollTop = el.scrollHeight;
    });
    ro.observe(el); // 容器自身：会话区 0fr→1fr 展开动画期间逐帧长高，逐帧补钉
    const attach = () => {
      for (const child of Array.from(el.children)) ro.observe(child);
    };
    attach();
    const mo = new MutationObserver(attach);
    mo.observe(el, { childList: true });
    return () => {
      ro.disconnect();
      mo.disconnect();
    };
  }, []);

  // 悬浮卡片模式：会话区平时收起为输入框一条，悬浮/聚焦展开；
  // 正在生成或麦克风聆听时强制展开（连续语音待机不强制，保持背景可见）；
  // 白泽朗读回复期间也强制展开——正在读的内容必须可见，朗读结束（tts-state=false）后再收起；
  // 任务结束后不立刻收起——保留阅读期供查看结果，鼠标移出会话区即自动轻声收起
  const [chatOpen, setChatOpen] = useState(false);
  const [ttsSpeaking, setTtsSpeaking] = useState(false);
  useEffect(() => {
    const onTts = (e: Event) =>
      setTtsSpeaking(!!(e as CustomEvent<{ speaking: boolean }>).detail?.speaking);
    window.addEventListener("baize:tts-state", onTts);
    return () => window.removeEventListener("baize:tts-state", onTts);
  }, []);
  // 输入保持：正在输入（输入框聚焦或有未发送文字）时不允许收起——
  // 光标在输入框里/打字途中哪怕鼠标移出，会话区也保持展开
  const [inputFocused, setInputFocused] = useState(false);
  const inputActive = inputFocused || input.trim().length > 0;
  const forceOpen =
    busy || comparing || !!streaming || listening || ttsSpeaking || inputActive;
  // 新手引导期间强制展开（引导事件控制），结束后恢复悬浮卡片逻辑
  const [guideHold, setGuideHold] = useState(false);
  useEffect(() => {
    const onG = (e: Event) =>
      setGuideHold(!!(e as CustomEvent<{ active: boolean }>).detail?.active);
    const onFill = (e: Event) => {
      const t = (e as CustomEvent<{ text: string }>).detail?.text ?? "";
      setInput(t);
      setChatOpen(true);
      requestAnimationFrame(() => textareaRef.current?.focus());
    };
    window.addEventListener("baize:guide-expand", onG);
    window.addEventListener("baize:guide-fill", onFill);
    return () => {
      window.removeEventListener("baize:guide-expand", onG);
      window.removeEventListener("baize:guide-fill", onFill);
    };
  }, []);
  const [holdOpen, setHoldOpen] = useState(false); // 阅读保持期
  const chatExpanded = chatOpen || forceOpen || holdOpen || guideHold;
  const hoverRef = useRef(false);
  const forceOpenRef = useRef(forceOpen);
  const holdRef = useRef(false);
  const relaxTimerRef = useRef<number | null>(null);

  // 任务完成流光：执行/对比从「进行中」转「空闲」的下降沿触发，
  // 整个聊天框边框闪一圈流光，按「通知与音效」页配置的时长后淡出。
  // 时长/方向/样式存 localStorage（baize_glow_ms / baize_glow_dir / baize_glow_style），0 = 关闭流光
  const [glowPhase, setGlowPhase] = useState<"on" | "fade" | null>(null);
  const [glowCfg, setGlowCfg] = useState<{ dir: string; style: string }>({
    dir: "cw",
    style: "aurora",
  });
  const prevRunRef = useRef(false);
  useEffect(() => {
    const running = busy || comparing;
    const was = prevRunRef.current;
    prevRunRef.current = running;
    if (!was || running) return;
    const cfg = Number(localStorage.getItem("baize_glow_ms") ?? "4000");
    if (!Number.isFinite(cfg) || cfg <= 0) return;
    // 触发时读取方向/样式，设置页改完下一次任务完成即生效
    setGlowCfg({
      dir: localStorage.getItem("baize_glow_dir") || "cw",
      style: localStorage.getItem("baize_glow_style") || "aurora",
    });
    setGlowPhase("on");
    const fadeAt = Math.max(500, cfg - 700);
    const t1 = window.setTimeout(() => setGlowPhase("fade"), fadeAt);
    const t2 = window.setTimeout(() => setGlowPhase(null), fadeAt + 700);
    return () => {
      window.clearTimeout(t1);
      window.clearTimeout(t2);
    };
  }, [busy, comparing]);

  // 消息分泌出场：发送瞬间打时间戳标记，新的用户消息落进对话流时以「细胞分泌」
  // 姿态从输入框方向被挤出（囊泡拉长→脱离→回弹落位），只对刚发出的这条生效
  const [secreteIdx, setSecreteIdx] = useState<number | null>(null);
  const secretePendingRef = useRef(0);
  const secreteTimerRef = useRef<number | null>(null);
  useEffect(() => {
    const last = history.length - 1;
    const pending = secretePendingRef.current;
    if (!pending || last < 0 || history[last]?.role !== "user") return;
    secretePendingRef.current = 0;
    if (Date.now() - pending > 3000) return; // 标记过期（消息未落地），不播
    setSecreteIdx(last);
    if (secreteTimerRef.current) window.clearTimeout(secreteTimerRef.current);
    secreteTimerRef.current = window.setTimeout(() => setSecreteIdx(null), 800);
  }, [history]);

  useEffect(() => {
    forceOpenRef.current = forceOpen;
  }, [forceOpen]);

  /** 退出阅读保持，轻声收起 */
  const exitHold = () => {
    if (relaxTimerRef.current) {
      window.clearTimeout(relaxTimerRef.current);
      relaxTimerRef.current = null;
    }
    holdRef.current = false;
    setHoldOpen(false);
    setChatOpen(false);
  };

  // 任务开始 → 展开并取消待执行的收起；任务结束 → 进入阅读保持期，
  // 之后由「鼠标移出」触发收起；鼠标始终没进来时用兜底超时收起
  useEffect(() => {
    if (forceOpen) {
      if (relaxTimerRef.current) {
        window.clearTimeout(relaxTimerRef.current);
        relaxTimerRef.current = null;
      }
      holdRef.current = true;
      setHoldOpen(true);
      return;
    }
    // 启动时没有任务在跑，不进入保持期
    if (!holdRef.current) return;
    relaxTimerRef.current = window.setTimeout(() => {
      relaxTimerRef.current = null;
      if (!forceOpenRef.current && !hoverRef.current) exitHold();
    }, READING_HOLD_MS);
    return () => {
      if (relaxTimerRef.current) {
        window.clearTimeout(relaxTimerRef.current);
        relaxTimerRef.current = null;
      }
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [forceOpen]);

  // 展开瞬间贴底（收起→展开时消息直接可见最新内容）。
  // 展开是 0fr→1fr 的过渡动画（--dur-slow=0.5s），起点的钉底在动画结束后
  // 会差出一截，导致执行流最新话语被输入框挡住 —— 动画结束后再补钉一次。
  useEffect(() => {
    if (!chatExpanded) return;
    scrollToBottom(true);
    const t = window.setTimeout(() => scrollToBottom(true), 600);
    return () => window.clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [chatExpanded]);

  // 新消息 / 执行开始：强制到底（并重置贴底状态——发新消息即想看新回答）
  useEffect(() => {
    stickRef.current = true;
    setStick(true);
    scrollToBottom(true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [history.length, busy]);
  // 流式 token / 执行流步骤增长：贴底才跟随（不打断用户上翻回看）。
  // 依赖 deferredStreaming 而非 streaming：DOM 渲染的是 deferred 值，
  // 用原始 streaming 触发时量到的是旧高度，钉底必然差一截。
  useEffect(() => {
    scrollToBottom(false);
  }, [deferredStreaming, thoughts]);

  // 执行中「过渡思考」（独白）的显示切换瞬间：独白出现 = 执行流整体卸载、
  // 独白单独成条；独白结束（chat-round-reset）= 执行流带着全部轨迹整体重挂。
  // 这两个方向的高度跳变 + deferred 内容滞后，软钉底会追丢，独白会被
  // 输入框挡住 —— 切换瞬间强制钉底并补两帧，保证话语完整可见。
  const narrating = busy && !!streaming;
  useEffect(() => {
    scrollToBottom(true);
    const t1 = window.setTimeout(() => scrollToBottom(true), 80);
    const t2 = window.setTimeout(() => scrollToBottom(true), 250);
    return () => {
      window.clearTimeout(t1);
      window.clearTimeout(t2);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [narrating]);

  // 兜底校准：生成期间逐帧贴底（rAF）。事件驱动的钉底（token/step/RO）
  // 依赖「事件 → 渲染 → 观察」的时序链，任何一环错过（clamp 抖动、deferred
  // 渲染晚帧、300ms 定时档的窗口期）都会让新增长的内容在折叠线下被截断——
  // 用户实测流式期间最新一行被输入框上方一小块挡住即此窗口期。
  // rAF 每帧校准：内容增长的同一帧就贴底，浏览器合并同一帧内的滚动与绘制，
  // 不产生可见的截断帧；成本仅每帧一次 scrollTop 写入。校准尊重 stick：
  // 用户向上翻页即停止，滚回底部自动恢复；会话结束后恢复自由滚动。
  useEffect(() => {
    if (!busy) return;
    let raf = 0;
    const tick = () => {
      // 只在贴底跟随（stick）时逐帧校准：用户向上翻页（stick=false）期间完全
      // 停止钉底，滚轮/拖动自由查看历史；重新滚回底部附近（nearBottom）
      // onScroll 会把 stick 翻回 true，跟随自动恢复——生成不中断
      const el = scrollRef.current;
      if (el && stickRef.current) el.scrollTop = el.scrollHeight;
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [busy]);

  const onPickFiles = async () => {
    const files = await pickFiles();
    if (files && files.length > 0) {
      setAttachments((prev) => [...prev, ...files]);
    }
  };

  // 看屏幕：截屏 → 截图作为附件 + 预填分析指令（用户可直接发送或改问具体问题）
  const onLookScreen = async () => {
    if (busy) return;
    try {
      const shot = await captureScreenForChat();
      setAttachments((prev) => [...prev, shot.path]);
      setInput("👁 请看当前屏幕截图：描述屏幕上的内容（正在使用的应用、关键信息、明显的问题），并给出 1~3 条可执行的建议。");
    } catch (e) {
      console.error("截屏失败:", e);
    }
  };

  const onPickFolder = async () => {
    const dir = await pickFolder();
    if (dir) {
      setWorkspace(dir);
      void setWorkspaceApi(dir);
    }
  };

  // 粘贴图片进附件：把剪贴板里的图片转 data URL → 后端落盘 → 拿到绝对路径加入附件
  const onPasteImages = async (e: ReactClipboardEvent<HTMLTextAreaElement>) => {
    const items = e.clipboardData?.items;
    if (!items) return;
    const dataUrls: string[] = [];
    for (let i = 0; i < items.length; i++) {
      const item = items[i];
      if (item.kind === "file" && item.type.startsWith("image/")) {
        const file = item.getAsFile();
        if (file) {
          const dataUrl = await new Promise<string | null>((resolve) => {
            const r = new FileReader();
            r.onload = () => resolve(typeof r.result === "string" ? r.result : null);
            r.onerror = () => resolve(null);
            r.readAsDataURL(file);
          });
          if (dataUrl) dataUrls.push(dataUrl);
        }
      }
    }
    if (dataUrls.length === 0) return;
    e.preventDefault();
    for (const d of dataUrls) {
      const path = await saveUploadedImage(d).catch(() => null);
      if (path) setAttachments((prev) => [...prev, path]);
    }
  };

  // 拖拽文件到窗口 → 直接把绝对路径加入附件
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void getCurrentWebview()
      .onDragDropEvent((event) => {
        const p = event.payload;
        if (p.type === "drop" && p.paths.length > 0) {
          setAttachments((prev) => [...prev, ...p.paths]);
        }
      })
      .then((f) => (unlisten = f))
      .catch(() => {});
    return () => unlisten?.();
  }, []);

  const onSubmit = () => {
    const m = input.trim();
    if (!m && attachments.length === 0) return;
    secretePendingRef.current = Date.now();
    const atts = attachments;
    setInput("");
    setAttachments([]);
    // 编辑重发：替换被编辑消息及其后内容；否则正常发送
    if (editingIdx !== null) {
      setEditingIdx(null);
      void editResend(m);
      return;
    }
    void send(m, atts);
  };

  // 对话分支：同一问题并行对比所有模型（无附件要求）
  const onCompare = () => {
    const m = input.trim();
    if (!m) return;
    setInput("");
    setAttachments([]);
    void compare(m);
  };

  // 文生图：临时提示（自动消失）
  const showImgHint = (msg: string) => {
    setImgHint(msg);
    if (imgHintTimer.current) window.clearTimeout(imgHintTimer.current);
    imgHintTimer.current = window.setTimeout(() => setImgHint(""), 4500);
  };

  const onToggleImg = () => {
    if (!imgCap) {
      showImgHint("正在检测模型的文生图能力，请稍候…");
      return;
    }
    if (imgCap.supported) {
      setImgOpen((v) => !v);
    } else {
      showImgHint(imgCap.hint);
    }
  };

  const onGenerateImg = async () => {
    if (!imgPrompt.trim()) {
      showImgHint("请先输入图片描述");
      return;
    }
    setImgBusy(true);
    setImgResult(null);
    try {
      const result = await generateImage(imgPrompt);
      setImgResult(result);
    } catch (e) {
      setImgResult(null);
      showImgHint(String(e));
    } finally {
      setImgBusy(false);
    }
  };

  return (
    <div
      className={`chat ${chatExpanded ? "open" : "collapsed"}`}
      data-guide="chat"
      onMouseEnter={() => {
        hoverRef.current = true;
        setChatOpen(true);
      }}
      onMouseLeave={() => {
        hoverRef.current = false;
        setChatOpen(false);
        // 任务执行中仍强制展开；阅读保持期内鼠标移出 → 自动轻声收起
        if (!forceOpenRef.current && holdRef.current) exitHold();
      }}
    >
      {/* 任务完成流光边框（纯装饰层，不挡交互） */}
      {glowPhase && (
        <div
          className={`chat-glow ${glowPhase} g-${glowCfg.style} d-${glowCfg.dir}`}
          aria-hidden
        />
      )}
      {/* 未贴底时的「回到底部」悬浮按钮 */}
      {!stick && (
        <button
          className="chat-jump"
          title="回到底部"
          onClick={() => {
            stickRef.current = true;
            setStick(true);
            scrollToBottom(true);
          }}
        >
          ↓
        </button>
      )}
      {/* 会话主体：包一层 grid 容器做 0fr→1fr 的高度展开动画（柔和展开/收起） */}
      <div className="chat-body">
        <div className="chat-scroll" ref={scrollRef} onScroll={onScroll}>
        {history.length === 0 && (
          <div className="chat-welcome">
            {/* 白泽徽标：圆角徽章 + 心电脉冲（与侧栏生命体征卡同源意象），光点持续巡游 */}
            <div className="chat-welcome-mark" aria-hidden>
              <svg viewBox="0 0 64 64" width="64" height="64">
                <defs>
                  <linearGradient id="welcome-ring" x1="0" y1="0" x2="1" y2="1">
                    <stop offset="0%" stopColor="var(--accent)" />
                    <stop offset="100%" stopColor="var(--cyan)" />
                  </linearGradient>
                </defs>
                <rect x="6" y="6" width="52" height="52" rx="16" fill="var(--accent-soft)" stroke="url(#welcome-ring)" strokeWidth="1.5" />
                <path
                  className="welcome-pulse"
                  d="M14 32 h9 l4 -9 l5 18 l4 -12 l3 6 h11"
                  fill="none"
                  stroke="url(#welcome-ring)"
                  strokeWidth="2.4"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  pathLength={80}
                />
              </svg>
            </div>
            <div className="chat-welcome-title">问问白泽吧</div>
            <div className="chat-welcome-sub">
              本地优先的桌面助手
              <span className="chat-welcome-sep">·</span>读写文件
              <span className="chat-welcome-sep">·</span>操作界面
              <span className="chat-welcome-sep">·</span>搜索网页
              <span className="chat-welcome-sep">·</span>撰写文档
            </div>
          </div>
        )}

        {(() => {
          // 最后一条用户消息的索引：只有它可以「编辑重发」
          let lastUserIdx = -1;
          for (let k = history.length - 1; k >= 0; k--) {
            if (history[k].role === "user") {
              lastUserIdx = k;
              break;
            }
          }
          return history.map((m, i) => {
            if (m.role === "user")
              return (
                <UserMessage
                  key={i}
                  m={m}
                  secrete={i === secreteIdx}
                  onEdit={
                    !busy && i === lastUserIdx
                      ? () => {
                          setEditingIdx(i);
                          setInput(m.content);
                        }
                      : undefined
                  }
                />
              );
            if (m.branches && m.branches.length > 0) return <BranchesMessage key={i} m={m} />;
            return <AssistantMessage key={i} m={m} onBranch={!busy ? () => void forkFrom(i + 1) : undefined} />;
          });
        })()}

        {busy && streaming && (
          <div className="msg assistant streaming">
            <Markdown text={deferredStreaming} />
            <span className="caret">▍</span>
          </div>
        )}

        {busy && !streaming && <ExecutionFlow />}

        {comparing && (
          <div className="msg assistant comparing-hint">
            ⚖ 正在并行对比多个模型，请稍候…
          </div>
        )}

        {listening && (
          <div className="msg assistant voice-listening">
            <VoiceOrb size={68} audioLevel={audioLevel} state="listening" />
            <span className="voice-text">{transcript || "正在聆听，请说话…"}</span>
          </div>
        )}
        </div>
      </div>

      <div className="chat-input-area">
        {/* 连续语音对话状态已迁至右侧意识网络水球下方（mind-voice-hint），输入框上方不再重复显示 */}

        {chatExpanded && history.length === 0 && !busy && !listening && (
          <div className="chat-suggestions">
            {SUGGESTIONS.map((s) => (
              <button key={s} className="suggestion-chip" onClick={() => setInput(s)}>
                {s}
              </button>
            ))}
          </div>
        )}

        {/* 快捷指令 "/" 提示浮层 */}
        {slashMatches.length > 0 && (
          <div
            style={{
              position: "absolute",
              bottom: "100%",
              left: 12,
              right: 12,
              marginBottom: 6,
              background: "var(--bg-elevated, #1c1c24)",
              border: "1px solid var(--border-soft, #2e2e3a)",
              borderRadius: 10,
              padding: 6,
              boxShadow: "0 8px 24px rgba(0,0,0,0.35)",
              zIndex: 50,
            }}
          >
            {slashMatches.map((c) => (
              <div
                key={c.name}
                onClick={() => setInput(`/${c.name} `)}
                style={{
                  display: "flex",
                  gap: 8,
                  alignItems: "center",
                  padding: "5px 8px",
                  borderRadius: 6,
                  cursor: "pointer",
                  fontSize: 12,
                }}
                onMouseEnter={(e) => (e.currentTarget.style.background = "var(--border-soft, #2e2e3a)")}
                onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
              >
                <span style={{ color: "#f59e0b", fontFamily: "monospace" }}>/{c.name}</span>
                <span style={{ color: "var(--text-dim, #999)", flex: 1, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                  {c.description || c.template}
                </span>
              </div>
            ))}
          </div>
        )}

        {attachments.length > 0 && (
          <div className="attach-bar">
            {attachments.map((a) => (
              <span className="attach-chip" key={a} title={a}>
                {IMAGE_EXT.test(a) && (
                  <img className="attach-thumb" src={convertFileSrc(a)} alt="" />
                )}
                <span className="attach-chip-name">{basename(a)}</span>
                <button
                  onClick={() => setAttachments((prev) => prev.filter((x) => x !== a))}
                  title="移除"
                >
                  ×
                </button>
              </span>
            ))}
          </div>
        )}

        {imgHint && <div className="img-hint">{imgHint}</div>}

        {/* 编辑重发横幅：提示当前处于编辑状态 */}
        {editingIdx !== null && (
          <div
            style={{
              display: "flex",
              alignItems: "center",
              gap: 8,
              padding: "4px 10px",
              marginBottom: 6,
              borderRadius: 8,
              background: "var(--bg-elevated)",
              border: "1px dashed var(--border-soft)",
              fontSize: 11,
              color: "var(--text-dim)",
            }}
          >
            <span style={{ flex: 1 }}>✎ 正在编辑重发该消息，发送后将替换其后所有内容</span>
            <button
              onClick={() => {
                setEditingIdx(null);
                setInput("");
              }}
              style={{ cursor: "pointer", border: "none", background: "none", color: "var(--text-dim)", fontSize: 11 }}
            >
              取消
            </button>
          </div>
        )}

        {imgOpen && imgCap?.supported && (
          <div className="img-panel">
            <div className="img-panel-head">
              <span>文生图 · {imgCap.model}</span>
              <button className="img-panel-close" onClick={() => setImgOpen(false)} title="关闭">
                ×
              </button>
            </div>
            <div className="img-panel-body">
              <textarea
                className="img-prompt"
                placeholder="描述你想生成的图片…"
                value={imgPrompt}
                onChange={(e) => setImgPrompt(e.target.value)}
                rows={2}
              />
              <div className="img-panel-actions">
                <button className="img-generate-btn" onClick={onGenerateImg} disabled={imgBusy}>
                  {imgBusy ? "生成中…" : "生成图片"}
                </button>
              </div>
              {imgResult && <img className="img-result" src={imgResult} alt="生成结果" />}
            </div>
          </div>
        )}

        <div className="chat-input" data-guide="input">
          <textarea
            ref={textareaRef}
            value={input}
            onFocus={() => {
              setInputFocused(true);
              setChatOpen(true);
            }}
            onBlur={() => {
              setInputFocused(false);
              // 焦点离开输入框且鼠标也不在会话区：阅读保持期内直接收起
              if (!forceOpenRef.current && holdRef.current && !hoverRef.current) exitHold();
            }}
            onChange={(e) => setInput(e.target.value)}
            onPaste={onPasteImages}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
                e.preventDefault();
                onSubmit();
              }
            }}
            placeholder={listening ? "识别中…" : "输入消息…"}
            rows={1}
          />
          <div className="chat-input-footer">
            <div className="chat-input-tools">
              <select
                className="model-select"
                value={modelCfg?.active ?? ""}
                onChange={(e) => void switchModel(e.target.value)}
                title="切换当前使用的模型（全局生效）"
              >
                {(modelCfg?.profiles ?? []).filter((p) => p.enabled).map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.name}
                    {p.tier === "local" ? " · 本地" : ""}
                  </option>
                ))}
              </select>
              <button
                className={`tool-btn mic-btn ${listening ? "active" : ""}`}
                onClick={() => (listening ? stopListening() : startListening())}
                title={sttSupported ? "语音输入" : "当前环境不支持语音识别"}
                disabled={!sttSupported}
              >
                {listening ? "■" : "T"}
              </button>
              <button
                className={`tool-btn conv-btn ${voiceConv.active ? "active" : ""}`}
                onClick={toggleVoiceConv}
                title={sttSupported ? "连续语音对话（唤醒词「白泽」）" : "当前环境不支持语音识别"}
                disabled={!sttSupported}
              >
                ◉
              </button>
              <button className="tool-btn" onClick={onPickFiles} title="上传文件">
                ＋
              </button>
              <button
                className="tool-btn"
                onClick={onLookScreen}
                title="看屏幕：截取当前屏幕，让白泽分析并给建议"
                disabled={busy}
              >
                👁
              </button>
              <button
                className={`tool-btn img-btn ${!imgCap ? "" : imgCap.supported ? "active" : "unsupported"}`}
                onClick={onToggleImg}
                title={!imgCap ? "检测文生图能力…" : imgCap.supported ? `文生图（${imgCap.model} 支持）` : imgCap.hint}
              >
                <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden="true">
                  <rect x="1.5" y="2.5" width="13" height="11" rx="1.5" />
                  <circle cx="5" cy="6" r="1.3" fill="currentColor" stroke="none" />
                  <path d="M2.5 12.5 6 8.5l2.5 2.5 2-2 3 3.5" />
                </svg>
              </button>
              <button
                className={`tool-btn workspace-btn ${workspace ? "active" : ""}`}
                onClick={onPickFolder}
                title={workspace ? `工作空间：${workspace}（点击更换）` : "选择工作空间"}
              >
                {workspace ? (
                  <>
                    <span className="workspace-name">{basename(workspace)}</span>
                    <span
                      className="workspace-clear"
                      onClick={(e) => {
                        e.stopPropagation();
                        setWorkspace(null);
                        void setWorkspaceApi(null);
                      }}
                      title="清除工作空间"
                    >
                      ×
                    </span>
                  </>
                ) : (
                  "WP"
                )}
              </button>
              <button
                className="tool-btn compare-btn"
                onClick={onCompare}
                title="对话分支：同一问题并行对比所有模型"
                disabled={!input.trim() || busy || comparing}
              >
                ⚖
              </button>
              <button
                className={`tool-btn tts-btn ${ttsEnabled ? "active" : ""}`}
                onClick={() => setTtsEnabled(!ttsEnabled)}
                title={ttsSupported ? (ttsEnabled ? "关闭朗读" : "朗读回答") : "当前环境不支持语音合成"}
                disabled={!ttsSupported}
              >
                <svg width="16" height="16" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true">
                  <rect x="1.5" y="6" width="3" height="4" rx="1" />
                  <rect x="6.5" y="3" width="3" height="10" rx="1" />
                  <rect x="11.5" y="1" width="3" height="14" rx="1" />
                </svg>
              </button>
              {/* 朗读方式：跟随设置 / 本地 / 云端（快捷切换，覆盖设置页配置） */}
              {ttsSupported && (
                <select
                  className="voice-select"
                  value={ttsMode}
                  onChange={(e) => setTtsMode(e.target.value as "auto" | "local" | "cloud")}
                  title="朗读方式：跟随设置 / 本地系统语音 / 云端语音模型"
                >
                  <option value="auto">跟随设置</option>
                  <option value="local">本地语音</option>
                  <option value="cloud">云端语音</option>
                </select>
              )}
              {/* 本地音色：仅强制本地模式时显示 */}
              {ttsSupported && ttsMode === "local" && voices.length > 1 && (
                <select
                  className="voice-select"
                  value={voiceIndex}
                  onChange={(e) => setVoiceIndex(Number(e.target.value))}
                  title="选择本地系统音色"
                >
                  {voices.map((v, i) => (
                    <option key={i} value={i}>
                      {v.name.replace(/Microsoft |Online \(Natural\)/g, "").trim() || `音色 ${i + 1}`}
                    </option>
                  ))}
                </select>
              )}
              {/* 云端音色：仅强制云端模式时显示（豆包音色预设，选中即生效） */}
              {ttsSupported && ttsMode === "cloud" && (
                <select
                  className="voice-select"
                  value=""
                  onChange={(e) => void setCloudVoice(e.target.value)}
                  title={
                    ttsCfg?.provider === "doubao"
                      ? `当前豆包音色：${ttsCfg.db_speaker || "默认"}`
                      : `当前云端音色：${ttsCfg?.voice || "默认"}`
                  }
                >
                  <option value="">
                    {ttsCfg?.provider === "doubao"
                      ? `豆包·${ttsCfg.db_speaker || "当前配置"}`
                      : ttsCfg?.voice || "当前配置"}
                  </option>
                  {ttsCfg?.provider === "kokoro"
                    ? KOKORO_VOICES.map((v) => (
                        <option key={v.id} value={v.id}>
                          {v.name}
                        </option>
                      ))
                    : null}
                  <option value="zh_female_cancan_uranus_bigtts">灿灿（女声·活泼）</option>
                  <option value="zh_female_wanwanwan_uranus_bigtts">晚晚（女声·温柔）</option>
                  <option value="BV700_streaming">通用女声</option>
                </select>
              )}
            </div>
            <div className="chat-input-actions">
              {busy ? (
                <button className="send-btn stop" onClick={stop} title="停止">
                  <svg viewBox="0 0 24 24" width="13" height="13" aria-hidden="true">
                    <rect x="6.5" y="6.5" width="11" height="11" rx="2.5" fill="currentColor" />
                  </svg>
                </button>
              ) : (
                <button className="send-btn" onClick={onSubmit} title="发送">
                  <svg
                    viewBox="0 0 24 24"
                    width="17"
                    height="17"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="2.4"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    aria-hidden="true"
                  >
                    <path d="M12 19V5" />
                    <path d="M5.5 11.5 12 5l6.5 6.5" />
                  </svg>
                </button>
              )}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
