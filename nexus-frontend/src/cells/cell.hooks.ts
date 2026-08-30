// ==========================================================================
// 细胞基元 · 通用 useCell Hook
// --------------------------------------------------------------------------
// 所有 Cell 组件用 useCell 注册身份和订阅。
// 与旧前端 useRuntimeEvents + 分散 useState 的区别：
//   - 所有 state 变更必须来自对应的 Bus 信号，组件自身不持有业务真值
//   - 每 Cell 自动声明 emits / listens 白名单，Bus 审计面板可追踪
// ==========================================================================
import { useEffect, useMemo, useRef, useState } from "react";
import { prismBus } from "../bus/prism.bus";
import type { PrismCellMeta, SignalEnvelope } from "../bus/prism.types";

export interface CellHandlers {
  [kind: string]: (env: SignalEnvelope) => void;
}

export interface UseCellResult {
  meta: PrismCellMeta;
  /** 便捷发射 —— source 自动填 cell id */
  emit: <T>(
    kind: string,
    payload: T,
    opts?: Partial<Pick<SignalEnvelope, "tier" | "prio" | "parent">> & { dedupKey?: string }
  ) => SignalEnvelope<T>;
}

export function useCell(partialMeta: Omit<PrismCellMeta, "id"> & { id?: string }, handlers: CellHandlers): UseCellResult {
  const idRef = useRef(partialMeta.id ?? `cell-${Math.random().toString(36).slice(2, 9)}`);
  const meta: PrismCellMeta = useMemo(
    () => ({
      id: idRef.current,
      name: partialMeta.name,
      category: partialMeta.category,
      emits: partialMeta.emits,
      listens: Object.keys(handlers),
    }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [partialMeta.name, partialMeta.category]
  );

  // 用 ref 保证 handler 的最新闭包（避免每次 render 重新订阅）
  const handlersRef = useRef(handlers);
  handlersRef.current = handlers;

  useEffect(() => {
    const unsub = prismBus.subscribe(
      (env) => {
        const fn = handlersRef.current[env.kind];
        if (fn) fn(env);
      },
      meta,
      Object.keys(handlersRef.current)
    );
    // 发射 cell 启动脉冲（用于 Inspector 审计）
    prismBus.emit("cell.mount", { id: meta.id, name: meta.name, category: meta.category }, {
      tier: "pulse",
      source: meta.id,
    });
    return () => {
      unsub();
      prismBus.emit("cell.unmount", { id: meta.id }, { tier: "pulse", source: meta.id });
    };
  }, [meta]);

  const emit: UseCellResult["emit"] = (kind, payload, opts = {}) =>
    prismBus.emit(kind, payload, {
      source: meta.id,
      tier: opts.tier ?? (kind.startsWith("cmd.") ? "command" : "pulse"),
      parent: opts.parent,
      prio: opts.prio,
      dedupKey: opts.dedupKey,
    });

  return { meta, emit };
}

/** 小工具：在组件内订阅某类信号并响应式保存最新值 */
export function useSignalValue<T>(kind: string, initial: T, pick: (env: SignalEnvelope) => T = (e) => e.payload as T): T {
  const [v, setV] = useState(initial);
  useEffect(() => {
    return prismBus.subscribe((env) => setV(pick(env)), undefined, [kind]);
  }, [kind, pick]);
  return v;
}