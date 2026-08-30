/**
 * 银河背景（地表视角的「星河」，纯观赏动效，无数据、不响应交互）：
 * 一条像河流一样横贯天幕的发光星带——拱形曲线跨越屏幕，
 * 沿带高斯分布的密集星点 + 中央暗尘带（Great Rift）+ 星云辉光斑块，
 * 偶有流星划过。独立于水球律动。
 */

import { useEffect, useRef } from "react";

/** 近似高斯随机（三次均匀随机求和居中） */
const gauss = () => Math.random() + Math.random() + Math.random() - 1.5;

// ── 辉光精灵缓存：每色预渲染一张径向渐变小图，运行期 drawImage + globalAlpha 合成 ──
// 星空每帧原本要创建 ~250 个 CanvasGradient（光斑/星云/星晕/尘云），是 CPU 大头；
// 换成精灵后渐变创建为 0，只剩廉价的 drawImage 位块传输。
const glowCache = new Map<string, HTMLCanvasElement>();
function glowSprite(color: string): HTMLCanvasElement {
  let c = glowCache.get(color);
  if (!c) {
    c = document.createElement("canvas");
    c.width = c.height = 128;
    const g = c.getContext("2d");
    if (g) {
      const grad = g.createRadialGradient(64, 64, 0, 64, 64, 64);
      grad.addColorStop(0, `rgba(${color},1)`);
      grad.addColorStop(1, `rgba(${color},0)`);
      g.fillStyle = grad;
      g.fillRect(0, 0, 128, 128);
    }
    glowCache.set(color, c);
  }
  return c;
}
function drawGlow(
  ctx: CanvasRenderingContext2D,
  color: string,
  x: number,
  y: number,
  r: number,
  alpha: number,
) {
  if (alpha <= 0 || r <= 0) return;
  ctx.globalAlpha = Math.min(1, alpha);
  ctx.drawImage(glowSprite(color), x - r, y - r, r * 2, r * 2);
}

interface BandStar {
  s: number; // 沿带参数 0..1
  off: number; // 垂直于带方向的偏移（px）
  sz: number;
  base: number;
  tw: number;
  ph: number;
  col: string; // 暗色主题星色（亮色系）
  colLight: string; // 浅色主题星色（暗色系，浅底上可见）
  dim: number; // 暗尘带压暗系数
  halo?: string; // 亮星彩色光晕（群星堆积处的「多彩星」）
  drift: number; // 沿河漂移速度（s/秒），极缓慢的「看着像在动又没动」
}

/** 暗色星色 → 浅色主题下的对应暗色（浅底上可见） */
const LIGHT_STAR_COLORS: Record<string, string> = {
  "255,226,188": "186,134,76",
  "255,240,220": "198,146,98",
  "232,240,255": "88,112,172",
  "150,205,250": "60,112,192",
  "172,152,255": "110,90,202",
  "255,214,170": "192,122,72",
};

interface SkyStar {
  x: number;
  y: number;
  sz: number;
  base: number;
  tw: number;
  ph: number;
}

interface Nebula {
  s: number;
  off: number;
  r: number;
  col: string;
  a: number;
  ph: number;
  drift: number; // 沿河蠕动速度（s/秒）
}

interface Meteor {
  x: number;
  y: number;
  vx: number;
  vy: number;
  age: number;
  life: number;
}

/** 暗星云：不发光的乌黑尘云，贴河中轴成团分布，遮挡身后星光（Great Rift 效果） */
interface DarkNebula {
  s: number;
  off: number;
  a: number;
  ph: number;
  drift: number;
  /** 子气团：沿河切向拉长排布，形成「棉絮状」云形 */
  blobs: { dx: number; dy: number; r: number; k: number }[];
}

export default function Galaxy() {
  const ref = useRef<HTMLCanvasElement | null>(null);

  useEffect(() => {
    const canvas = ref.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    let raf = 0;
    let last = performance.now();
    let t = 0;
    let w = 0;
    let h = 0;

    // ── 主题感知：浅色毛玻璃（light-glass）下整套配色换暗色系，黑尘云/暖光停用 ──
    // html 的 data-theme 由命令面板切换，这里用 MutationObserver 跟随
    let themeLight = document.documentElement.dataset.theme === "light-glass";
    const themeMo = new MutationObserver(() => {
      themeLight = document.documentElement.dataset.theme === "light-glass";
    });
    themeMo.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["data-theme"],
    });

    // ── 星带粒子：直线星河 + 高斯横向散布 + 中央暗尘带 ──
    const band: BandStar[] = [];
    const N = 2600;
    for (let i = 0; i < N; i++) {
      const s = Math.random();
      const mid = 1 - Math.abs(s - 0.5) * 2; // 带中部更宽更密
      const sigma = 26 + 74 * mid;
      const off = gauss() * sigma;
      // 暗尘带：贴近带中线的大部分星点被压暗/剔除，形成「河流中的暗滩」
      const inRift = Math.abs(off) < sigma * 0.3;
      const dim = inRift ? (Math.random() < 0.72 ? 0 : 0.18) : 1;
      if (dim === 0) continue;
      const bulge = Math.abs(s - 0.46) < 0.12; // 银心方向更亮更密更暖
      const k = Math.random();
      const col = bulge
        ? k < 0.5
          ? "255,226,188"
          : "255,240,220"
        : k < 0.55
          ? "232,240,255"
          : k < 0.8
            ? "150,205,250"
            : k < 0.95
              ? "172,152,255"
              : "255,214,170";
      // 漂移视差：越贴近河中轴（视为越近）漂得越快，全程需 10 分钟级——「像在动又没动」
      const depth = 1 - Math.min(1, Math.abs(off) / 150);
      band.push({
        s,
        off,
        sz: (bulge ? rand(0.7, 1.9) : rand(0.4, 1.4)) * dpr,
        base: (bulge ? 0.85 : 0.65) * rand(0.45, 1),
        tw: rand(0.3, 1.4),
        ph: rand(0, Math.PI * 2),
        col,
        colLight: LIGHT_STAR_COLORS[col] ?? "90,110,170",
        dim,
        // 少数亮星带彩色光晕（蓝/金/粉/紫），让星群堆积处呈现多彩
        halo:
          !inRift && Math.random() < 0.05
            ? ["110,190,255", "255,200,120", "255,150,190", "180,140,255", "90,230,210"][
                Math.floor(Math.random() * 5)
              ]
            : undefined,
        drift: 0.0005 + 0.0022 * depth,
      });
    }

    // ── 全天散星：满天繁星（参考实拍星野，银河之外的天空也有大量星点） ──
    const sky: SkyStar[] = Array.from({ length: 800 }, () => ({
      x: Math.random(),
      y: Math.random(),
      sz: rand(0.35, 1.0) * dpr,
      base: rand(0.12, 0.5),
      tw: rand(0.2, 1.1),
      ph: rand(0, Math.PI * 2),
    }));

    // ── 星云辉光：沿带分布的柔和色斑，中段（银心方向）更密集——亮核的「斑驳云气」 ──
    // 多彩发射星云配色：H-α 红粉 / OIII 青 / 紫电蓝 / 橙金 / 冰蓝
    const nebCols = [
      "90,140,255", "140,90,255", "80,200,230", "120,160,255", "200,220,255",
      "255,120,170", "80,255,210", "255,140,90", "170,120,255", "255,190,150",
    ];
    const nebulas: Nebula[] = Array.from({ length: 22 }, (_, i) => {
      // 中段 0.28-0.72 更密，两端稀疏
      const mid = i < 15;
      const s = mid ? 0.16 + Math.random() * 0.68 : 0.06 + Math.random() * 0.88;
      return {
        s,
        off: gauss() * 44,
        r: rand(65, 190),
        col: nebCols[i % nebCols.length],
        a: rand(0.04, mid ? 0.095 : 0.07),
        ph: rand(0, Math.PI * 2),
        drift: rand(0.0003, 0.001),
      };
    });

    const meteors: Meteor[] = [];

    // ── 暗星云：乌黑尘云，从银河中段向两侧扩散成棉絮状，遮挡身后的光 ──
    const darkNebulas: DarkNebula[] = Array.from({ length: 9 }, (_, i) => {
      const nBlobs = 4 + Math.floor(Math.random() * 4); // 4-7 个子气团
      return {
        s: 0.18 + (i / 8) * 0.64 + rand(-0.05, 0.05), // 中段为主，向两端扩散
        off: gauss() * 20, // 紧贴河中轴
        a: rand(0.38, 0.62),
        ph: rand(0, Math.PI * 2),
        drift: rand(0.0002, 0.0007),
        blobs: Array.from({ length: nBlobs }, (_, j) => ({
          // 沿河切向拉长（dx 大 dy 小），像被「河风吹开」的黑色棉絮
          dx: (j - nBlobs / 2) * rand(26, 58) + rand(-10, 10),
          dy: rand(-18, 18),
          r: rand(46, 112),
          k: rand(0.55, 1),
        })),
      };
    });

    function rand(a: number, b: number) {
      return a + Math.random() * (b - a);
    }

    // 星河路径：左下 → 右上的一条直线（控制点取中点，贝塞尔退化为直线）
    let p0 = [0, 0];
    let p1 = [0, 0];
    let p2 = [0, 0];
    const rebuildCurve = () => {
      p0 = [-0.05 * w, 0.96 * h];
      p1 = [0.5 * w, 0.5 * h];
      p2 = [1.05 * w, 0.04 * h];
    };
    const bez = (tt: number) => {
      const u = 1 - tt;
      return [
        u * u * p0[0] + 2 * u * tt * p1[0] + tt * tt * p2[0],
        u * u * p0[1] + 2 * u * tt * p1[1] + tt * tt * p2[1],
      ];
    };
    const bezNormal = (tt: number) => {
      const e = 0.01;
      const a = bez(Math.max(0, tt - e));
      const b = bez(Math.min(1, tt + e));
      const dx = b[0] - a[0];
      const dy = b[1] - a[1];
      const len = Math.hypot(dx, dy) || 1;
      return [-dy / len, dx / len];
    };

    const resize = () => {
      const el = canvas.parentElement;
      if (!el) return;
      w = el.clientWidth;
      h = el.clientHeight;
      canvas.width = Math.max(1, Math.round(w * dpr));
      canvas.height = Math.max(1, Math.round(h * dpr));
      canvas.style.width = `${w}px`;
      canvas.style.height = `${h}px`;
      rebuildCurve();
    };
    resize();
    window.addEventListener("resize", resize);

    // 性能：30fps 限帧（星空动效足够顺滑，渲染量减半）+ 页面隐藏时整循环暂停
    const FRAME_MS = 33;
    let paused = false;
    const onVisibility = () => {
      paused = document.hidden;
      if (paused) {
        cancelAnimationFrame(raf);
      } else {
        last = performance.now();
        raf = requestAnimationFrame(frame);
      }
    };
    document.addEventListener("visibilitychange", onVisibility);

    const frame = (now: number) => {
      raf = requestAnimationFrame(frame);
      if (now - last < FRAME_MS) return; // 限帧：距上帧不足 33ms 直接跳过
      const dt = Math.min(now - last, 50) / 1000;
      last = now;
      t += dt;

      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, w, h);

      // ── 银河光晕：沿河的微蓝色雾状辉光（精灵合成，不再每帧建渐变） ──
      // 1) 银心区域的大范围环境光（每帧仅 1 个渐变，保留）
      const [gx, gy] = bez(0.46);
      const amb = ctx.createRadialGradient(gx, gy, 0, gx, gy, Math.max(w, h) * 0.42);
      amb.addColorStop(0, "rgba(120,170,255,0.012)");
      amb.addColorStop(0.5, "rgba(100,150,255,0.005)");
      amb.addColorStop(1, "rgba(100,150,255,0)");
      ctx.fillStyle = amb;
      ctx.fillRect(0, 0, w, h);
      // 2) 沿河铺设软边光斑——精灵版（无硬边，中心密两端稀）
      const breathe = 0.92 + 0.08 * Math.sin(t * 0.12); // 极缓慢的呼吸
      for (let s = 0.02; s <= 0.99; s += 0.018) {
        const [bx, by] = bez(s);
        const midK = Math.max(0, 1 - Math.abs(s - 0.5) * 1.3);
        const radius = 140 + 220 * midK;
        const a = (0.002 + 0.005 * midK) * breathe;
        drawGlow(ctx, "150,195,255", bx, by, radius, a);
      }
      ctx.globalAlpha = 1;

      // ── 地平线暖光：底部大气辉光/远景光害的暖色渐变（参考实拍星野） ──
      // 浅色主题下跳过：浅底上会显成一块脏橙色
      if (!themeLight) {
        const hz = ctx.createLinearGradient(0, h, 0, h * 0.68);
        hz.addColorStop(0, "rgba(196,116,64,0.11)");
        hz.addColorStop(0.45, "rgba(160,95,60,0.05)");
        hz.addColorStop(1, "rgba(160,95,60,0)");
        ctx.fillStyle = hz;
        ctx.fillRect(0, h * 0.68, w, h * 0.32);
      }

      // ── 星云辉光（沿河蠕动 + 缓慢漂移）——精灵合成，浅色下减半 ──
      for (const n of nebulas) {
        n.s += n.drift * dt;
        if (n.s > 1.02) n.s -= 1.04;
        const [bx, by] = bez(Math.min(1, Math.max(0, n.s)));
        const [nx, ny] = bezNormal(n.s);
        const sway = Math.sin(t * 0.05 + n.ph) * 5;
        const x = bx + nx * (n.off + sway);
        const y = by + ny * (n.off + sway);
        drawGlow(ctx, n.col, x, y, n.r, n.a * (themeLight ? 0.5 : 1));
      }

      // ── 星带星点（沿河极缓慢流动，视差分层） ──
      for (const st of band) {
        st.s += st.drift * dt;
        if (st.s > 1.02) st.s -= 1.04;
        const sc = Math.min(1, Math.max(0, st.s));
        const [bx, by] = bez(sc);
        const [nx, ny] = bezNormal(sc);
        const x = bx + nx * st.off;
        const y = by + ny * st.off;
        const tw = 0.55 + 0.45 * Math.sin(t * st.tw + st.ph);
        // 亮星彩色光晕：精灵合成（浅色下停用，浅底上会发灰）
        if (st.halo && !themeLight) {
          drawGlow(ctx, st.halo, x, y, st.sz * 7, 0.05 * st.base * st.dim * tw);
        }
        ctx.globalAlpha = st.base * st.dim * tw * (themeLight ? 0.75 : 1);
        ctx.fillStyle = themeLight ? `rgb(${st.colLight})` : `rgb(${st.col})`;
        ctx.fillRect(x - st.sz / 2, y - st.sz / 2, st.sz, st.sz);
      }

      // ── 暗星云：乌黑棉絮状尘云，压在辉光与星带之上（精灵合成） ──
      // 浅色主题下停用：黑色尘云画在浅底上会显成一片片黑斑（异常的根源）
      if (!themeLight) {
        for (const dn of darkNebulas) {
          dn.s += dn.drift * dt;
          if (dn.s > 1.02) dn.s -= 1.04;
          const sc = Math.min(1, Math.max(0, dn.s));
          const [bx, by] = bez(sc);
          const [nx, ny] = bezNormal(sc);
          // 整团极缓慢的呼吸（几乎察觉不到）
          const breatheK = 0.8 + 0.2 * Math.sin(t * 0.03 + dn.ph);
          // 切向单位向量（沿河方向），用于把子气团排成拉长的云形
          const [tx, ty] = [ny, -nx];
          for (const b of dn.blobs) {
            const x = bx + nx * dn.off + tx * b.dx;
            const y = by + ny * dn.off + ty * b.dx + ny * b.dy;
            drawGlow(ctx, "7,9,16", x, y, b.r, dn.a * b.k * breatheK);
          }
        }
        ctx.globalAlpha = 1;
      }

      // ── 全天散星（浅色下用暗蓝灰，浅底上才可见） ──
      for (const ss of sky) {
        ctx.globalAlpha = ss.base * (0.5 + 0.5 * Math.sin(t * ss.tw + ss.ph));
        ctx.fillStyle = themeLight ? "rgb(96,116,176)" : "rgb(220,230,255)";
        ctx.fillRect(ss.x * w, ss.y * h, ss.sz, ss.sz);
      }
      ctx.globalAlpha = 1;

      // ── 流星：随机间隔生成，斜向划过自熄 ──
      if (Math.random() < dt * 0.14 && meteors.length < 2) {
        const fromLeft = Math.random() < 0.5;
        const ang = (fromLeft ? rand(0.35, 0.6) : Math.PI - rand(0.35, 0.6));
        const speed = rand(520, 820);
        meteors.push({
          x: rand(0.15, 0.85) * w,
          y: rand(0.05, 0.45) * h,
          vx: Math.cos(ang) * speed,
          vy: Math.sin(ang) * speed,
          age: 0,
          life: rand(0.7, 1.1),
        });
      }
      for (let i = meteors.length - 1; i >= 0; i--) {
        const m = meteors[i];
        m.age += dt;
        if (m.age >= m.life) {
          meteors.splice(i, 1);
          continue;
        }
        m.x += m.vx * dt;
        m.y += m.vy * dt;
        const k = m.age / m.life;
        const alpha = Math.sin(k * Math.PI); // 淡入淡出
        const tail = 110;
        const tailCol = themeLight ? "80,120,210" : "220,240,255";
        const g = ctx.createLinearGradient(m.x, m.y, m.x - m.vx * (tail / 700), m.y - m.vy * (tail / 700));
        g.addColorStop(0, `rgba(${tailCol},${(themeLight ? 0.55 : 0.85) * alpha})`);
        g.addColorStop(1, `rgba(${tailCol},0)`);
        ctx.strokeStyle = g;
        ctx.lineWidth = 1.4 * dpr;
        ctx.beginPath();
        ctx.moveTo(m.x, m.y);
        ctx.lineTo(m.x - m.vx * (tail / 700), m.y - m.vy * (tail / 700));
        ctx.stroke();
      }
    };
    raf = requestAnimationFrame(frame);

    return () => {
      cancelAnimationFrame(raf);
      window.removeEventListener("resize", resize);
      document.removeEventListener("visibilitychange", onVisibility);
      themeMo.disconnect();
    };
  }, []);

  return <canvas ref={ref} className="galaxy-bg" aria-hidden />;
}
