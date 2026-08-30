import { useEffect, useRef, useState } from "react";
import { getMemoryGraph, onMemoryRecall } from "../api";
import type { MemoryGraph, MemoryRow } from "../types";

/**
 * 记忆星图：点击水球全屏展开的记忆宇宙。
 * - 每条记忆是一颗星：大小=显著度，色相=记忆类型，位置由 mem_id 哈希稳定撒点
 * - 记忆间关联画成极淡的星链
 * - 白泽检索记忆时光线从星图射回（视觉上「取回记忆」）
 * - 顶部搜索即时点亮匹配星、压暗其余
 */

const KIND_COLOR: Record<string, string> = {
  fact: "#60a5fa",
  preference: "#f472b6",
  skill: "#34d399",
  event: "#fbbf24",
  person: "#a78bfa",
  topic: "#22d3ee",
};

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
  const queryRef = useRef("");
  queryRef.current = query;
  const pickedRef = useRef<MemoryRow | null>(null);
  pickedRef.current = picked;

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
      <canvas ref={canvasRef} className="galaxy-canvas" />
      <div className="galaxy-head">
        <span className="galaxy-title">✦ 记忆星图</span>
        <input
          className="galaxy-search"
          placeholder="搜索记忆…（回车无效，即时过滤）"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          autoFocus
        />
        <button className="replay-close" onClick={onClose} title="关闭 (Esc)">
          ✕
        </button>
      </div>
      {graph && graph.nodes.length === 0 && (
        <div className="galaxy-empty">这片宇宙还很空——聊得越多，星星越多。</div>
      )}
      {picked && (
        <div className="galaxy-tip" onClick={() => setPicked(null)}>
          <span className="galaxy-tip-kind">{picked.kind}</span>
          <span className="galaxy-tip-sal">显著度 {(picked.salience * 100).toFixed(0)}</span>
          <p>{picked.content}</p>
        </div>
      )}
      <div className="galaxy-foot">点击星星查看记忆 · Esc 关闭 · 白泽检索记忆时光线会射向水球</div>
    </div>
  );
}
