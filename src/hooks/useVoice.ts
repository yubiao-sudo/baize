import { useCallback, useEffect, useRef, useState } from "react";
import { getTtsConfig, getVoice, setVoice, setTtsConfig, TtsConfig } from "../api";
import { reactiveSpeak, speakWithCloud } from "../voiceReactive";

/** 朗读方式：auto=跟随设置页配置 | local=强制本地系统语音 | cloud=强制云端语音模型 */
export type TtsMode = "auto" | "local" | "cloud";

/**
 * 语音能力 hook：基于 Web Speech / Web Audio API。
 *  - STT：SpeechRecognition（说话转文字）
 *  - TTS：SpeechSynthesis（文字朗读，可切换音色）
 *  - 音量：getUserMedia + AnalyserNode（驱动语音球）
 */
export interface UseVoice {
  sttSupported: boolean;
  ttsSupported: boolean;
  listening: boolean;
  /** 麦克风实时音量 0-1 */
  audioLevel: number;
  /** 语音识别文字（最终 + 中间结果） */
  transcript: string;
  ttsEnabled: boolean;
  setTtsEnabled: (v: boolean) => void;
  /** 可用音色列表 */
  voices: SpeechSynthesisVoice[];
  /** 当前选中音色下标 */
  voiceIndex: number;
  setVoiceIndex: (i: number) => void;
  /** 朗读方式（跟随设置 / 本地 / 云端） */
  ttsMode: TtsMode;
  setTtsMode: (m: TtsMode) => void;
  /** 语音模型后端配置（云端音色切换时读写） */
  ttsCfg: TtsConfig | null;
  /** 云端音色快捷切换（写入后端配置） */
  setCloudVoice: (id: string) => Promise<void>;
  startListening: () => void;
  stopListening: () => void;
  /** 朗读文字（TTS） */
  speak: (text: string, onend?: () => void) => void;
  resetTranscript: () => void;
}

export function useVoice(): UseVoice {
  const [sttSupported] = useState(() => {
    const SR = (window as unknown as Record<string, unknown>).SpeechRecognition
      || (window as unknown as Record<string, unknown>).webkitSpeechRecognition;
    return !!SR;
  });
  const [ttsSupported] = useState(() => "speechSynthesis" in window);
  const [listening, setListening] = useState(false);
  const [audioLevel, setAudioLevel] = useState(0);
  const [transcript, setTranscript] = useState("");
  const [ttsEnabled, setTtsEnabled] = useState(false);
  const [voices, setVoices] = useState<SpeechSynthesisVoice[]>([]);
  const [voiceIndex, setVoiceIndex] = useState(0);
  // 朗读方式覆盖：auto 跟随设置；local/cloud 强制指定（输入框快捷切换）
  const [ttsMode, setTtsModeState] = useState<TtsMode>(() => {
    const m = localStorage.getItem("baize_tts_mode");
    return m === "local" || m === "cloud" ? m : "auto";
  });
  const [ttsCfg, setTtsCfg] = useState<TtsConfig | null>(null);
  const setTtsMode = useCallback((m: TtsMode) => {
    setTtsModeState(m);
    localStorage.setItem("baize_tts_mode", m);
  }, []);
  // 语音模型后端：local=浏览器内置 | cloud=OpenAI 兼容 | doubao=豆包
  const cloudRef = useRef(false);

  const recognitionRef = useRef<{ stop: () => void } | null>(null);
  const streamRef = useRef<MediaStream | null>(null);
  const audioCtxRef = useRef<AudioContext | null>(null);
  const rafRef = useRef(0);

  // 加载可用音色（异步，需监听 onvoiceschanged）
  useEffect(() => {
    if (!ttsSupported) return;
    const load = () => {
      const all = speechSynthesis.getVoices();
      const zh = all.filter((v) => v.lang.toLowerCase().includes("zh"));
      setVoices(zh.length > 0 ? zh : all);
    };
    load();
    speechSynthesis.onvoiceschanged = load;
    return () => {
      speechSynthesis.onvoiceschanged = null;
    };
  }, [ttsSupported]);

  // 恢复持久化的音色
  useEffect(() => {
    if (!ttsSupported || voices.length === 0) return;
    getVoice().then((saved) => {
      if (!saved) return;
      const idx = voices.findIndex((v) => v.name === saved);
      if (idx >= 0) setVoiceIndex(idx);
    });
  }, [ttsSupported, voices]);

  // 保存音色偏好
  useEffect(() => {
    if (voices[voiceIndex]) {
      void setVoice(voices[voiceIndex].name);
    }
  }, [voiceIndex, voices]);

  // 设置页切换音色时同步（baize:voice-changed → 重新读取持久化音色名 → 定位下标）
  useEffect(() => {
    if (!ttsSupported || voices.length === 0) return;
    const sync = () => {
      void getVoice().then((saved) => {
        if (!saved) return;
        const idx = voices.findIndex((v) => v.name === saved);
        if (idx >= 0) setVoiceIndex(idx);
      });
    };
    window.addEventListener("baize:voice-changed", sync);
    return () => window.removeEventListener("baize:voice-changed", sync);
  }, [ttsSupported, voices]);

  const stopAudioAnalysis = useCallback(() => {
    if (rafRef.current) cancelAnimationFrame(rafRef.current);
    rafRef.current = 0;
    if (audioCtxRef.current) {
      audioCtxRef.current.close().catch(() => {});
      audioCtxRef.current = null;
    }
    if (streamRef.current) {
      streamRef.current.getTracks().forEach((t) => t.stop());
      streamRef.current = null;
    }
    setAudioLevel(0);
  }, []);

  const startListening = useCallback(async () => {
    if (!sttSupported) return;
    try {
      // 麦克风音量分析
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      streamRef.current = stream;
      const ctx = new AudioContext();
      audioCtxRef.current = ctx;
      const source = ctx.createMediaStreamSource(stream);
      const analyser = ctx.createAnalyser();
      analyser.fftSize = 512;
      source.connect(analyser);
      const data = new Uint8Array(analyser.frequencyBinCount);
      const loop = () => {
        analyser.getByteFrequencyData(data);
        let sum = 0;
        for (let i = 0; i < data.length; i++) sum += data[i];
        const avg = sum / data.length / 255;
        setAudioLevel(Math.min(1, avg * 3));
        rafRef.current = requestAnimationFrame(loop);
      };
      loop();

      // 语音识别
      const SR = (window as unknown as Record<string, unknown>).SpeechRecognition
        || (window as unknown as Record<string, unknown>).webkitSpeechRecognition;
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const recognition = new (SR as any)();
      recognition.lang = "zh-CN";
      recognition.continuous = false;
      recognition.interimResults = true;
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      recognition.onresult = (e: any) => {
        let interim = "";
        let final = "";
        for (let i = 0; i < e.results.length; i++) {
          const r = e.results[i];
          if (r.isFinal) final += r[0].transcript;
          else interim += r[0].transcript;
        }
        setTranscript((final || interim).trim());
      };
      recognition.onend = () => {
        setListening(false);
        stopAudioAnalysis();
      };
      recognition.onerror = () => {
        setListening(false);
        stopAudioAnalysis();
      };
      recognitionRef.current = recognition;
      recognition.start();
      setTranscript("");
      setListening(true);
    } catch (e) {
      console.error("麦克风访问失败", e);
      stopAudioAnalysis();
    }
  }, [sttSupported, stopAudioAnalysis]);

  const stopListening = useCallback(() => {
    if (recognitionRef.current) {
      recognitionRef.current.stop();
      recognitionRef.current = null;
    }
    setListening(false);
    stopAudioAnalysis();
  }, [stopAudioAnalysis]);

  // 加载语音模型后端配置（云端 / 本地）
  useEffect(() => {
    const loadCfg = () => {
      void getTtsConfig()
        .then((c) => {
          setTtsCfg(c);
          cloudRef.current =
            c?.provider === "cloud" ||
            c?.provider === "doubao" ||
            c?.provider === "kokoro";
        })
        .catch(() => {});
    };
    loadCfg();
    // 设置页切换语音模型后同步
    window.addEventListener("baize:voice-changed", loadCfg);
    return () => window.removeEventListener("baize:voice-changed", loadCfg);
  }, []);

  /** 云端音色快捷切换：写入后端配置（豆包改 speaker，OpenAI 兼容改 voice） */
  const setCloudVoice = useCallback(
    async (id: string) => {
      if (!ttsCfg || !id) return;
      const next: TtsConfig =
        ttsCfg.provider === "doubao" ? { ...ttsCfg, db_speaker: id } : { ...ttsCfg, voice: id };
      try {
        await setTtsConfig(next);
        setTtsCfg(next);
        window.dispatchEvent(new CustomEvent("baize:voice-changed"));
      } catch {
        // 保存失败保持原配置
      }
    },
    [ttsCfg],
  );

  const speak = useCallback(
    (text: string, onend?: () => void) => {
      if (!text) return;
      // 决定本次朗读后端：输入框强制模式优先，否则跟随设置页配置
      const useCloud =
        ttsMode === "cloud" || (ttsMode === "auto" && cloudRef.current);
      // 云端语音模型：后端合成 + 真实频谱律动；失败回落本地语音
      if (useCloud) {
        void speakWithCloud(text, onend).catch(() => {
          if ("speechSynthesis" in window) {
            reactiveSpeak(text, {
              lang: "zh-CN",
              rate: 1.0,
              voice: voices[voiceIndex] ?? undefined,
              onend,
            });
          } else {
            onend?.();
          }
        });
        return;
      }
      if (!ttsSupported) {
        onend?.();
        return;
      }
      const voice = voices[voiceIndex];
      reactiveSpeak(text, { lang: "zh-CN", rate: 1.0, voice: voice ?? undefined, onend });
    },
    [ttsMode, ttsSupported, voices, voiceIndex],
  );

  const resetTranscript = useCallback(() => setTranscript(""), []);

  useEffect(() => {
    return () => {
      if (recognitionRef.current) recognitionRef.current.stop();
      stopAudioAnalysis();
    };
  }, [stopAudioAnalysis]);

  return {
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
  };
}
