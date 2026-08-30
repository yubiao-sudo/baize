import { useEffect, useRef, useState } from "react";
import { getMemoryGraph, onMemoryRecall } from "../../core/api";
import type { MemoryGraph } from "../../core/types";

function hash01(s: string, salt: number): number {
  let h = 2166136261 ^ salt;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return ((h >>> 0) % 1000) / 1000;
}

interface Star {
  id: string;
  x: number;
  y: number;
  r: number;
}

/** 记忆星云：canvas 绘制记忆图谱，召回节点脉冲高亮 */
export default function MemoryNebula() {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const graphRef = useRef<MemoryGraph>({ nodes: [], edges: [] });
  const recallRef = useRef<Set<string>>(new Set());
  const [info, setInfo] = useState("");

  // 数据装载 + 召回订阅
  useEffect(() => {
    let disposed = false;
    let off: (() => void) | null = null;
    getMemoryGraph()
      .then((g) => {
        if (disposed) return;
        graphRef.current = g;
        setInfo(`${g.nodes.length} 记忆 · ${g.edges.length} 链接`);
      })
      .catch(() => {});
    onMemoryRecall((ids) => {
      recallRef.current = new Set(ids);
    }).then((f) => {
      if (disposed) f();
      else off = f;
    });
    return () => {
      disposed = true;
      off?.();
    };
  }, []);

  // 绘制循环
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    let raf = 0;

    const stars = new Map<string, Star>();
    const syncStars = () => {
      const next = new Map<string, Star>();
      for (const n of graphRef.current.nodes) {
        const prev = stars.get(n.mem_id);
        next.set(
          n.mem_id,
          prev ?? {
            id: n.mem_id,
            x: 0.08 + hash01(n.mem_id, 1) * 0.84,
            y: 0.1 + hash01(n.mem_id, 2) * 0.8,
            r: 1.6 + Math.min(n.salience, 1) * 3.2,
          }
        );
      }
      stars.clear();
      next.forEach((v, k) => stars.set(k, v));
    };

    const draw = (t: number) => {
      const w = canvas.clientWidth;
      const h = canvas.clientHeight;
      const dpr = window.devicePixelRatio || 1;
      if (canvas.width !== Math.round(w * dpr) || canvas.height !== Math.round(h * dpr)) {
        canvas.width = Math.round(w * dpr);
        canvas.height = Math.round(h * dpr);
      }
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, w, h);
      syncStars();
      const g = graphRef.current;

      // 星云底辉
      const grad = ctx.createRadialGradient(w / 2, h / 2, 0, w / 2, h / 2, Math.max(w, h) * 0.7);
      grad.addColorStop(0, "rgba(122,215,255,0.05)");
      grad.addColorStop(1, "rgba(0,0,0,0)");
      ctx.fillStyle = grad;
      ctx.fillRect(0, 0, w, h);

      // 记忆链接
      for (const e of g.edges) {
        const a = stars.get(e.from);
        const b = stars.get(e.to);
        if (!a || !b) continue;
        ctx.strokeStyle = `rgba(122,215,255,${Math.min(0.05 + e.weight * 0.12, 0.3)})`;
        ctx.lineWidth = 0.6;
        ctx.beginPath();
        ctx.moveTo(a.x * w, a.y * h);
        ctx.lineTo(b.x * w, b.y * h);
        ctx.stroke();
      }

      // 记忆星点（召回节点脉冲高亮）
      for (const n of stars.values()) {
        const px = n.x * w + Math.sin(t * 0.0004 + n.x * 20) * 3;
        const py = n.y * h + Math.cos(t * 0.0005 + n.y * 17) * 3;
        const recalled = recallRef.current.has(n.id);
        ctx.fillStyle = recalled ? "#ffcf24" : "rgba(156,230,255,0.85)";
        ctx.beginPath();
        ctx.arc(px, py, n.r, 0, Math.PI * 2);
        ctx.fill();
        if (recalled) {
          const pulse = 0.5 + 0.5 * Math.sin(t * 0.006);
          ctx.strokeStyle = `rgba(255,207,36,${0.25 + pulse * 0.45})`;
          ctx.lineWidth = 1;
          ctx.beginPath();
          ctx.arc(px, py, n.r + 3 + pulse * 5, 0, Math.PI * 2);
          ctx.stroke();
        }
      }
      raf = requestAnimationFrame(draw);
    };
    raf = requestAnimationFrame(draw);
    return () => cancelAnimationFrame(raf);
  }, []);

  return (
    <div className="nebula">
      <canvas ref={canvasRef} />
      <span className="nebula-info">{info}</span>
    </div>
  );
}