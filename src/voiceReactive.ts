/**
 * TTS 语音律动总线：把语音合成的说话状态与节奏事件广播为全局事件，
 * 供主页大水球（ConsciousnessNetwork）/ 桌面悬浮球等组件订阅，实现「白泽说话时水球跟着律动」。
 *
 * 双后端：
 *  - reactiveSpeak（本地）：speechSynthesis 不暴露音频流，无法做真实 FFT 频谱；
 *    改用 onboundary（逐词边界）事件近似节奏 —— 每念到一个词边界触发一次脉冲，
 *    能量随机模拟语调起伏，听感上与语音节奏同步。
 *  - speakWithCloud（云端语音模型）：后端合成 mp3 后用 <audio> 播放，
 *    经 AnalyserNode 做**真实频谱分析**，按音量能量广播脉冲 —— 标准的播放器级律动。
 */

import { emit } from "@tauri-apps/api/event";
import { convertFileSrc } from "@tauri-apps/api/core";
import { synthesizeTts } from "./api";

/** 说话状态变化（true=开始朗读 / false=结束或被打断）；同步经 Tauri 事件广播给悬浮球等子窗口 */
export function dispatchTtsState(speaking: boolean) {
  window.dispatchEvent(new CustomEvent("baize:tts-state", { detail: { speaking } }));
  void emit("baize:tts-state", { speaking }).catch(() => {});
}

/** 一次语音脉冲（energy 0~1，代表当前念到的词的起伏强度）；同步广播给子窗口 */
export function dispatchTtsPulse(energy: number) {
  window.dispatchEvent(new CustomEvent("baize:tts-pulse", { detail: { energy } }));
  void emit("baize:tts-pulse", { energy }).catch(() => {});
}

export interface SpeakOptions {
  lang?: string;
  voice?: SpeechSynthesisVoice | null;
  rate?: number;
  volume?: number;
  onend?: () => void;
}

/**
 * 独白提取：从回复中只留白泽的叙述性文字（独白），
 * 代码块、表格、列表、标题、引用、分隔线等内容性结构一律跳过不朗读。
 * 返回空串表示这条回复没有独白（纯内容型），整条静音。
 */
export function extractMonologue(text: string): string {
  const out: string[] = [];
  let inCode = false;
  for (const raw of text.split("\n")) {
    const line = raw.trimEnd();
    if (/^\s*```/.test(line)) {
      inCode = !inCode;
      continue;
    }
    if (inCode) continue;
    const t = line.trim();
    if (!t) continue;
    // 表格 / 列表 / 标题 / 引用 / 分隔线 → 结构化内容，不朗读
    if (/^(\|[^|]*\|[-*•+]\s|\d+[.、)]\s*|#+\s*|>\s*|[-*_=]{3,})/.test(t)) continue;
    out.push(t);
  }
  return out.join(" ").replace(/\s{2,}/g, " ").trim();
}

/**
 * 朗读文本清洗：去掉 markdown 语法、装饰符号、表情，只留可读文字。
 * 否则 TTS 会把「**加粗**」「✕」「×」「⚖」等念成「星号」「对勾」「叉号」。
 */
export function cleanForSpeech(text: string): string {
  return (
    text
      // 高亮标记 ==重点==
      .replace(/==([^=]+)==/g, "$1")
      // 代码块整体替换为占位词（代码内容念出来全是乱码）
      .replace(/```[\s\S]*?```/g, "，代码略，")
      // 行内代码保留内容、去反引号
      .replace(/`([^`]+)`/g, "$1")
      // 链接 [文字](url) → 只念文字；裸 URL → 「链接」
      .replace(/!?\[([^\]]*)\]\(([^)]*)\)/g, (_m, t: string, u: string) =>
        u.startsWith("http") ? t || "链接" : `${t || ""}${u}`,
      )
      .replace(/https?:\/\/\S+/g, "链接")
      // markdown 语法字符（星号/井号/下划线/竖线/引用符/波浪线）
      .replace(/[*_#>|~^]+/g, "")
      // 表格分隔行（---|---|---）
      .replace(/^\s*\|?[\s:|-]+\|?\s*$/gm, "")
      // 列表/标题行首的装饰符
      .replace(/^\s*[-+•·]\s+/gm, "")
      // Unicode 符号大扫除：箭头(2190-21FF) 数学符号(2200-22FF incl √×) 制表/几何(2580-25FF
      // incl ▍●◆■▲) 杂项符号与丁字花(2600-27BF incl ✓✔✕✘✗★⚖) 表情(1F000-1FAFF) 变体选择符
      .replace(/[\u2190-\u22FF\u2500-\u25FF\u2600-\u27BF\u{1F000}-\u{1FAFF}\uFE0F\u200D]/gu, "")
      // 兜底：C0/拉丁补充里的 ×(00D7) √ 用不到，但保留正常标点
      .replace(/[√]/g, "")
      // 行首残留的纯符号片段（清洗后只剩符号开头的行）
      .replace(/^[^\u4e00-\u9fa5A-Za-z0-9（(「"']+.{0,2}?[^\u4e00-\u9fa5A-Za-z0-9（(「"']]*$/gm, "")
      // 连续空行压缩
      .replace(/\n{3,}/g, "\n\n")
      .trim()
  );
}

/** 包装 speechSynthesis.speak：自动清洗文本 + 状态与边界事件广播 */
export function reactiveSpeak(text: string, opts?: SpeakOptions) {
  // 空文本/不支持时直接返回，不递增会话令牌——否则正在进行的云端朗读会因会话失效
  // 静默退出却无人派发 speaking=false，回声门永久关闭（唤醒词全盲的根因之一）
  if (!("speechSynthesis" in window) || !text) return;
  speakSession++; // 本地朗读接管：中止正在进行的云端分句管线，保证同一时刻只有一个声音
  try {
    stopCloudAudio();
    speechSynthesis.cancel();
    const u = new SpeechSynthesisUtterance(cleanForSpeech(text));
    if (!u.text) return;
    if (opts?.lang) u.lang = opts.lang;
    if (opts?.voice) u.voice = opts.voice;
    if (opts?.rate) u.rate = opts.rate;
    if (opts?.volume !== undefined) u.volume = opts.volume;
    u.onstart = () => dispatchTtsState(true);
    u.onboundary = () => dispatchTtsPulse(0.5 + Math.random() * 0.5);
    const done = () => {
      dispatchTtsState(false);
      opts?.onend?.();
    };
    u.onend = done;
    u.onerror = done;
    speechSynthesis.speak(u);
  } catch (e) {
    console.error("TTS 失败", e);
    dispatchTtsState(false);
  }
}

// ---------------- 云端语音模型播放（真实频谱律动 + 分句流水线） ----------------

let cloudAudio: HTMLAudioElement | null = null;

// ---- 朗读会话令牌：任何新的朗读/停止都会使旧会话失效，杜绝两个声源抢着朗读 ----
let speakSession = 0;
let sessionAudios: HTMLAudioElement[] = [];
let sessionCtx: AudioContext | null = null;

/** 关闭当前频谱 AudioContext（独立函数，规避 TS 对模块级 let 的控制流窄化） */
function closeSessionCtx() {
  const c = sessionCtx;
  sessionCtx = null;
  if (c) void c.close().catch(() => {});
}

/** 中止当前云端朗读会话（暂停所有已排队的音频并释放频谱资源） */
function abortCloudSession() {
  speakSession++;
  for (const a of sessionAudios) {
    a.onended = null;
    a.onerror = null;
    a.pause();
    a.src = "";
  }
  sessionAudios = [];
  closeSessionCtx();
  cloudAudio = null;
  dispatchTtsState(false);
}

/** 停掉云端音频播放并释放频谱分析资源 */
export function stopCloudAudio() {
  abortCloudSession();
}

/** 全局停止朗读：本地 + 云端一起停 */
export function stopSpeaking() {
  if ("speechSynthesis" in window) speechSynthesis.cancel();
  abortCloudSession();
}

/**
 * 朗读分块：保留换行/分段结构——空行=段间（长停顿），单换行=行间（短停顿），
 * 行内按句末标点切句（句间微停顿）。行首行尾空白与空行跳过。
 */
interface SpeechChunk {
  text: string;
  pause: number;
}

const PAUSE_SENT = 110; // 句间
const PAUSE_LINE = 220; // 行间（换行分段）
const PAUSE_PARA = 460; // 段间（空行分段）

/** 行内分句：按句末标点切分，短句合并（目标 ≤80 字） */
function splitLineSentences(line: string, maxLen = 80): string[] {
  const out: string[] = [];
  let buf = "";
  const matches = line.match(/[^。！？!?；;]*[。！？!?；;]+|[^。！？!?；;]+$/g) ?? [];
  for (const m of matches) {
    buf += m;
    if (buf.trim().length >= maxLen) {
      if (buf.trim()) out.push(buf.trim());
      buf = "";
    }
  }
  if (buf.trim()) out.push(buf.trim());
  return out;
}

function splitSpeechChunks(text: string): SpeechChunk[] {
  const out: SpeechChunk[] = [];
  let pendingBlank = 0;
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (!line) {
      pendingBlank++;
      continue;
    }
    const tailPause = pendingBlank > 0 ? PAUSE_PARA : PAUSE_LINE;
    pendingBlank = 0;
    for (const s of splitLineSentences(line)) {
      out.push({ text: s, pause: PAUSE_SENT });
    }
    // 该行最后一个分块的停顿升级为行间/段间停顿
    if (out.length) out[out.length - 1].pause = tailPause;
  }
  // 超长且无标点的分块硬切（120 字/块）
  const hard: SpeechChunk[] = [];
  for (const c of out) {
    if (c.text.length <= 160) {
      hard.push(c);
      continue;
    }
    for (let i = 0; i < c.text.length; i += 120) {
      hard.push({ text: c.text.slice(i, i + 120), pause: c.pause });
    }
  }
  return hard;
}

/** 播放一段音频（接入频谱律动）。返回 true=完整播完，false=失败或被中止 */
function playChunk(
  src: string,
  session: number
): Promise<boolean> {
  return new Promise<boolean>((resolve) => {
    let settled = false;
    let failsafe = 0;
    const finish = (ok: boolean) => {
      if (settled) return;
      settled = true;
      window.clearTimeout(failsafe);
      resolve(ok);
    };
    const audio = new Audio(src.startsWith("data:") ? src : convertFileSrc(src));
    sessionAudios.push(audio);
    cloudAudio = audio;
    // 真实频谱：MediaElementSource → AnalyserNode（AudioContext 被挂起时只播放、保声音不保频谱）
    try {
      if (!sessionCtx) {
        const ctx = new AudioContext();
        if (ctx.state === "suspended") void ctx.resume().catch(() => {});
        if (ctx.state === "running") sessionCtx = ctx;
        else void ctx.close().catch(() => {});
      }
      if (sessionCtx) {
        const source = sessionCtx.createMediaElementSource(audio);
        const analyser = sessionCtx.createAnalyser();
        analyser.fftSize = 256;
        source.connect(analyser);
        analyser.connect(sessionCtx.destination);
        const data = new Uint8Array(analyser.frequencyBinCount);
        let lastPulse = 0;
        const loop = () => {
          if (session !== speakSession || cloudAudio !== audio) return;
          analyser.getByteFrequencyData(data);
          let sum = 0;
          for (let k = 0; k < data.length; k++) sum += data[k];
          const energy = Math.min(1, (sum / data.length / 255) * 3.2);
          const now = performance.now();
          if (energy > 0.12 && now - lastPulse > 90) {
            dispatchTtsPulse(Math.max(0.45, energy));
            lastPulse = now;
          }
          requestAnimationFrame(loop);
        };
        requestAnimationFrame(loop);
      }
    } catch {
      // AudioContext 不可用时只播放，无频谱律动
    }
    audio.onended = () => finish(true);
    audio.onerror = () => finish(false);
    audio.play().catch(() => finish(false));
    // 兜底：onended/onerror 均不触发时（WebView2 偶发挂起），超时强制放行，
    // 避免整条朗读管线 await 挂死、speaking 状态永不复位、回声门永久关闭
    failsafe = window.setTimeout(() => finish(false), 120000);
  });
}

/**
 * 云端语音模型朗读（分句流水线）：首句合成完立即开口，后续句子在播放期间后台预合成，
 * 播完一句接一句，总等待从「整篇合成完」降到「首句合成完」。
 * 同一时刻只允许一个朗读会话（新请求接管并中止旧的）。全部句子合成失败时抛错（调用方回落本地语音）。
 */
export async function speakWithCloud(text: string, onend?: () => void): Promise<void> {
  const session = ++speakSession;
  const clean = cleanForSpeech(text);
  if (!clean) return;
  if ("speechSynthesis" in window) speechSynthesis.cancel();
  // 清理上一会话的音频资源（session 已递增，旧管线自行失效）
  for (const a of sessionAudios) {
    a.onended = null;
    a.onerror = null;
    a.pause();
    a.src = "";
  }
  sessionAudios = [];
  closeSessionCtx();
  cloudAudio = null;

  const chunks = splitSpeechChunks(clean);
  const pending: Promise<string | null>[] = [];
  // 记录首个真实合成错误（未配置/鉴权/网络…），全部失败时透传给调用方，不再报误导文案
  let firstSynthError: string | null = null;
  const synth = (i: number): Promise<string | null> =>
    synthesizeTts(chunks[i].text).catch((e) => {
      if (!firstSynthError) firstSynthError = String(e);
      return null;
    });
  // 预取：保持播放位置前方最多 2 句在合成中
  const ensure = (upto: number) => {
    while (pending.length < Math.min(chunks.length, upto + 2)) {
      pending.push(synth(pending.length));
    }
  };

  dispatchTtsState(true);
  let playedAny = false;

  for (let i = 0; i < chunks.length; i++) {
    if (session !== speakSession) return; // 被新朗读接管：静默退出
    ensure(i + 1);
    // 合成等待期心跳：低能量脉冲（<0.12 不会触发水球律动），仅向语音对话的
    // 卡死兜底证明「朗读管线还活着」，避免 Kokoro 冷启动等长等待被误判卡死强停
    dispatchTtsPulse(0.08);
    const src = await pending[i];
    if (session !== speakSession) return;
    if (!src) continue; // 该句合成失败，跳过读下一句
    const ok = await playChunk(src, session);
    if (session !== speakSession) return;
    if (ok) playedAny = true;
    // 句/行/段间停顿：让分行分段有呼吸感（被新朗读接管时立即退出）
    if (i < chunks.length - 1 && chunks[i].pause > 0) {
      await new Promise((r) => setTimeout(r, chunks[i].pause));
      if (session !== speakSession) return;
    }
  }

  // 正常结束（或全部句子失败）：释放资源并收尾
  sessionAudios = [];
  closeSessionCtx();
  cloudAudio = null;
  dispatchTtsState(false);
  onend?.();
  if (!playedAny) {
    throw new Error(
      firstSynthError
        ? `语音合成失败：${firstSynthError}`
        : "语音合成失败（音频播放不可用，请检查音频设备）"
    );
  }
}


