import { create } from "zustand";
import {
  chat,
  compareModels,
  createConversation,
  deleteConversation,
  addProject,
  deleteProject,
  getMessages,
  listConversations,
  listProjects,
  saveCompareResult,
  setConversationProject,
  setWorkspace,
  stopChat,
} from "../api";
import type {
  ChatMsg,
  Conversation,
  MessageRow,
  PermissionRequest,
  Project,
  ThoughtEvent,
  Todo,
} from "../types";
import { playSfx } from "../utils/sound";

let thoughtSeq = 0;

/** 把后端消息行映射为前端历史（含执行流 trace + 附件路径）；
    trace.branches 还原「多模型对比」分支消息（对比结果已落库，重启后仍可回看） */
const hydrate = (msgs: MessageRow[]): ChatMsg[] =>
  msgs.map((m) => {
    if (m.trace) {
      try {
        const obj = JSON.parse(m.trace) as { branches?: unknown };
        if (Array.isArray(obj.branches)) {
          return {
            role: m.role as "user" | "assistant",
            content: m.content,
            branches: obj.branches as ChatMsg["branches"],
          };
        }
      } catch {
        // trace 不是 JSON 或无 branches：按普通消息处理
      }
    }
    return {
      role: m.role as "user" | "assistant",
      content: m.content,
      ...(m.trace != null ? { trace: m.trace } : {}),
      ...(m.attachments != null ? { attachments: parseAttachments(m.attachments) } : {}),
    };
  });

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
  projects: Project[];
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
  newConversation: (projectId?: string | null) => Promise<void>;
  removeConversation: (id: string) => Promise<void>;
  loadProjects: () => Promise<void>;
  addProject: (name: string, path: string) => Promise<void>;
  removeProject: (id: string) => Promise<void>;
  moveConversation: (convId: string, projectId: string | null) => Promise<void>;
  followProjectWorkspace: (convId: string) => void;
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
  projects: [],
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
    // 若正在生成，先停止并等待结束（支持中途插话干预；10s 超时兜底防后端卡死轮询永挂）
    if (get().busy) {
      void stopChat();
      await new Promise<void>((resolve) => {
        const started = Date.now();
        const timer = setInterval(() => {
          if (!get().busy || Date.now() - started > 10000) {
            clearInterval(timer);
            resolve();
          }
        }, 100);
      });
    }
    const history = get().history;
    const convId = get().currentConvId;
    // 首条消息即会话话题：会话还是默认标题时立即用该消息命名（与后端规则一致，取前 20 字），侧栏即时生效
    const topic = Array.from(msg.split(/\s+/).filter(Boolean).join(" ")).slice(0, 20).join("");
    if (topic && convId) {
      set((s) => ({
        conversations: s.conversations.map((c) =>
          c.id === convId && (c.title === "新会话" || c.title === "默认会话" || !c.title.trim())
            ? { ...c, title: topic }
            : c
        ),
      }));
    }
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
    // 消息发出：极轻的触感反馈
    playSfx("message-sent");
    try {
      const reply = await chat(convId, msg, history, attachments);
      // 后端已按首条用户消息落库会话标题，回拉会话列表保持侧栏标题与服务端一致
      void get().loadConversations();
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
      // 任务完成：温润的木琴上行琶音
      playSfx("task-done");
    } catch (e) {
      playSfx("error");
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
        const started = Date.now();
        const timer = setInterval(() => {
          if ((!get().busy && !get().comparing) || Date.now() - started > 10000) {
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
      // 对比结果落库（assistant 消息，分支存 trace.branches），重启后仍可回看
      const convId = get().currentConvId;
      if (convId) void saveCompareResult(convId, branches).catch(() => {});
      playSfx("task-done");
    } catch (e) {
      playSfx("error");
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
    // 项目列表先于首次会话切换加载：启动续接项目会话时工作空间联动才能命中
    const [list] = await Promise.all([listConversations(), get().loadProjects()]);
    set({ conversations: list });
    if (list.length === 0) {
      await get().newConversation();
      return;
    }
    if (get().currentConvId) return;
    // 启动不续接最近会话：最近会话已有消息则开新会话；
    // 最近会话本身是空的就直接复用，避免每次启动堆积空会话
    try {
      const msgs = await getMessages(list[0].id);
      if (msgs.length > 0) {
        await get().newConversation();
      } else {
        await get().switchConversation(list[0].id);
      }
    } catch {
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
    // 项目↔工作空间联动：选中项目里的会话 = 切到该项目的工作目录
    get().followProjectWorkspace(id);
  },

  newConversation: async (projectId) => {
    const conv = await createConversation("新会话", projectId ?? null);
    set((s) => ({
      conversations: [conv, ...s.conversations],
      currentConvId: conv.id,
      history: [],
      streaming: "",
      comparing: false,
      thoughts: [],
      todos: [],
    }));
    // 在项目内新建会话：工作空间立即切到项目目录
    get().followProjectWorkspace(conv.id);
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

  // ---------------- 项目（侧边栏「项目」导航） ----------------

  loadProjects: async () => {
    set({ projects: await listProjects().catch(() => []) });
  },

  addProject: async (name, path) => {
    const list = await addProject(name, path);
    set({ projects: list });
  },

  removeProject: async (id) => {
    await deleteProject(id);
    // 该项目下的会话回到「未分组」：本地同步置空归属，避免整表回拉
    set((s) => ({
      projects: s.projects.filter((p) => p.id !== id),
      conversations: s.conversations.map((c) =>
        c.project_id === id ? { ...c, project_id: null } : c
      ),
    }));
  },

  moveConversation: async (convId, projectId) => {
    await setConversationProject(convId, projectId);
    set((s) => ({
      conversations: s.conversations.map((c) =>
        c.id === convId ? { ...c, project_id: projectId } : c
      ),
    }));
    // 归档的正是当前会话：工作空间同步跟随到项目目录
    if (projectId && get().currentConvId === convId) get().followProjectWorkspace(convId);
  },

  /** 项目↔工作空间联动：当前会话归属项目且绑定了目录时，把全局工作空间切到该目录。
      未分组会话不改动工作空间（保留手动设置）。 */
  followProjectWorkspace: (convId) => {
    const { conversations, projects } = get();
    const conv = conversations.find((c) => c.id === convId);
    const proj = conv?.project_id
      ? projects.find((p) => p.id === conv.project_id)
      : undefined;
    if (proj?.path) void setWorkspace(proj.path).catch(() => {});
  },
}));
