// ==========================================================================
// 棱镜信号流总线（Prism Signal Bus）
// --------------------------------------------------------------------------
// 架构设计亮点（与旧前端 zustand store / 事件订阅 模型完全不同）：
//   1. 所有组件通过 Bus 收发「带签名的信封」（SignalEnvelope），不做直接 props 传值
//   2. 每个订阅者（Cell/Organelle）声明 listen/emits 白名单，Bus 负责过滤
//   3. 内置 5 级信号层（atomic/pulse/wave/prism/command）优先级和路由策略不同
//   4. Command 信号通过 Bridge 细胞器向上调用 Tauri，结果以 Pulse 信号回灌
//   5. 全链路审计：保留最近 N 条信封供 Inspector Cell 追踪调试
// ==========================================================================
import type { SignalEnvelope, SignalTier, PrismCellMeta } from "./prism.types";

let _sid = 0;
const uid = (prefix: string) => `${prefix}-${Date.now().toString(36)}-${(++_sid).toString(36)}`;

export type SignalHandler = (env: SignalEnvelope) => void | Promise<void>;

interface Subscriber {
  key: string;
  meta?: PrismCellMeta;
  listenKinds: string[];   // 空数组 = 监听所有
  handler: SignalHandler;
}

type TierRouter = (subs: Subscriber[], env: SignalEnvelope) => Subscriber[];

/** 按层路由：不同层采用不同分发排序/合并策略 */
const tierRouter: Record<SignalTier, TierRouter> = {
  // 原子信号（chat token）：只给声明了 exact kind 的订阅者，按注册顺序
  atomic: (subs, env) => subs.filter((s) => s.listenKinds.length === 0 || s.listenKinds.includes(env.kind)),
  // 脉冲信号（消息边界）：同 atomic，但优先级先给声明了 kind 的订阅者
  pulse: (subs, env) => {
    const exact = subs.filter((s) => s.listenKinds.includes(env.kind));
    const any = subs.filter((s) => s.listenKinds.length === 0);
    return [...exact, ...any];
  },
  // 波信号（连续流）：广播给所有匹配订阅者，保持顺序
  wave: (subs, env) => subs.filter((s) => s.listenKinds.length === 0 || s.listenKinds.includes(env.kind)),
  // 棱镜信号（主题/全局广播）：先广播，再触发主题变更（副作用在 organelle）
  prism: (subs, env) => subs,
  // 指令信号：按优先级排序，优先级高的 handler 先消费，仅给声明 kind 的订阅者
  command: (subs, env) => {
    const matched = subs.filter((s) => s.listenKinds.includes(env.kind));
    return matched.sort((a, b) => (b.meta?.emits?.length ?? 0) - (a.meta?.emits?.length ?? 0));
  },
};

export class PrismBus {
  private subscribers: Map<string, Subscriber> = new Map();
  /** 审计日志（环形缓冲） */
  private audit: SignalEnvelope[] = [];
  private readonly auditSize: number;
  /** 去重指纹（防止重复 command） */
  private dedup: Set<string> = new Set();
  private readonly dedupWindowMs = 50;

  constructor(auditSize = 512) {
    this.auditSize = auditSize;
  }

  /** 订阅：可选绑定 cell 元信息（推荐声明 listens 白名单以减少广播压力） */
  subscribe(handler: SignalHandler, meta?: PrismCellMeta, listenKinds: string[] = []): () => void {
    const key = meta?.id ?? uid("sub");
    this.subscribers.set(key, { key, meta, listenKinds, handler });
    return () => this.subscribers.delete(key);
  }

  /** 发射信号：所有 Cell 都走这一个 API，自动写审计、去重、路由 */
  emit<T = unknown>(
    kind: string,
    payload: T,
    opts: {
      tier?: SignalTier;
      source?: string;
      parent?: string;
      prio?: number;
      dedupKey?: string;
    } = {}
  ): SignalEnvelope<T> {
    const tier = opts.tier ?? (kind.startsWith("cmd.") ? "command" : "pulse");
    const source = opts.source ?? "unknown";
    const env: SignalEnvelope<T> = {
      sid: uid("sig"),
      kind,
      tier,
      source,
      ts: performance.now(),
      payload,
      parent: opts.parent,
      prio: opts.prio,
    };
    // 去重：仅对 command 级别生效（避免用户连点触发多次）
    if (opts.dedupKey && tier === "command") {
      const fp = `${source}:${opts.dedupKey}`;
      if (this.dedup.has(fp)) return env;
      this.dedup.add(fp);
      setTimeout(() => this.dedup.delete(fp), this.dedupWindowMs);
    }
    this.pushAudit(env as SignalEnvelope);
    this.dispatch(env as SignalEnvelope);
    return env;
  }

  private dispatch(env: SignalEnvelope) {
    const subs = [...this.subscribers.values()];
    const receivers = tierRouter[env.tier](subs, env);
    // async 非阻塞：任何一个 Cell 抛错都不影响其他 Cell（熔断隔离）
    for (const sub of receivers) {
      Promise.resolve()
        .then(() => sub.handler(env))
        .catch((err) => {
          // eslint-disable-next-line no-console
          console.warn(`[PrismBus] 细胞 ${sub.key} 处理信号 ${env.kind} 失败`, err);
        });
    }
  }

  private pushAudit(env: SignalEnvelope) {
    this.audit.push(env);
    if (this.audit.length > this.auditSize) this.audit.shift();
  }

  /** 审计查询：按 kind 前缀过滤，供 inspector 使用 */
  inspect(prefix = "", limit = 64): SignalEnvelope[] {
    const slice = this.audit.slice(-limit);
    return prefix ? slice.filter((e) => e.kind.startsWith(prefix)) : slice;
  }
}

/** 全局单例：所有 Cell 共用同一总线 */
export const prismBus = new PrismBus(1024);

/** 便捷 hook-like helper（供非 react 模块使用） */
export function busOn(kind: string, handler: SignalHandler, meta?: PrismCellMeta) {
  return prismBus.subscribe(handler, meta, [kind]);
}