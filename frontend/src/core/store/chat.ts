import { create } from "zustand";
import {
  chat,
  compareModels,
  createConversation,
  deleteConversation,
  getMessages,
  listConversations,
  stopChat,
} from "../api";
import type {
  ChatMsg,
  Conversation,
  MessageRow,
  PermissionRequest,
  ThoughtEvent,
  Todo,
} from "../types";

let thoughtSeq = 0;

/** 把后端消息行映射为前端历史（含执行流 trace + 附件路径） */
const hydrate = (msgs: MessageRow[]): ChatMsg[] =>
  msgs.map((m) => ({
    role: m.role as "user" | "assistant",
    content: m.content,
    ...(m.trace != null ? { trace: m.trace } : {}),
    ...(m.attachments != null ? { attachments: parseAttachments(m.attachments) } : {}),
  }));

/** 解析后端存取的附件路径 JSON（失败返回空数组） */
function parseAttachments(raw: string): string[] {
  try {
    const v = JSON.parse(raw);
    return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

interface ChatState {
  history: ChatMsg[];
  busy: boolean;
  comparing: boolean;
  pending: PermissionRequest[];
  thoughts: ThoughtEvent[];
  todos: Todo[];
  streaming: string;
  conversations: Conversation[];
  currentConvId: string;
  send: (msg: string, attachments?: string[]) => Promise<void>;
  compare: (msg: string) => Promise<void>;
  stop: () => void;
  addPending: (req: PermissionRequest) => void;
  removePending: (id: string) => void;
  addThought: (t: ThoughtEvent) => void;
  setTodos: (todos: Todo[]) => void;
  appendStream: (token: string) => void;
  resetStream: () => void;
  loadConversations: () => Promise<void>;
  switchConversation: (id: string) => Promise<void>;
  newConversation: () => Promise<void>;
  removeConversation: (id: string) => Promise<void>;
}

export const useChat = create<ChatState>((set, get) => ({
  history: [],
  busy: false,
  comparing: false,
  pending: [],
  todos: [],
  streaming: "",
  conversations: [],
  currentConvId: "",
  // 觉醒自检种子（真实记忆 M3 接入）
  thoughts: [
    {
      id: "boot-1",
      ts: Date.now(),
      kind: "awaken",
      label: "白泽已觉醒",
      detail: "常驻循环启动 · 三级接地能力就绪",
    },
    {
      id: "boot-2",
      ts: Date.now(),
      kind: "awaken",
      label: "启动自检通过",
      detail: "文件读写 · 屏幕感知 · 记忆星图",
    },
  ],

  send: async (msg, attachments = []) => {
    // 若正在生成，先停止并等待结束（支持中途插话干预）
    if (get().busy) {
      void stopChat();
      await new Promise<void>((resolve) => {
        const timer = setInterval(() => {
          if (!get().busy) {
            clearInterval(timer);
            resolve();
          }
        }, 100);
      });
    }
    const history = get().history;
    const convId = get().currentConvId;
    set({
      busy: true,
      streaming: "",
      thoughts: [],
      todos: [],
      history: [
        ...history,
        { role: "user", content: msg, ...(attachments.length ? { attachments } : {}) },
      ],
    });
    try {
      const reply = await chat(convId, msg, history, attachments);
      // 回拉后端已固化的消息（含执行流 trace），保证任务结束后可回看
      try {
        const msgs = await getMessages(convId);
        // 回拉后清空本轮实时思考：trace 已固化到消息，思考流改为按 trace 聚合展示，避免重复
        set({ history: hydrate(msgs), streaming: "", thoughts: [] });
      } catch {
        set((s) => ({
          history: [...s.history, { role: "assistant", content: reply }],
          streaming: "",
        }));
      }
    } catch (e) {
      set((s) => ({
        history: [...s.history, { role: "assistant", content: `出错了：${String(e)}` }],
        streaming: "",
      }));
    } finally {
      set({ busy: false });
    }
  },

  // 「对话分支」：同一问题并行对比所有模型，结果作为一条带 branches 的 assistant 消息展示
  compare: async (msg) => {
    if (get().busy || get().comparing) {
      void stopChat();
      await new Promise<void>((resolve) => {
        const timer = setInterval(() => {
          if (!get().busy && !get().comparing) {
            clearInterval(timer);
            resolve();
          }
        }, 100);
      });
    }
    const history = get().history;
    set({
      comparing: true,
      streaming: "",
      thoughts: [],
      todos: [],
      history: [...history, { role: "user", content: msg }],
    });
    try {
      const branches = await compareModels(msg, history);
      set((s) => ({
        history: [...s.history, { role: "assistant", content: "", branches }],
        comparing: false,
      }));
    } catch (e) {
      set((s) => ({
        history: [...s.history, { role: "assistant", content: `出错了：${String(e)}` }],
        comparing: false,
      }));
    }
  },

  stop: () => {
    void stopChat();
  },

  addPending: (req) =>
    set((s) => (s.pending.some((p) => p.id === req.id) ? s : { pending: [...s.pending, req] })),

  removePending: (id) => set((s) => ({ pending: s.pending.filter((p) => p.id !== id) })),

  addThought: (t) =>
    set((s) => {
      const enriched = {
        ...t,
        id: t.id ?? `t-${thoughtSeq++}`,
        convId: t.convId ?? get().currentConvId,
      };
      // 安装进度合并为单条：复用尾部「进行中」的进度轨迹，只刷新进度条与文字，不追加新条目
      if (t.kind === "tool_progress") {
        for (let i = s.thoughts.length - 1; i >= 0; i--) {
          const p = s.thoughts[i];
          if (p.kind !== "tool_progress") break;
          if (p.phase !== "done" && p.phase !== "failed") {
            const next = s.thoughts.slice();
            next[i] = enriched;
            return { thoughts: next };
          }
        }
      }
      return { thoughts: [...s.thoughts.slice(-49), enriched] };
    }),

  setTodos: (todos) => set({ todos }),

  appendStream: (token) => set((s) => ({ streaming: s.streaming + token })),

  resetStream: () => set({ streaming: "" }),

  loadConversations: async () => {
    const list = await listConversations();
    set({ conversations: list });
    if (list.length === 0) {
      await get().newConversation();
    } else if (!get().currentConvId) {
      await get().switchConversation(list[0].id);
    }
  },

  switchConversation: async (id) => {
    set({ currentConvId: id, busy: false, comparing: false, streaming: "", thoughts: [], todos: [] });
    try {
      const msgs = await getMessages(id);
      set({ history: hydrate(msgs) });
    } catch {
      set({ history: [] });
    }
  },

  newConversation: async () => {
    const conv = await createConversation("新会话");
    set((s) => ({
      conversations: [conv, ...s.conversations],
      currentConvId: conv.id,
      history: [],
      streaming: "",
      comparing: false,
      thoughts: [],
      todos: [],
    }));
  },

  removeConversation: async (id) => {
    await deleteConversation(id);
    const list = get().conversations.filter((c) => c.id !== id);
    set({ conversations: list });
    if (get().currentConvId === id) {
      if (list.length > 0) {
        await get().switchConversation(list[0].id);
      } else {
        await get().newConversation();
      }
    }
  },
}));
