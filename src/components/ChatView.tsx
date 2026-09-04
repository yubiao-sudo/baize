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
import { pickFiles, pickFolder, openPath, setWorkspace as setWorkspaceApi, detectImageModel, generateImage, getModelConfig, setActiveModel, onDocReady, saveUploadedImage } from "../api";
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

const UserMessage = memo(function UserMessage({ m }: { m: ChatMsg }) {
  const atts = m.attachments ?? [];
  const imgs = atts.filter((p) => IMAGE_EXT.test(p));
  const docs = atts.filter((p) => !IMAGE_EXT.test(p));
  return (
    <div className="msg user">
      {m.content}
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
      <div className="ai-notice">内容由AI生成</div>
    </div>
  );
});

const AssistantMessage = memo(function AssistantMessage({ m }: { m: ChatMsg }) {
  const isError = m.content.startsWith("出错了");
  const trace = parseTrace(m.trace);
  const images = extractImages(m.content);
  // 执行回放：把该消息的思考流变成可播放的行动纪录片
  const [showReplay, setShowReplay] = useState(false);
  return (
    <div className={`msg assistant${isError ? " error" : ""}`}>
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
      <div className="ai-notice">内容由AI生成</div>
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
  const compare = useChat((s) => s.compare);
  const stop = useChat((s) => s.stop);
  const [input, setInput] = useState("");
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
  // 新的用户消息到来说明进入新一轮：文档朗读抑制解除
  useEffect(() => {
    const last = history[history.length - 1];
    if (last?.role === "user") suppressReplyReadRef.current = false;
  }, [history]);
  useEffect(() => {
    const last = history[history.length - 1];
    if (
      ttsEnabled &&
      ttsSupported &&
      last &&
      last.role === "assistant" &&
      last.content &&
      !last.content.startsWith("出错了") &&
      last.content !== lastSpokenRef.current
    ) {
      lastSpokenRef.current = last.content;
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

  // 智能滚动：用「用户是否贴底」的瞬时状态决定跟随（scroll 事件实时维护），
  // 而不是内容长高后再补救——执行流单步增长上百像素时，事后判断永远追不上，
  // 新内容会一直堆在可视区外，看起来像被输入框挡住。
  // rAF 二次滚动：大图/图标等异步资源加载后再补一次，避免次帧又冒出新高度。
  const stickRef = useRef(true);
  const [stick, setStick] = useState(true);
  const onScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    const s = el.scrollHeight - el.scrollTop - el.clientHeight < 60;
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
  // 观察直接子元素尺寸变化，贴底状态下始终重新钉底。
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => {
      if (stickRef.current) el.scrollTop = el.scrollHeight;
    });
    for (const child of Array.from(el.children)) ro.observe(child);
    return () => ro.disconnect();
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
  const forceOpen = busy || comparing || !!streaming || listening || ttsSpeaking;
  const [holdOpen, setHoldOpen] = useState(false); // 阅读保持期
  const chatExpanded = chatOpen || forceOpen || holdOpen;
  const hoverRef = useRef(false);
  const forceOpenRef = useRef(forceOpen);
  const holdRef = useRef(false);
  const relaxTimerRef = useRef<number | null>(null);

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

  // 展开瞬间贴底（收起→展开时消息直接可见最新内容）
  useEffect(() => {
    if (chatExpanded) scrollToBottom(true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [chatExpanded]);

  // 新消息 / 执行开始：强制到底
  useEffect(() => {
    scrollToBottom(true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [history.length, busy]);
  // 流式 token / 执行流步骤增长：贴底才跟随（不打断用户上翻回看）
  useEffect(() => {
    scrollToBottom(false);
  }, [streaming, thoughts]);

  const onPickFiles = async () => {
    const files = await pickFiles();
    if (files && files.length > 0) {
      setAttachments((prev) => [...prev, ...files]);
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
    const atts = attachments;
    setInput("");
    setAttachments([]);
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
            <div className="chat-welcome-icon">🧭</div>
            <div className="chat-welcome-title">问问白泽吧</div>
            <div className="chat-welcome-sub">一个本地优先的桌面助手，能读写文件、操作界面、搜索网页、撰写文档</div>
          </div>
        )}

        {history.map((m, i) => {
          if (m.role === "user") return <UserMessage key={i} m={m} />;
          if (m.branches && m.branches.length > 0) return <BranchesMessage key={i} m={m} />;
          return <AssistantMessage key={i} m={m} />;
        })}

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

        {voiceConv.active && !listening && (
          <div className="msg assistant voice-listening voice-conv-hint">
            <span className={`voice-conv-dot ${voiceConv.mode}`} />
            <span className="voice-text">
              {voiceConv.error
                ? `语音对话不可用：${voiceConv.error}`
                : voiceConv.mode === "listening"
                  ? `聆听中：${voiceConv.heard || "请说出你的需求…"}`
                  : voiceConv.heard
                    ? `待唤醒 · 刚听到「${voiceConv.heard}」（说「白泽」唤醒，说话时喊「白泽」可打断我）`
                    : "待唤醒 · 说「白泽」开始对话（说话时喊「白泽」可打断我）"}
            </span>
            <button className="voice-conv-close" onClick={voiceConv.stop} title="退出连续对话">
              ×
            </button>
          </div>
        )}
        </div>
      </div>

      <div className="chat-input-area">
        {/* 收起态的语音对话状态（连续语音常驻时保持可见，不依赖展开） */}
        {!chatExpanded && voiceConv.active && (
          <div className="voice-conv-mini" onClick={() => setChatOpen(true)}>
            <span className={`voice-conv-dot ${voiceConv.mode}`} />
            <span>
              {voiceConv.error
                ? `语音对话不可用：${voiceConv.error}`
                : voiceConv.mode === "listening"
                  ? "聆听中…"
                  : "语音待唤醒 · 说「白泽」"}
            </span>
            <button
              className="voice-conv-close"
              onClick={(e) => {
                e.stopPropagation();
                voiceConv.stop();
              }}
              title="退出连续对话"
            >
              ×
            </button>
          </div>
        )}

        {chatExpanded && history.length === 0 && !busy && !listening && (
          <div className="chat-suggestions">
            {SUGGESTIONS.map((s) => (
              <button key={s} className="suggestion-chip" onClick={() => setInput(s)}>
                {s}
              </button>
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

        <div className="chat-input">
          <textarea
            ref={textareaRef}
            value={input}
            onFocus={() => setChatOpen(true)}
            onBlur={() => {
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
                  ■
                </button>
              ) : (
                <button className="send-btn" onClick={onSubmit} title="发送">
                  ↑
                </button>
              )}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
