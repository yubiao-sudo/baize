// ==========================================================================
// 棱镜契约层 · Prism Nexus · 信号类型全集
// 与旧前端（kernel/types.ts / zustand store）完全无关联
// 所有 Cell 组件、Organelle 细胞器、Bridge 桥都只消费这一份不可变契约
// ==========================================================================

export type SignalTier =
  | "atomic"    // 原子信号（单 token / 单工具事件）
  | "pulse"     // 脉冲信号（消息边界、状态翻转）
  | "wave"      // 波信号（持续流、订阅流）
  | "prism"     // 棱镜信号（跨 Cell 广播、多主题切换）
  | "command";  // 指令信号（Cell → Bus → Bridge 上行调用）

export interface SignalEnvelope<T = unknown> {
  /** 信号唯一 id（UUID），用于幂等去重和审计回溯 */
  sid: string;
  /** 信号类型标识（点号分隔命名空间） e.g. "chat.token" / "approval.arrive" */
  kind: string;
  tier: SignalTier;
  /** 信号发射源：cell 名 / organelle 名 / bridge / user  */
  source: string;
  /** 毫秒时间戳 */
  ts: number;
  /** 载荷（由 kind 决定结构） */
  payload: T;
  /** 关联信号 sid（用于追踪链） */
  parent?: string;
  /** 信号优先级（0-9，越大优先级越高，仅用于命令通道排序） */
  prio?: number;
}

// --------------------------------------------------------------------
// Cell（细胞组件）通用身份卡
// --------------------------------------------------------------------
export interface PrismCellMeta {
  id: string;          // 细胞实例 id
  name: string;        // 人类可读名
  category: "conversation" | "toolstream" | "approval" | "memory" | "settings";
  /** 细胞输出的信号 kind 白名单（不声明则广播给所有订阅者） */
  emits?: string[];
  /** 细胞接收的信号 kind 白名单（不声明则默认忽略） */
  listens?: string[];
}

// --------------------------------------------------------------------
// 领域载荷
// --------------------------------------------------------------------
export interface ChatTurn {
  turnId: string;
  role: "user" | "assistant" | "system";
  text: string;
  /** 是否已流式渲染结束 */
  sealed: boolean;
  attachments?: string[];
}

export interface ToolFrame {
  frameId: string;
  tool: string;
  args: unknown;
  status: "pending" | "running" | "ok" | "fail" | "skip";
  /** 截断后结果（纯文本片段） */
  resultSnip?: string;
  startedAt?: number;
  finishedAt?: number;
}

export interface ApprovalItem {
  id: string;
  tool: string;
  cls: "ReadOnly" | "Write" | "HighRisk";
  args: unknown;
  /** 人类可读描述 */
  description?: string;
  arrivedAt: number;
}

export interface MemoryShard {
  id: string;
  content: string;
  tag: "fact" | "intent" | "artifact" | "session";
  weight: number;
  createdAt: number;
}

export interface ModelProfileLite {
  id: string;
  name: string;
  vendor: string;
  tier: "local" | "cloud" | "proxy";
  /** 已加载就绪 */
  ready: boolean;
}

export interface PrismThemeManifest {
  id: string;          // "aurora" | "nebula" | "glass" | "garden"
  name: string;        // 中文展示名
  description: string; // 一句话描述
}