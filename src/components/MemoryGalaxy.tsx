import { useEffect, useRef, useState } from "react";
import { addMemory, deleteMemoryById, forgetMemory, getMemoryGraph, listMemoriesPanel, onMemoryRecall, pinMemoryById, updateMemoryById } from "../api";
import type { MemoryGraph, MemoryRow } from "../types";

/**
 * 记忆星图：点击水球全屏展开的记忆宇宙。
 * - 每条记忆是一颗星：大小=显著度，色相=记忆类型，位置由 mem_id 哈希稳定撒点
 * - 记忆间关联画成极淡的星链
 * - 白泽检索记忆时光线从星图射回（视觉上「取回记忆」）
 * - 顶部搜索即时点亮匹配星、压暗其余
 * - 「列表」视图：按类型浏览明细，支持置顶（提升召回权重）与删除
 */

const KIND_COLOR: Record<string, string> = {
  fact: "#60a5fa",
  preference: "#f472b6",
  skill: "#34d399",
  event: "#fbbf24",
  person: "#a78bfa",
  topic: "#22d3ee",
  lesson: "#fb7185",
  recipe: "#2dd4bf",
  task: "#c084fc",
  episodic: "#94a3b8",
};

const KIND_LABEL: Record<string, string> = {
  fact: "事实",
  preference: "偏好",
  skill: "技能",
  event: "事件",
  person: "人物",
  topic: "话题",
  lesson: "经验",
  recipe: "配方",
  task: "任务",
  episodic: "情景",
  project: "项目",
  habit: "习惯",
};

/** 列表视图的类型页签（全部 + 高价值类型优先） */
const KIND_TABS: { key: string; label: string }[] = [
  { key: "", label: "全部" },
  { key: "lesson", label: "经验" },
  { key: "recipe", label: "配方" },
  { key: "task", label: "任务" },
  { key: "preference", label: "偏好" },
  { key: "fact", label: "事实" },
];

function hashPos(id: string): number {
  let h = 2166136261;
  for (let i = 0; i < id.length; i++) {
    h ^= id.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return (h >>> 0) / 4294967295;
}

interface Star {
  x: number;
  y: number;
  r: number;
  color: string;
  tw: number; // 闪烁相位
  node: MemoryRow;
}

export default function MemoryGalaxy({ onClose }: { onClose: () => void }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [graph, setGraph] = useState<MemoryGraph | null>(null);
  const [query, setQuery] = useState("");
  const [picked, setPicked] = useState<MemoryRow | null>(null);
  const [view, setView] = useState<"map" | "list">("map");
  const [rows, setRows] = useState<MemoryRow[]>([]);
  const [kindTab, setKindTab] = useState("");
  // 新增记忆
  const [newOpen, setNewOpen] = useState(false);
  const [newText, setNewText] = useState("");
  const [newKind, setNewKind] = useState("fact");
  // 按关键词遗忘
  const [forgetText, setForgetText] = useState("");
  // 行内编辑
  const [editId, setEditId] = useState<string | null>(null);
  const [editText, setEditText] = useState("");
  const queryRef = useRef("");
  queryRef.current = query;
  const pickedRef = useRef<MemoryRow | null>(null);
  pickedRef.current = picked;

  const loadRows = (kind: string) => {
    void listMemoriesPanel(kind || undefined)
      .then(setRows)
      .catch(() => {});
  };

  useEffect(() => {
    if (view === "list") loadRows(kindTab);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [view, kindTab]);

  const handlePin = async (id: string) => {
    await pinMemoryById(id).catch(() => {});
    loadRows(kindTab);
    void getMemoryGraph().then(setGraph).catch(() => {});
  };

  const handleDelete = async (id: string) => {
    await deleteMemoryById(id).catch(() => {});
    setRows((r) => r.filter((x) => x.mem_id !== id));
    setPicked(null);
    void getMemoryGraph().then(setGraph).catch(() => {});
  };

  const handleAdd = async () => {
    const t = newText.trim();
    if (!t) return;
    await addMemory(t, newKind).catch(() => {});
    setNewText("");
    setNewOpen(false);
    loadRows(kindTab);
    void getMemoryGraph().then(setGraph).catch(() => {});
  };

  const handleForget = async () => {
    const k = forgetText.trim();
    if (!k) return;
    await forgetMemory(k).catch(() => {});
    setForgetText("");
    loadRows(kindTab);
    void getMemoryGraph().then(setGraph).catch(() => {});
  };

  const startEdit = (m: MemoryRow) => {
    setEditId(m.mem_id);
    setEditText(m.content);
  };

  const saveEdit = async () => {
    if (!editId) return;
    const t = editText.trim();
    if (t) await updateMemoryById(editId, t).catch(() => {});
    setEditId(null);
    loadRows(kindTab);
    void getMemoryGraph().then(setGraph).catch(() => {});
  };

  useEffect(() => {
    void getMemoryGraph().then(setGraph).catch(() => {});
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || !graph) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    let raf = 0;
    let stars: Star[] = [];
    let edges: { a: Star; b: Star; w: number }[] = [];
    const rays: { x: number; y: number; t0: number }[] = [];

    const build = () => {
      // dpr 钳制到 2：高 DPI 屏（150%/200% 缩放）下不钳制会让全屏 canvas 像素量翻倍，徒增显存与填充率
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      canvas.width = canvas.clientWidth * dpr;
      canvas.height = canvas.clientHeight * dpr;
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      const W = canvas.clientWidth;
      const H = canvas.clientHeight;
      // 撒点：哈希 → 椭圆分布（避开正中，视觉像悬浮的星云）
      const maxSal = Math.max(0.001, ...graph.nodes.map((n) => n.salience));
      stars = graph.nodes.map((n) => {
        const a = hashPos(n.mem_id) * Math.PI * 2;
        const d = 0.18 + hashPos(n.mem_id + "d") * 0.34;
        const x = W / 2 + Math.cos(a) * W * d;
        const y = H / 2 + Math.sin(a) * H * d * 0.92;
        const r = 2.2 + (n.salience / maxSal) * 4.6;
        const color = KIND_COLOR[n.kind] || "#93c5fd";
        return { x, y, r, color, tw: hashPos(n.mem_id + "t") * Math.PI * 2, node: n };
      });
      const byId = new Map(stars.map((s) => [s.node.mem_id, s]));
      edges = [];
      for (const e of graph.edges) {
        const a = byId.get(e.from);
        const b = byId.get(e.to);
        if (a && b) edges.push({ a, b, w: e.weight });
      }
    };
    build();
    const onResize = () => build();
    window.addEventListener("resize", onResize);

    // 订阅记忆召回：星光从节点射向中心（水球方向）
    let unRecall: (() => void) | undefined;
    void onMemoryRecall((ids) => {
      const t0 = performance.now();
      for (const s of stars) {
        if (ids.includes(s.node.mem_id)) rays.push({ x: s.x, y: s.y, t0: t0 + Math.random() * 300 });
      }
      if (rays.length === 0 && stars.length > 0) {
        // 无精确命中时从最亮的几颗泛射
        [...stars].sort((a, b) => b.r - a.r).slice(0, 3).forEach((s) => rays.push({ x: s.x, y: s.y, t0 }));
      }
    }).then((f) => (unRecall = f));

    let unlinked = false;
    const draw = (now: number) => {
      if (unlinked) return;
      const W = canvas.clientWidth;
      const H = canvas.clientHeight;
      ctx.clearRect(0, 0, W, H);
      const q = queryRef.current.trim().toLowerCase();
      const hit = (s: Star) => !q || s.node.content.toLowerCase().includes(q);

      // 星链
      ctx.lineWidth = 0.6;
      for (const { a, b, w } of edges) {
        const alpha = Math.min(0.16, 0.04 + w * 0.06) * (hit(a) && hit(b) ? 1 : 0.25);
        ctx.strokeStyle = `rgba(148, 197, 253, ${alpha})`;
        ctx.beginPath();
        ctx.moveTo(a.x, a.y);
        ctx.lineTo(b.x, b.y);
        ctx.stroke();
      }

      // 记忆召回光线（1.2s 淡出）
      const cx = W / 2;
      const cy = H * 0.86;
      for (let i = rays.length - 1; i >= 0; i--) {
        const p = (now - rays[i].t0) / 1200;
        if (p < 0) continue;
        if (p > 1) {
          rays.splice(i, 1);
          continue;
        }
        const alpha = 0.5 * (1 - p);
        ctx.strokeStyle = `rgba(34, 211, 238, ${alpha})`;
        ctx.lineWidth = 1.2;
        ctx.setLineDash([6, 8]);
        ctx.lineDashOffset = -now / 20;
        ctx.beginPath();
        ctx.moveTo(rays[i].x, rays[i].y);
        ctx.lineTo(cx, cy);
        ctx.stroke();
        ctx.setLineDash([]);
      }

      // 星星：闪烁 + 光晕
      for (const s of stars) {
        const tw = 0.55 + 0.45 * Math.sin(now / 700 + s.tw);
        const on = hit(s);
        const alpha = on ? tw : 0.08;
        const glow = ctx.createRadialGradient(s.x, s.y, 0, s.x, s.y, s.r * 5);
        glow.addColorStop(0, s.color + Math.round(alpha * 200).toString(16).padStart(2, "0"));
        glow.addColorStop(1, "transparent");
        ctx.fillStyle = glow;
        ctx.beginPath();
        ctx.arc(s.x, s.y, s.r * 5, 0, Math.PI * 2);
        ctx.fill();
        ctx.fillStyle = on ? s.color : "rgba(148,197,253,0.1)";
        ctx.globalAlpha = on ? alpha : 0.3;
        ctx.beginPath();
        ctx.arc(s.x, s.y, s.r, 0, Math.PI * 2);
        ctx.fill();
        ctx.globalAlpha = 1;
      }
      raf = requestAnimationFrame(draw);
    };
    raf = requestAnimationFrame(draw);

    // 点击拾取
    const onClick = (ev: MouseEvent) => {
      const rect = canvas.getBoundingClientRect();
      const mx = ev.clientX - rect.left;
      const my = ev.clientY - rect.top;
      let best: Star | null = null;
      let bd = 18;
      for (const s of stars) {
        const d = Math.hypot(s.x - mx, s.y - my);
        if (d < bd) {
          bd = d;
          best = s;
        }
      }
      setPicked(best ? best.node : null);
    };
    canvas.addEventListener("click", onClick);

    return () => {
      unlinked = true;
      cancelAnimationFrame(raf);
      unRecall?.();
      window.removeEventListener("resize", onResize);
      canvas.removeEventListener("click", onClick);
    };
  }, [graph]);

  return (
    <div className="galaxy-mask">
      {view === "map" && <canvas ref={canvasRef} className="galaxy-canvas" />}
      <div className="galaxy-head">
        <span className="galaxy-title">✦ 记忆星图</span>
        {view === "map" ? (
          <input
            className="galaxy-search"
            placeholder="搜索记忆…（回车无效，即时过滤）"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            autoFocus
          />
        ) : (
          <div className="galaxy-tabs">
            {KIND_TABS.map((t) => (
              <button
                key={t.key}
                className={`galaxy-tab${kindTab === t.key ? " active" : ""}`}
                onClick={() => setKindTab(t.key)}
              >
                {t.label}
              </button>
            ))}
          </div>
        )}
        <button
          className="galaxy-view-btn"
          onClick={() => setView(view === "map" ? "list" : "map")}
          title={view === "map" ? "切换到列表视图（可置顶/删除）" : "切换到星图视图"}
        >
          {view === "map" ? "☰ 列表" : "✦ 星图"}
        </button>
        <button className="replay-close" onClick={onClose} title="关闭 (Esc)">
          ✕
        </button>
      </div>
      {view === "map" && graph && graph.nodes.length === 0 && (
        <div className="galaxy-empty">这片宇宙还很空——聊得越多，星星越多。</div>
      )}
      {view === "list" && (
        <div className="galaxy-list">
          <div className="galaxy-toolbar">
            <button
              className="galaxy-add-btn"
              onClick={() => {
                setNewOpen((v) => !v);
                setNewText("");
              }}
            >
              {newOpen ? "取消" : "＋ 记一条"}
            </button>
            <input
              className="galaxy-forget-input"
              placeholder="按关键词遗忘…"
              value={forgetText}
              onChange={(e) => setForgetText(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void handleForget();
              }}
            />
            <button className="galaxy-forget-btn" onClick={() => void handleForget()} disabled={!forgetText.trim()}>
              遗忘
            </button>
          </div>
          {newOpen && (
            <div className="galaxy-add-panel">
              <textarea
                className="galaxy-add-input"
                placeholder="输入要记住的内容…"
                value={newText}
                onChange={(e) => setNewText(e.target.value)}
                rows={2}
                autoFocus
              />
              <div className="galaxy-add-actions">
                <select className="galaxy-add-kind" value={newKind} onChange={(e) => setNewKind(e.target.value)}>
                  {Object.entries(KIND_LABEL).map(([k, v]) => (
                    <option key={k} value={k}>
                      {v}
                    </option>
                  ))}
                </select>
                <button className="galaxy-add-ok" onClick={() => void handleAdd()} disabled={!newText.trim()}>
                  保存
                </button>
              </div>
            </div>
          )}
          {rows.length === 0 && <div className="galaxy-empty">该类型下暂无记忆。</div>}
          {rows.map((m) => (
            <div key={m.mem_id} className="galaxy-row" onClick={() => setPicked(m)}>
              <span
                className="galaxy-row-kind"
                style={{
                  color: KIND_COLOR[m.kind] || "#93c5fd",
                  borderColor: (KIND_COLOR[m.kind] || "#93c5fd") + "66",
                }}
              >
                {KIND_LABEL[m.kind] || m.kind}
              </span>
              {editId === m.mem_id ? (
                <input
                  className="galaxy-row-edit"
                  value={editText}
                  onChange={(e) => setEditText(e.target.value)}
                  onClick={(e) => e.stopPropagation()}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") void saveEdit();
                    if (e.key === "Escape") setEditId(null);
                  }}
                  autoFocus
                />
              ) : (
                <span className="galaxy-row-content">{m.content}</span>
              )}
              <span className="galaxy-row-sal" title="显著度（置顶或常用心会提升）">
                {m.salience}
              </span>
              {editId === m.mem_id ? (
                <button
                  className="galaxy-row-btn"
                  title="保存修改"
                  onClick={(e) => {
                    e.stopPropagation();
                    void saveEdit();
                  }}
                >
                  ✓
                </button>
              ) : (
                <button
                  className="galaxy-row-btn"
                  title="编辑这条记忆"
                  onClick={(e) => {
                    e.stopPropagation();
                    startEdit(m);
                  }}
                >
                  ✎
                </button>
              )}
              <button
                className="galaxy-row-btn"
                title="置顶：提升召回权重"
                onClick={(e) => {
                  e.stopPropagation();
                  void handlePin(m.mem_id);
                }}
              >
                ↑
              </button>
              <button
                className="galaxy-row-btn del"
                title="删除这条记忆"
                onClick={(e) => {
                  e.stopPropagation();
                  void handleDelete(m.mem_id);
                }}
              >
                ✕
              </button>
            </div>
          ))}
        </div>
      )}
      {picked && (
        <div className="galaxy-tip" onClick={() => setPicked(null)}>
          <span className="galaxy-tip-kind">{picked.kind}</span>
          <span className="galaxy-tip-sal">显著度 {(picked.salience * 100).toFixed(0)}</span>
          <p>{picked.content}</p>
        </div>
      )}
      <div className="galaxy-foot">
        {view === "map"
          ? "点击星星查看记忆 · Esc 关闭 · 白泽检索记忆时光线会射向水球"
          : "＋ 记一条 · ✎ 编辑 · ↑ 置顶 · ✕ 删除 · 遗忘按关键词 · Esc 关闭"}
      </div>
    </div>
  );
}
