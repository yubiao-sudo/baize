// ==========================================================================
// 会话树 + 记忆碎片（左侧面板两个细胞）
// ==========================================================================
import { useMemo, useState } from "react";
import { PrismCell } from "../PrismCell";
import { useCell } from "../cell.hooks";
import type { MemoryShard } from "../../bus/prism.types";

const INIT_SESSIONS = [
  { id: "s-main",   title: "棱镜舱 · 当前会话", date: "今日", active: true },
  { id: "s-demo-1", title: "多模型对比测试：R1 vs Qwen", date: "昨日" },
  { id: "s-demo-2", title: "会议纪要整理工作流", date: "8-25" },
  { id: "s-demo-3", title: "某项目代码 Review", date: "8-24" },
  { id: "s-demo-4", title: "SQL 负载均衡脚本生成", date: "8-23" },
];

export function SessionTree() {
  const [items, setItems] = useState(INIT_SESSIONS);
  const [query, setQuery] = useState("");

  useCell(
    { name: "会话·目录树", category: "conversation", emits: ["session.switch", "session.new"] },
    {
      "cmd.chat.send": (env) => {
        // 有消息时，把当前会话置顶并重命名（截断内容前 20 字）
        const msg = String((env.payload as { message?: string }).message ?? "");
        if (!msg) return;
        setItems((prev) => {
          const first = prev[0];
          if (!first) return prev;
          const next = [...prev];
          next[0] = {
            ...first,
            title: msg.slice(0, 18) + (msg.length > 18 ? "…" : ""),
            date: "刚刚",
          };
          return next;
        });
      },
    }
  );

  const filtered = useMemo(
    () =>
      query
        ? items.filter((s) => s.title.toLowerCase().includes(query.toLowerCase()))
        : items,
    [items, query]
  );

  return (
    <PrismCell
      className="stretch"
      title="会话·目录树"
      subtitle="Session Tree"
      tools={
        <button className="prism-btn" style={{ padding: "4px 10px", fontSize: 11.5 }} onClick={() => {
          const ns = { id: "s-" + Date.now(), title: "新折射会话", date: "刚刚", active: true };
          setItems((prev) => [ns, ...prev.map((s) => ({ ...s, active: false }))]);
        }}>＋ 新会话</button>
      }
      bodyClassName=""
    >
      <input
        className="prism-input"
        placeholder="搜索会话…"
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        style={{ marginBottom: 10, fontSize: 12.5 }}
      />
      <div className="scroll-area">
        <div className="session-list">
          {filtered.map((s) => (
            <div
              key={s.id}
              className={`session-item ${s.active ? "active" : ""}`}
              onClick={() =>
                setItems((prev) => prev.map((x) => ({ ...x, active: x.id === s.id })))
              }
            >
              <span style={{ width: 6, height: 6, borderRadius: 3, background: s.active ? "var(--prism-action)" : "var(--prism-ink-3)" }} />
              <span className="s-title">{s.title}</span>
              <span className="s-date">{s.date}</span>
            </div>
          ))}
          {filtered.length === 0 ? (
            <div style={{ color: "var(--prism-ink-3)", fontSize: 12, textAlign: "center", padding: 12 }}>
              没有匹配的会话
            </div>
          ) : null}
        </div>
      </div>
    </PrismCell>
  );
}

// --------------------------------------------------------------------

const INIT_SHARDS: MemoryShard[] = [
  { id: "seed-1", content: "用户偏好新前端架构：信号流驱动 + 细胞组件", tag: "intent", weight: 0.92, createdAt: Date.now() - 86400_000 },
  { id: "seed-2", content: "视觉必须摒弃终末地工业风 & 水晶工坊紫晶", tag: "fact",   weight: 0.88, createdAt: Date.now() - 3600_000 },
  { id: "seed-3", content: "新旧前端完全隔离，独立端口运行", tag: "fact", weight: 0.96, createdAt: Date.now() - 7200_000 },
  { id: "seed-4", content: "主题引擎用运行时 CSS 变量，而非加载不同 CSS 文件", tag: "artifact", weight: 0.72, createdAt: Date.now() - 1800_000 },
];

export function MemoryPrism() {
  const [shards, setShards] = useState<MemoryShard[]>(INIT_SHARDS);
  const [onlyIntent, setOnlyIntent] = useState(false);

  useCell({ name: "记忆·棱镜", category: "memory" }, {
    "memory.shard.added": (env) => {
      const p = env.payload as MemoryShard;
      setShards((prev) => [p, ...prev].slice(0, 32));
    },
  });

  const list = useMemo(
    () => (onlyIntent ? shards.filter((s) => s.tag === "intent" || s.tag === "fact") : shards),
    [shards, onlyIntent]
  );

  return (
    <PrismCell
      className="stretch"
      title="记忆·棱镜"
      subtitle="Memory Prism · Weighted Shards"
      tools={
        <button
          className={`prism-btn ${onlyIntent ? "" : "ghost"}`}
          style={{ padding: "3px 10px", fontSize: 11 }}
          onClick={() => setOnlyIntent((v) => !v)}
        >
          {onlyIntent ? "仅关键碎片" : "全部碎片"}
        </button>
      }
      bodyClassName=""
    >
      <div className="scroll-area">
        {list.map((s) => (
          <div
            key={s.id}
            className="memory-shard"
            style={{
              borderLeftColor:
                s.tag === "intent" ? "var(--prism-axis)" :
                s.tag === "fact"   ? "var(--prism-face-b)" :
                s.tag === "artifact" ? "var(--prism-face-g)" : "var(--prism-ink-3)",
            }}
          >
            {s.content}
            <div className="wt">
              <span style={{
                display: "inline-block", padding: "1px 6px", borderRadius: 999,
                background: "color-mix(in srgb, var(--prism-axis) 18%, transparent)",
                marginRight: 6, color: "var(--prism-ink-1)",
                letterSpacing: "0.08em", textTransform: "uppercase",
              }}>{s.tag}</span>
              weight {(s.weight * 100).toFixed(0)} ·{" "}
              {new Date(s.createdAt).toLocaleString("zh-CN", { hour12: false })}
            </div>
          </div>
        ))}
      </div>
    </PrismCell>
  );
}