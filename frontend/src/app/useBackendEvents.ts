import { useEffect, useRef } from "react";
import {
  getFeishuStatus,
  getModelConfig,
  getPendingPermissions,
  getWechatStatus,
  onChatRoundReset,
  onChatToken,
  onEscalationCancelled,
  onEscalationLevel,
  onEscalationUpdate,
  onFeishuStatus,
  onPermissionChannel,
  onPermissionRequest,
  onThought,
  onTodoList,
  onTodoUpdate,
  onWechatStatus,
} from "../core/api";
import { useChat } from "../core/store/chat";
import { useHud } from "../core/store/hud";
import type { EscalationLevelEvent } from "../core/types";

/**
 * 后端事件装配舱：一次性订阅全部 Tauri 事件（会话流 / 审批 / 思考 / 通知升级 / IM 状态），
 * 组件树只从 store 读数据，不各自订阅。语音播报（L3）按 15s 循环直至用户响应。
 */
export function useBackendEvents() {
  const voiceRef = useRef<{
    active: boolean;
    timer: ReturnType<typeof setInterval> | null;
    audio: HTMLAudioElement | null;
  }>({ active: false, timer: null, audio: null });

  useEffect(() => {
    let disposed = false;
    const offs: Array<() => void> = [];

    const stopVoice = () => {
      const v = voiceRef.current;
      v.active = false;
      if (v.timer) {
        clearInterval(v.timer);
        v.timer = null;
      }
      if ("speechSynthesis" in window) speechSynthesis.cancel();
      if (v.audio) {
        v.audio.pause();
        v.audio = null;
      }
    };

    const playVoice = (e: EscalationLevelEvent) => {
      const v = voiceRef.current;
      if (!v.active) return;
      if (e.audio_file) {
        const audio = new Audio(e.audio_file);
        audio.volume = 0.8;
        audio.play().catch(() => {});
        v.audio = audio;
      }
      if (e.tts_text && "speechSynthesis" in window) {
        speechSynthesis.cancel();
        const u = new SpeechSynthesisUtterance(e.tts_text);
        u.lang = "zh-CN";
        speechSynthesis.speak(u);
      }
    };

    const setup = async () => {
      const chat = useChat.getState();
      const regs = await Promise.all([
        onPermissionRequest((r) => !disposed && chat.addPending(r)),
        onPermissionChannel((u) =>
          !disposed ? useHud.getState().markChannel(u.approval_id, u.channels) : undefined
        ),
        onThought((t) => !disposed && chat.addThought(t)),
        onTodoList((t) => !disposed && chat.setTodos(t)),
        onTodoUpdate((t) => !disposed && chat.setTodos(t)),
        onChatToken((t) => !disposed && chat.appendStream(t)),
        onChatRoundReset(() => !disposed && chat.resetStream()),
        onEscalationUpdate((e) => {
          if (disposed) return;
          useHud.getState().setEscalation({ level: e.level, label: e.level_label });
          // 低于 L2（回到弹窗层）或已到顶时停播
          if (e.max_level || e.level < 2) stopVoice();
        }),
        onEscalationLevel((e) => {
          if (disposed) return;
          useHud.getState().setEscalation({ level: e.level, label: e.level_label });
          if (e.action === "system_notify") {
            useHud.getState().notify("alert", `${e.title} · ${e.body}`);
            if ("Notification" in window) {
              if (Notification.permission === "granted") {
                new Notification(e.title, { body: e.body });
              } else if (Notification.permission !== "denied") {
                void Notification.requestPermission();
              }
            }
          }
          if (e.action === "voice") {
            stopVoice();
            voiceRef.current.active = true;
            playVoice(e);
            if (e.repeat) {
              voiceRef.current.timer = setInterval(() => playVoice(e), 15000);
            }
          }
        }),
        onEscalationCancelled(() => {
          if (disposed) return;
          stopVoice();
          useHud.getState().setEscalation(null);
        }),
        onWechatStatus((s) => !disposed && useHud.getState().setIm("wechat", s.status)),
        onFeishuStatus((s) => !disposed && useHud.getState().setIm("feishu", s.status)),
      ]);
      if (disposed) regs.forEach((f) => f());
      else offs.push(...regs);
    };
    void setup();

    getPendingPermissions()
      .then((list) => !disposed && list.forEach((r) => useChat.getState().addPending(r)))
      .catch(() => {});
    useChat.getState().loadConversations().catch(() => {});
    getModelConfig()
      .then((c) => !disposed && useHud.getState().setModels(c))
      .catch(() => {});
    getWechatStatus()
      .then((s) => !disposed && useHud.getState().setIm("wechat", s.status))
      .catch(() => {});
    getFeishuStatus()
      .then((s) => !disposed && useHud.getState().setIm("feishu", s.status))
      .catch(() => {});

    return () => {
      disposed = true;
      offs.forEach((f) => f());
      stopVoice();
    };
  }, []);
}