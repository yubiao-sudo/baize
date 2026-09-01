import { useCallback, useEffect, useRef, useState } from "react";
import { getMemoryGraph, onMemoryRecall } from "../api";
import { useChat } from "../stores/chat";
import { derive } from "./AiActivity";

// ============================================================
// 「意识网络 —— 蠕动记忆水球」（CSS 液态球渲染，与启动动画同款）
// 一颗大水球：border-radius 液态形变 + 内部流光 + 双层发光晕圈。
// 白泽检索/使用记忆（memory-recall）时，水球心跳式搏动（2 跳）、
// 晕圈发光爆发，并从球面向外扩散两圈涟漪，约 2.6 秒平滑回归常态。
// ============================================================

// 模块级标记：启动交接（splash 水球飞入）只需在应用首次挂载时等待一次；
// 之后面板切换导致的重挂载直接显示，避免水球白等 15 秒兜底而「消失」
let blobRevealed = false;

export default function ConsciousnessNetwork() {
  const orbRef = useRef<HTMLDivElement>(null);
  const pulseTimer = useRef(0);
  const voicePulseTimer = useRef(0);
  const [nodeCount, setNodeCount] = useState(0);

  // TTS 语音律动：白泽说话时水球「张嘴」——晕圈加速呼吸，每个词边界触发一次
  // 平滑脉冲缩放（speechSynthesis 无音频流，用逐词 onboundary 事件近似节奏）
  // 性能：缩放收敛后挂起 rAF 循环，由 pulse/speaking 事件再唤醒——空闲时零帧开销
  useEffect(() => {
    const el = orbRef.current;
    if (!el) return;
    let scale = 1;
    let target = 1;
    let raf = 0;
    let running = false;
    const step = () => {
      scale += (target - scale) * 0.18;
      if (Math.abs(target - scale) < 0.0005) {
        scale = target;
        el.style.setProperty("--voice-scale", scale.toFixed(4));
        running = false; // 已收敛：挂起，事件到来再唤醒
        return;
      }
      el.style.setProperty("--voice-scale", scale.toFixed(4));
      raf = requestAnimationFrame(step);
    };
    const wake = () => {
      if (!running) {
        running = true;
        raf = requestAnimationFrame(step);
      }
    };
    const onState = (e: Event) => {
      const speaking = (e as CustomEvent<{ speaking: boolean }>).detail.speaking;
      el.classList.toggle("speaking", speaking);
      if (!speaking) {
        target = 1;
        wake();
      }
    };
    const onPulse = (e: Event) => {
      const { energy } = (e as CustomEvent<{ energy: number }>).detail;
      target = 1 + 0.07 * energy;
      wake();
      // 短暂保持后回落，下一个词边界会再次抬升
      window.clearTimeout(voicePulseTimer.current);
      voicePulseTimer.current = window.setTimeout(() => {
        target = 1;
        wake();
      }, 180);
    };
    window.addEventListener("baize:tts-state", onState);
    window.addEventListener("baize:tts-pulse", onPulse);
    return () => {
      window.removeEventListener("baize:tts-state", onState);
      window.removeEventListener("baize:tts-pulse", onPulse);
      cancelAnimationFrame(raf);
      window.clearTimeout(voicePulseTimer.current);
      el.classList.remove("speaking");
      el.style.removeProperty("--voice-scale");
    };
  }, []);

  // 启动交接：主页水球初始透明，等启动动画的水球飞入 .mind-canvas 落位后淡入；
  // 与启动水球同一套 CSS（视觉 1:1），衔接处肉眼无感。15s 兜底强制显示。
  useEffect(() => {
    const el = orbRef.current;
    if (!el) return;
    if (blobRevealed) {
      el.style.opacity = "1";
      return;
    }
    el.style.opacity = "0";
    el.style.transition = "opacity 0.5s ease";
    const reveal = () => {
      blobRevealed = true;
      el.style.opacity = "1";
    };
    window.addEventListener("baize:blob-handoff", reveal, { once: true });
    const fallback = window.setTimeout(reveal, 15000);
    return () => {
      window.removeEventListener("baize:blob-handoff", reveal);
      window.clearTimeout(fallback);
    };
  }, []);

  // 连续语音对话形态：待唤醒=缓呼吸 + 青色晕圈（voice-standby），
  // 聆听指令/等待插话=快速脉动 + 晕圈扩张（voice-listening）
  useEffect(() => {
    const el = orbRef.current;
    if (!el) return;
    const onMode = (e: Event) => {
      const mode = (e as CustomEvent<{ mode: string }>).detail.mode;
      el.classList.toggle("voice-standby", mode === "standby");
      el.classList.toggle("voice-listening", mode === "listening");
    };
    window.addEventListener("baize:voice-mode", onMode);
    return () => {
      window.removeEventListener("baize:voice-mode", onMode);
      el.classList.remove("voice-standby", "voice-listening");
    };
  }, []);

  // 任务形变：白泽不同行为 → 水球不同形态
  // 思考中=深潜（慢速大幅蠕动、色偏靛紫）| 调用工具=干练（快速摆动、色偏青绿）
  // 生成中=涌动（高频微颤、色相流转加速）| 空闲=默认呼吸；说话/记忆召回为事件类单独控制
  const busy = useChat((s) => s.busy);
  const streaming = useChat((s) => s.streaming);
  const thoughts = useChat((s) => s.thoughts);
  const activity = derive(thoughts, busy, streaming);
  useEffect(() => {
    const el = orbRef.current;
    if (!el) return;
    const isTool = activity.tone === "tool";
    el.classList.toggle("thinking", busy && !streaming && !isTool);
    el.classList.toggle("working", busy && !streaming && isTool);
    el.classList.toggle("generating", !!streaming);
  }, [busy, streaming, activity.tone]);

  // 加载记忆数量用于展示
  const loadData = useCallback(() => {
    return getMemoryGraph()
      .then((g) => {
        setNodeCount(g.nodes.length);
      })
      .catch(() => {});
  }, []);

  // 数据加载 + 5 秒轮询
  useEffect(() => {
    loadData();
    const interval = setInterval(loadData, 5000);
    return () => clearInterval(interval);
  }, [loadData]);

  // 记忆召回 → 水球反馈：心跳搏动 + 发光爆发 + 涟漪扩散（CSS 类触发，约 2.6 秒回归常态）
  useEffect(() => {
    let disposed = false;
    let unRecall: (() => void) | null = null;
    onMemoryRecall((ids) => {
      if (disposed || ids.length === 0) return;
      const el = orbRef.current;
      if (el) {
        el.classList.remove("recalling");
        void el.offsetWidth; // 强制 reflow 以重播动画
        el.classList.add("recalling");
        window.clearTimeout(pulseTimer.current);
        pulseTimer.current = window.setTimeout(() => el.classList.remove("recalling"), 2600);
      }
      // 刷新计数（命中记忆 last_access 已更新）
      loadData();
    }).then((f) => {
      // 卸载早于注册完成则立即反注册，防止 listener 泄漏
      if (disposed) f();
      else unRecall = f;
    });
    return () => {
      disposed = true;
      unRecall?.();
      window.clearTimeout(pulseTimer.current);
    };
  }, [loadData]);

  return (
    <div className="panel-block mind">
      <div className="panel-head">
        意识网络 <span className="tag">记忆水球 · 最近 {nodeCount} 条</span>
      </div>
      <div className="mind-canvas">
        <div
          className="mind-orb clickable"
          ref={orbRef}
          onClick={() => window.dispatchEvent(new CustomEvent("baize:open-galaxy"))}
          title="点击展开记忆星图"
        >
          <div className="halo" />
          <div className="halo2" />
          <div className="orb-live">
            <div className="orb" />
          </div>
          <div className="ripple-ring" />
        </div>
        {nodeCount === 0 && (
          <div className="mind-empty">暂无记忆 · 对话后水球会随检索泛起涟漪</div>
        )}
      </div>
    </div>
  );
}