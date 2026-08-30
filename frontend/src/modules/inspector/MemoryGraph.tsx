import { useEffect, useRef } from "react";

/**
 * 水晶工坊记忆星云（调色板：紫+玫+召回=极光绿）
 * 和 Endfield 青黄完全不同。
 */
export function MemoryGraph() {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const nodesRef = useRef<{ x: number; y: number; vx: number; vy: number; r: number; recalled: boolean; label: string }[]>([]);

  useEffect(() => {
    // 初始化节点（mock：模拟 28 颗记忆星点，含 5 颗召回态）
    const labels = ["用户偏好", "项目记忆", "任务约束", "API 契约", "最近对话", "工具调用模式", "架构约定", "测试基线", "上下文锚点", "近期主题", "错误修复", "启动参数", "模型链路", "审批策略", "通知分级", "GUI 规则", "端口分配", "组件分层", "主题令牌", "动画时序", "画布尺寸", "附件路径", "工作目录", "任务进度", "分支选择", "OCR 语言", "屏幕锁定", "快捷键"];
    const nodes = labels.map((l, i) => ({
      x: 0, y: 0, vx: 0, vy: 0,
      r: 1.6 + Math.random() * 3.2,
      recalled: i < 4 || Math.random() < 0.12,
      label: l,
    }));
    nodesRef.current = nodes;

    let raf = 0;
    let lastT = performance.now();
    const layout = () => {
      const cv = canvasRef.current; if (!cv) return;
      const W = cv.clientWidth; const H = cv.clientHeight;
      const dpr = Math.min(2, window.devicePixelRatio || 1);
      if (cv.width !== W * dpr || cv.height !== H * dpr) {
        cv.width = W * dpr; cv.height = H * dpr;
        // 首次按画布尺寸散布
        nodes.forEach((n) => {
          if (n.x === 0 && n.y === 0) {
            n.x = 40 + Math.random() * (W - 80);
            n.y = 40 + Math.random() * (H - 80);
            n.vx = (Math.random() - 0.5) * 0.25;
            n.vy = (Math.random() - 0.5) * 0.25;
          }
        });
      }
      const ctx = cv.getContext("2d"); if (!ctx) return;
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, W, H);

      const now = performance.now();
      const dt = Math.min(40, now - lastT) / 16;
      lastT = now;

      // 连边（近邻）
      for (let i = 0; i < nodes.length; i++) {
        const a = nodes[i];
        a.x += a.vx * dt; a.y += a.vy * dt;
        if (a.x < 16 || a.x > W - 16) a.vx *= -1;
        if (a.y < 16 || a.y > H - 16) a.vy *= -1;
        for (let j = i + 1; j < nodes.length; j++) {
          const b = nodes[j];
          const dx = a.x - b.x; const dy = a.y - b.y;
          const d2 = dx * dx + dy * dy;
          if (d2 < 90 * 90) {
            const alpha = (1 - Math.sqrt(d2) / 90) * 0.18;
            ctx.strokeStyle = (a.recalled && b.recalled)
              ? `rgba(110,244,197,${alpha * 1.6})`
              : `rgba(196,157,255,${alpha})`;
            ctx.lineWidth = 0.7;
            ctx.beginPath(); ctx.moveTo(a.x, a.y); ctx.lineTo(b.x, b.y); ctx.stroke();
          }
        }
      }
      // 节点
      nodes.forEach((n) => {
        const color = n.recalled ? "255,142,203" : "196,157,255";
        const grad = ctx.createRadialGradient(n.x, n.y, 0, n.x, n.y, n.r * 4);
        grad.addColorStop(0, `rgba(${color},0.55)`);
        grad.addColorStop(1, `rgba(${color},0)`);
        ctx.fillStyle = grad;
        ctx.beginPath(); ctx.arc(n.x, n.y, n.r * 4, 0, Math.PI * 2); ctx.fill();
        ctx.fillStyle = n.recalled ? "rgba(255,204,232,0.98)" : "rgba(210,180,255,0.95)";
        ctx.beginPath(); ctx.arc(n.x, n.y, n.r, 0, Math.PI * 2); ctx.fill();
      });

      raf = requestAnimationFrame(layout);
    };
    raf = requestAnimationFrame(layout);
    const ro = new ResizeObserver(() => {});
    try { ro.observe(canvasRef.current!.parentElement!); } catch {}
    return () => { cancelAnimationFrame(raf); ro.disconnect(); };
  }, []);

  return (
    <>
      <div className="pad-header">
        <span className="pad-sigil" style={{ background: "linear-gradient(135deg,var(--violet-hi),var(--orchid))" }} />
        <span className="pad-title">记忆星云</span>
        <span className="pad-sub">MEMORY · NEBULA</span>
      </div>
      <div className="nebula-wrap pad-body" style={{ padding: 0 }}>
        <canvas ref={canvasRef} />
        <div className="nebula-meta">紫晶节点：常规 · 粉晶节点：召回</div>
      </div>
    </>
  );
}