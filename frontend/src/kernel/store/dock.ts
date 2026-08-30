import { create } from "zustand";
import type { ModelProfile } from "../types";

/**
 * Dock Workbench 状态机：
 * · boot: 启动自检序列是否完成
 * · archiveOpen / settingsOpen: 弹出面板
 * · modelMenuAnchor: 指令台模型下拉的 DOM 锚点（null=关闭）
 * · activeModels: 当前模型档案列表（来自 ModelConfig.profiles）
 * · currentModelId: 当前激活 profile.id（来自 ModelConfig.active）
 */
export interface NoticeItem {
  id: string;
  title?: string;
  body: string;
  tier: "info" | "alert";
  createdAt: number;
  onClick?: () => void;
}

interface DockState {
  boot: boolean;
  completeBoot: () => void;
  archiveOpen: boolean;
  toggleArchive: (v?: boolean) => void;
  settingsOpen: boolean;
  toggleSettings: (v?: boolean) => void;
  modelMenuAnchor: HTMLElement | null;
  setModelMenuAnchor: (el: HTMLElement | null) => void;
  compareMode: boolean;
  setCompareMode: (v: boolean) => void;
  activeModels: ModelProfile[];
  setActiveModels: (m: ModelProfile[]) => void;
  currentModelId: string;
  setCurrentModelId: (id: string) => void;
  notices: NoticeItem[];
  pushNotice: (n: Omit<NoticeItem, "id" | "createdAt"> & { id?: string; createdAt?: number }) => void;
  removeNotice: (id: string) => void;
}

export const useDock = create<DockState>((set, get) => ({
  boot: false,
  completeBoot: () => set({ boot: true }),
  archiveOpen: false,
  toggleArchive: (v) => set({ archiveOpen: typeof v === "boolean" ? v : !get().archiveOpen }),
  settingsOpen: false,
  toggleSettings: (v) => set({ settingsOpen: typeof v === "boolean" ? v : !get().settingsOpen }),
  modelMenuAnchor: null,
  setModelMenuAnchor: (el) => set({ modelMenuAnchor: el }),
  compareMode: false,
  setCompareMode: (v) => set({ compareMode: v }),
  activeModels: [],
  setActiveModels: (m) => set({ activeModels: m }),
  currentModelId: "",
  setCurrentModelId: (id) => set({ currentModelId: id }),
  notices: [],
  pushNotice: (n) => {
    const full: NoticeItem = {
      id: n.id ?? `notice-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`,
      createdAt: n.createdAt ?? Date.now(),
      tier: n.tier ?? "info",
      body: n.body,
      title: n.title,
      onClick: n.onClick,
    };
    set((s) => ({ notices: [...s.notices, full] }));
    setTimeout(() => {
      set((s) => ({ notices: s.notices.filter((x) => x.id !== full.id) }));
    }, 6000);
  },
  removeNotice: (id) => set((s) => ({ notices: s.notices.filter((x) => x.id !== id) })),
}));