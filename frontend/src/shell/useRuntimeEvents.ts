import { useEffect } from "react";
import { useChat } from "../kernel/store/chat";
import { useDock } from "../kernel/store/dock";
import {
  getModelConfig,
  getNotifyConfig,
  getWechatStatus,
  getFeishuStatus,
  onChatToken,
  onChatRoundReset,
  onThought,
  onTodoList,
  onTodoUpdate,
  onPermissionRequest,
  onEscalationLevel,
  setActiveModel,
} from "../kernel/api";
import type { EscalationLevelEvent, ModelProfile, PermissionRequest, ThoughtEvent, Todo } from "../kernel/types";

/**
 * Shell 层一次性挂载：事件订阅 + 初始数据拉取
 * 浏览器预览模式下 invoke 会 reject → 所有 .catch 吞掉（契约层优雅降级）
 */
export function useRuntimeEvents() {
  const addThought = useChat((s) => s.addThought);
  const setTodos = useChat((s) => s.setTodos);
  const appendStream = useChat((s) => s.appendStream);
  const resetStream = useChat((s) => s.resetStream);
  const addPending = useChat((s) => s.addPending);
  const pushNotice = useDock((s) => s.pushNotice);
  const setActiveModels = useDock((s) => s.setActiveModels);
  const setCurrentModelId = useDock((s) => s.setCurrentModelId);

  useEffect(() => {
    const unsubs: (void | (() => void))[] = [];
    let mounted = true;

    // ── 初始数据 ──────────────────────────────────────────────
    getModelConfig()
      .then((cfg) => {
        if (!mounted) return;
        setActiveModels(cfg.profiles as ModelProfile[]);
        if (cfg.active) setCurrentModelId(cfg.active);
      })
      .catch(() => {});
    // getNotifyConfig() 预热缓存
    getNotifyConfig().catch(() => {});
    getWechatStatus().catch(() => {});
    getFeishuStatus().catch(() => {});

    // ── 事件订阅 ──────────────────────────────────────────────
    // 1) Chat 流
    onChatToken((token: string) => appendStream(token)).then((u) => unsubs.push(u)).catch(() => {});
    onChatRoundReset(() => resetStream()).then((u) => unsubs.push(u)).catch(() => {});

    // 2) 思维流
    onThought((ev: ThoughtEvent) => {
      // 给种子事件一个 id（兼容前端种子觉醒事件）
      const withId: ThoughtEvent = { ...ev, id: ev.id ?? `th-${Date.now()}-${Math.random().toString(36).slice(2, 5)}` };
      addThought(withId);
    }).then((u) => unsubs.push(u)).catch(() => {});

    // 3) 任务台账
    onTodoList((todos: Todo[]) => setTodos(todos)).then((u) => unsubs.push(u)).catch(() => {});
    onTodoUpdate((todos: Todo[]) => setTodos(todos)).then((u) => unsubs.push(u)).catch(() => {});

    // 4) 审批联锁
    onPermissionRequest((req: PermissionRequest) => addPending(req)).then((u) => unsubs.push(u)).catch(() => {});

    // 5) 升级通知（L1..L5 级别触发 → 弹出卡片）
    onEscalationLevel((e: EscalationLevelEvent) => {
      pushNotice({
        tier: (e.level >= 4 ? "alert" : "info"),
        title: e.title || `L${e.level} · ${e.level_label}`,
        body: `${e.body}${e.detail ? `\n${e.detail}` : ""}`,
        onClick: () => useDock.getState().toggleSettings(true),
      });
    }).then((u) => unsubs.push(u)).catch(() => {});

    // ── 全局工具（审批联锁调用） ─────────────────────────────────
    (window as any).__crystal_approve = (id: string, remember: boolean) =>
      import("../kernel/api").then((api) => api.resolvePermission(id, true, remember)).catch(() => {});
    (window as any).__crystal_deny = (id: string, remember: boolean) =>
      import("../kernel/api").then((api) => api.resolvePermission(id, false, remember)).catch(() => {});

    return () => {
      mounted = false;
      unsubs.forEach((fn) => typeof fn === "function" && fn());
      delete (window as any).__crystal_approve;
      delete (window as any).__crystal_deny;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 懒加载会话列表
  useEffect(() => {
    useChat.getState().loadConversations().catch(() => {});
  }, []);
}