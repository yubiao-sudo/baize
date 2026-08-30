import { create } from "zustand";
import type { ModelConfig } from "../types";

export interface Notice {
  id: number;
  kind: "info" | "alert";
  text: string;
}

/** 前端 HUD 状态：面板开关 / 指令种子 / 模型与 IM 快照 / 升级链 / 通知栈 */
interface HudState {
  booted: boolean;
  telemetry: boolean;
  nebula: boolean;
  archive: boolean;
  settings: boolean;
  draft: string;
  models: ModelConfig | null;
  im: { wechat: string; feishu: string };
  escalation: { level: number; label: string } | null;
  channels: Record<string, string[]>;
  notices: Notice[];
  setBooted: () => void;
  toggleTelemetry: () => void;
  toggleNebula: () => void;
  toggleArchive: () => void;
  toggleSettings: () => void;
  setDraft: (v: string) => void;
  consumeDraft: () => void;
  setModels: (c: ModelConfig) => void;
  setIm: (k: "wechat" | "feishu", status: string) => void;
  setEscalation: (e: { level: number; label: string } | null) => void;
  markChannel: (approvalId: string, channels: string[]) => void;
  notify: (kind: "info" | "alert", text: string) => void;
  dismiss: (id: number) => void;
}

let noticeSeq = 0;

export const useHud = create<HudState>((set, get) => ({
  booted: false,
  telemetry: true,
  nebula: true,
  archive: false,
  settings: false,
  draft: "",
  models: null,
  im: { wechat: "idle", feishu: "idle" },
  escalation: null,
  channels: {},
  notices: [],

  setBooted: () => set({ booted: true }),
  toggleTelemetry: () => set((s) => ({ telemetry: !s.telemetry })),
  toggleNebula: () => set((s) => ({ nebula: !s.nebula })),
  toggleArchive: () => set((s) => ({ archive: !s.archive })),
  toggleSettings: () => set((s) => ({ settings: !s.settings })),
  setDraft: (v) => set({ draft: v }),
  consumeDraft: () => set({ draft: "" }),
  setModels: (c) => set({ models: c }),
  setIm: (k, status) => set((s) => ({ im: { ...s.im, [k]: status } })),
  setEscalation: (e) => set({ escalation: e }),
  markChannel: (approvalId, channels) =>
    set((s) => ({ channels: { ...s.channels, [approvalId]: channels } })),

  notify: (kind, text) => {
    if (!text) return;
    const id = ++noticeSeq;
    set((s) => ({ notices: [...s.notices.slice(-3), { id, kind, text }] }));
    setTimeout(() => get().dismiss(id), 5000);
  },
  dismiss: (id) => set((s) => ({ notices: s.notices.filter((n) => n.id !== id) })),
}));