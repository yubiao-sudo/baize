import { useCallback, useEffect, useRef, useState } from "react";
import { stopSpeaking } from "../voiceReactive";
import { playSfx } from "../utils/sound";
import { emit } from "@tauri-apps/api/event";

/**
 * 连续语音对话模式：唤醒词「白泽」→ 说需求 → 打断插话（barge-in）。
 *
 * 状态机：
 *  - standby  常驻聆听，等唤醒词（「白泽」及常见近音误识别）
 *  - listening 命中唤醒词后捕获指令，静音超时自动回落 standby
 *  - barge-in  白泽正在说话（TTS）时喊「白泽」立即闭嘴并进入 listening —— 打断插话
 *
 * 实现要点：continuous=true 常驻识别为主（一次授权长期出结果，比单轮重启更稳），
 * 另加看门狗兜底：WebView2 偶发识别进程静默失联（不再吐结果也不触发 onend），
 * 超过静默阈值没有任何事件就强制 stop→重启，保证常驻聆听不悄悄死掉。
 *
 * 水球律动：模式变化广播 baize:voice-mode 事件，主页大水球订阅后切换待机呼吸/聆听脉动形态。
 */
export type VoiceConvMode = "off" | "standby" | "listening";

/** 唤醒词（含常见近音误识别：白泽 → 白色/百泽/拜泽/柏泽/白则…） */
const WAKE_RE = /白\s*泽|百\s*泽|白\s*色|拜\s*泽|柏\s*泽|白\s*则|baize/i;
/** listening 态下静音超时（无最终结果则回落待机） */
const LISTEN_TIMEOUT_MS = 12000;
/** 看门狗：持续无任何识别事件超过该阈值视为静默失联，强制重启识别进程 */
const SILENCE_RESTART_MS = 10000;
/** 看门狗巡检间隔 */
const WATCHDOG_TICK_MS = 4000;

function dispatchVoiceMode(mode: VoiceConvMode) {
  window.dispatchEvent(new CustomEvent("baize:voice-mode", { detail: { mode } }));
  void emit("baize:voice-mode", { mode }).catch(() => {});
}

export interface UseVoiceConversation {
  active: boolean;
  mode: VoiceConvMode;
  /** 当前聆听到的文字（standby 下显示环境语音，listening 下显示指令） */
  heard: string;
  /** 致命错误（麦克风被占用 / 无权限 / 识别服务不可用），非空时对话模式已自动退出 */
  error: string;
  sttSupported: boolean;
  start: () => void;
  stop: () => void;
  toggle: () => void;
  /** 问句交回：白泽以提问结尾时自动唤醒聆听（免唤醒词，带交回提示音） */
  wakeForAnswer: () => void;
}

export function useVoiceConversation(onCommand: (text: string) => void): UseVoiceConversation {
  const [active, setActive] = useState(false);
  const [mode, setMode] = useState<VoiceConvMode>("off");
  const [heard, setHeard] = useState("");
  const [error, setError] = useState("");
  const [sttSupported] = useState(() => {
    const SR = (window as unknown as Record<string, unknown>).SpeechRecognition
      || (window as unknown as Record<string, unknown>).webkitSpeechRecognition;
    return !!SR;
  });

  const activeRef = useRef(false);
  const modeRef = useRef<VoiceConvMode>("off");
  const recRef = useRef<{ stop: () => void } | null>(null);
  const cmdRef = useRef(onCommand);
  const listenTimer = useRef(0);
  const restartTimer = useRef(0);
  /** 回声门：白泽正在朗读（TTS 播放中）时为 true。
   *  识别器是常驻的，白泽念到自己名字（「我是白泽」）会被麦克风拾回并命中唤醒词，
   *  造成「自己唤醒自己、自己打断自己」——播放期间的所有识别结果一律丢弃。 */
  const ttsSpeakingRef = useRef(false);

  useEffect(() => {
    cmdRef.current = onCommand;
  }, [onCommand]);

  useEffect(() => {
    const onTts = (e: Event) => {
      const speaking = !!(e as CustomEvent<{ speaking?: boolean }>).detail?.speaking;
      ttsSpeakingRef.current = speaking;
      // 朗读结束瞬间，识别器里可能还残留刚被拾进去的尾音（含「白泽」），
      // 稍等片刻再放行，避免最后一句回声在门开的瞬间漏过去
      if (!speaking) {
        const until = echoCooldownUntil.current;
        if (until) window.clearTimeout(until);
        echoCooldownUntil.current = window.setTimeout(() => {
          echoGate.current = false;
        }, 800);
        echoGate.current = true;
      } else {
        echoGate.current = false;
        if (echoCooldownUntil.current) window.clearTimeout(echoCooldownUntil.current);
      }
    };
    window.addEventListener("baize:tts-state", onTts);
    return () => window.removeEventListener("baize:tts-state", onTts);
  }, []);
  /** 朗读结束后 800ms 的回声冷却窗（放行前的最后一道闸） */
  const echoGate = useRef(false);
  const echoCooldownUntil = useRef(0);

  const setModeSafe = useCallback((m: VoiceConvMode) => {
    modeRef.current = m;
    setMode(m);
    dispatchVoiceMode(m);
  }, []);

  const sendCommand = useCallback((text: string) => {
    setHeard("");
    setModeSafe("standby");
    cmdRef.current(text);
  }, [setModeSafe]);

  const handleChunk = useCallback((chunk: string, isFinal: boolean) => {
    const m = modeRef.current;
    if (m === "off") return;
    // 回声门：白泽朗读期间（及结束后 800ms 冷却窗内）丢弃所有识别结果。
    // 无法区分「用户喊白泽」与「白泽念到自己名字」的麦克风回声，
    // 若放行会自己唤醒/打断自己，甚至把朗读内容后半句误当指令发送。
    // 打断朗读请用悬浮球/停止按钮；门开后再喊「白泽」即可正常唤醒。
    if (ttsSpeakingRef.current || echoGate.current) return;
    const hit = WAKE_RE.exec(chunk);
    if (hit) {
      // 唤醒 / 打断插话：白泽正在朗读就立即闭嘴
      stopSpeaking();
      const rest = chunk
        .slice(hit.index + hit[0].length)
        .replace(/^[，,。.\s、?？!！]*/, "")
        .trim();
      if (rest.length >= 2 && (isFinal || rest.length >= 4)) {
        sendCommand(rest);
      } else {
        setHeard("");
        setModeSafe("listening");
        playSfx("voice-wake"); // 唤醒提示音：确认「我在听」，也盖住 TTS 戛然而止的突兀感
        window.clearTimeout(listenTimer.current);
        listenTimer.current = window.setTimeout(() => {
          if (modeRef.current === "listening") setModeSafe("standby");
        }, LISTEN_TIMEOUT_MS);
      }
      return;
    }
    if (m === "listening") {
      setHeard(chunk);
      if (isFinal) {
        window.clearTimeout(listenTimer.current);
        if (chunk.trim().length >= 2) sendCommand(chunk.trim());
        else setModeSafe("standby");
      }
    } else if (m === "standby" && isFinal) {
      // 待机态把「刚听到的」透出，方便确认识别链路活着、唤醒词被听成了什么
      setHeard(chunk);
    }
  }, [sendCommand, setModeSafe]);

  const startRec = useCallback(() => {
    const SR = (window as unknown as Record<string, unknown>).SpeechRecognition
      || (window as unknown as Record<string, unknown>).webkitSpeechRecognition;
    if (!SR) return;
    // 最近一次识别事件时间戳（onresult / onend 都会刷新），供看门狗判断静默失联
    let lastEventAt = Date.now();
    let watchdog = 0;

    const spawn = () => {
      if (!activeRef.current) return;
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const rec = new (SR as any)();
      rec.lang = "zh-CN";
      rec.continuous = true; // 常驻识别：一次授权长期出结果
      rec.interimResults = true;
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      rec.onresult = (e: any) => {
        lastEventAt = Date.now();
        let interim = "";
        let finalTxt = "";
        for (let i = e.resultIndex; i < e.results.length; i++) {
          const r = e.results[i];
          if (r.isFinal) finalTxt += r[0].transcript;
          else interim += r[0].transcript;
        }
        const chunk = (finalTxt || interim).trim();
        if (chunk) handleChunk(chunk, !!finalTxt && !interim);
      };
      // 识别进程结束（正常/异常）→ 快速重启，保持常驻聆听
      rec.onend = () => {
        lastEventAt = Date.now();
        if (!activeRef.current) return;
        window.clearTimeout(restartTimer.current);
        restartTimer.current = window.setTimeout(spawn, 300);
      };
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      rec.onerror = (e: any) => {
        lastEventAt = Date.now();
        const err = String(e?.error ?? "");
        // 致命错误：重启也无意义，直接退出对话模式并提示
        if (err === "not-allowed" || err === "service-not-allowed") {
          setError("麦克风权限被拒绝，或语音识别服务不可用（WebView2 需联网）");
          activeRef.current = false;
          setActive(false);
          setModeSafe("off");
        } else if (err === "audio-capture") {
          setError("找不到可用麦克风");
          activeRef.current = false;
          setActive(false);
          setModeSafe("off");
        }
        /* no-speech / network / aborted 等交由 onend 重启 */
      };
      try {
        rec.start();
        recRef.current = rec;
      } catch {
        /* start 抛错（上一轮还没停干净）→ 稍后重启兜底 */
        window.clearTimeout(restartTimer.current);
        restartTimer.current = window.setTimeout(spawn, 500);
        return;
      }
      // 看门狗：识别进程静默失联（无事件也不触发 onend）时强制 stop→重启。
      // onend 若正常触发会由 onend 分支重启，这里兜住「彻底没事件」的情况。
      const armWatchdog = () => {
        window.clearTimeout(watchdog);
        watchdog = window.setTimeout(() => {
          if (!activeRef.current) return;
          if (Date.now() - lastEventAt > SILENCE_RESTART_MS) {
            try {
              rec.stop();
            } catch {
              /* 已停止 */
            }
            // onend 万一不来，直接再 spawn 一个新识别进程兜底
            window.clearTimeout(restartTimer.current);
            restartTimer.current = window.setTimeout(spawn, 500);
          } else {
            armWatchdog();
          }
        }, WATCHDOG_TICK_MS);
      };
      armWatchdog();
    };
    spawn();
  }, [handleChunk, setModeSafe]);

  const start = useCallback(() => {
    if (!sttSupported || activeRef.current) return;
    activeRef.current = true;
    setActive(true);
    setError("");
    setHeard("");
    setModeSafe("standby");
    startRec();
  }, [sttSupported, setModeSafe, startRec]);

  const stop = useCallback(() => {
    activeRef.current = false;
    setActive(false);
    window.clearTimeout(listenTimer.current);
    window.clearTimeout(restartTimer.current);
    recRef.current?.stop();
    recRef.current = null;
    setHeard("");
    setError("");
    setModeSafe("off");
  }, [setModeSafe]);

  const toggle = useCallback(() => {
    if (activeRef.current) stop();
    else start();
  }, [start, stop]);

  /** 问句交回：白泽回答以提问结尾时，跳过唤醒词直接进入聆听态（伴随交回提示音）。
      仅在连续语音对话模式激活时生效；超时未说话自动回落待机。 */
  const wakeForAnswer = useCallback(() => {
    if (!activeRef.current) return;
    playSfx("voice-handoff");
    setHeard("");
    setModeSafe("listening");
    window.clearTimeout(listenTimer.current);
    listenTimer.current = window.setTimeout(() => {
      if (modeRef.current === "listening") setModeSafe("standby");
    }, LISTEN_TIMEOUT_MS);
  }, [setModeSafe]);

  useEffect(() => {
    return () => {
      activeRef.current = false;
      window.clearTimeout(listenTimer.current);
      window.clearTimeout(restartTimer.current);
      recRef.current?.stop();
      dispatchVoiceMode("off");
    };
  }, []);

  return { active, mode, heard, error, sttSupported, start, stop, toggle, wakeForAnswer };
}
