import { useEffect, useRef, useState } from "react";
import { getHaloLast, haloOverlayReady, logFrontend, onHaloEvent } from "../api";

/**
 * 目标应用光圈覆盖层：全屏透明穿透置顶窗，渲染三类视觉标记——
 *  - window：目标应用边缘光圈环绕（青色呼吸辉光），提示「这是正在被操作的应用」
 *  - flash ：被点击组件/按钮位置的光环闪烁（琥珀色，1.2s 自动消散）
 *  - rect  ：目标元素定位方框（Browser Use 风格实线选框 + 角标 + 文字标签），
 *            用于「即将点击/输入的到底是哪个元素」的可视化，2.6s 自动消散
 * 事件坐标为物理屏幕像素，页面按 devicePixelRatio 换算成 CSS 坐标。
 */

type Rect = { x: number; y: number; w: number; h: number };

type WindowMark = Rect & { title: string; seq: number };
type FlashMark = Rect & { seq: number };
type BoxMark = Rect & { label: string; seq: number; fading?: boolean };

let seq = 0;

export default function HaloOverlay() {
  const [winMark, setWinMark] = useState<WindowMark | null>(null);
  const [flashes, setFlashes] = useState<FlashMark[]>([]);
  const [boxes, setBoxes] = useState<BoxMark[]>([]);
  const winSeqRef = useRef(0);
  const disposed = useRef(false);

  useEffect(() => {
    disposed.current = false;
    document.body.style.margin = "0";
    document.body.style.background = "transparent";
    document.body.style.overflow = "hidden";
    document.documentElement.style.background = "transparent";

    const dpr = () => window.devicePixelRatio || 1;
    const toCss = (x: number, y: number, w: number, h: number): Rect => ({
      x: x / dpr(),
      y: y / dpr(),
      w: Math.max(w / dpr(), 6),
      h: Math.max(h / dpr(), 6),
    });

    type HaloEvent = {
      type: "window" | "flash" | "rect" | "clear";
      rect?: number[];
      title?: string;
      label?: string;
      x?: number;
      y?: number;
      w?: number;
      h?: number;
    };

    // 统一渲染入口：实时事件与「订阅前被暂存的事件」两条来路共用，避免行为不一致
    const applyEvent = (e: HaloEvent) => {
      if (disposed.current) return;
      if (e.type === "window") {
        const [x, y, w, h] = e.rect ?? [];
        if ([x, y, w, h].some((n) => typeof n !== "number")) return;
        winSeqRef.current += 1;
        const seqId = winSeqRef.current;
        setWinMark({ ...toCss(x, y, w, h), title: e.title ?? "", seq: seqId });
        // 点亮持续 60 秒：任务开头清屏/搜索要花很久，8 秒太短用户永远看不到；
        // 任务结束由 halo_clear/disengage 隐藏覆盖窗，此超时只是兜底
        window.setTimeout(() => {
          setWinMark((cur) => (cur && cur.seq === seqId ? null : cur));
        }, 60000);
      } else if (e.type === "flash") {
        const id = ++seq;
        const x = e.x ?? 0;
        const y = e.y ?? 0;
        const w = e.w ?? 0;
        const h = e.h ?? 0;
        const rect =
          w > 4 && h > 4 ? toCss(x, y, w, h) : { x: x / dpr() - 40, y: y / dpr() - 40, w: 80, h: 80 };
        setFlashes((prev) => [...prev.slice(-5), { ...rect, seq: id }]);
        window.setTimeout(() => {
          setFlashes((prev) => prev.filter((f) => f.seq !== id));
        }, 2200);
      } else if (e.type === "rect") {
        // 元素定位方框：坐标含义有两种载荷（rect 数组优先，兼容 x/y/w/h 分字段）
        const [rx, ry, rw, rh] = e.rect ?? [e.x ?? 0, e.y ?? 0, e.w ?? 0, e.h ?? 0];
        if ([rx, ry, rw, rh].some((n) => typeof n !== "number") || rw <= 0 || rh <= 0) return;
        const id = ++seq;
        const r = toCss(rx, ry, rw, rh);
        setBoxes((prev) => [...prev.slice(-2), { ...r, label: e.label ?? "", seq: id }]);
        // 停留 2.4s 后先淡出再移除（直接移除太突兀；且方框本身不做自动淡出动画，
        // 避免「元素在覆盖窗隐藏期间创建、动画跑完变透明」导致框根本看不见）
        window.setTimeout(() => {
          setBoxes((prev) => prev.map((b) => (b.seq === id ? { ...b, fading: true } : b)));
          window.setTimeout(() => {
            setBoxes((prev) => prev.filter((b) => b.seq !== id));
          }, 260);
        }, 2400);
      }
    };

    let unlisten: (() => void) | null = null;
    (async () => {
      void logFrontend(
        `[halo] 覆盖层就绪 dpr=${dpr()} viewport=${window.innerWidth}x${window.innerHeight}`
      );
      // 顺序很关键：先订阅，再握手。反过来（先握手后订阅）中间窗口期的事件仍会丢。
      unlisten = await onHaloEvent((e) => {
        void logFrontend(`[halo] 实时事件 type=${e.type}`);
        applyEvent(e);
      });
      // 握手：领取覆盖层尚未就绪时后端暂存的事件并重放。
      // 覆盖窗首次创建、页面还在加载时，第一批方框事件必然赶不上订阅——这是
      // 「元素定位看不到框」的根因，靠这步把丢失的事件补回来。
      let replayed = 0;
      try {
        const missed = await haloOverlayReady();
        replayed = missed.length;
        if (replayed) {
          void logFrontend(`[halo] 重放暂存事件 ${replayed} 条`);
          missed.forEach(applyEvent);
        }
      } catch {
        /* 忽略握手失败 */
      }
      // 兜底：暂存队列为空、但更早还点亮过「目标窗口」时用最后一条点亮记录补齐
      if (replayed === 0) {
        try {
          const last = await getHaloLast();
          if (last && last.type === "window") {
            const [x, y, w, h] = last.rect ?? [];
            if ([x, y, w, h].every((n) => typeof n === "number")) {
              void logFrontend(`[halo] 补拉最近点亮 ${JSON.stringify(last.rect)}`);
              applyEvent({ type: "window", rect: [x, y, w, h], title: last.title });
            }
          }
        } catch {
          /* 忽略补拉失败 */
        }
      }
    })();

    return () => {
      disposed.current = true;
      if (unlisten) unlisten();
    };
  }, []);

  return (
    <div className="halo-stage">
      {winMark && (
        <div
          key={`w${winMark.seq}`}
          className="halo-window-ring"
          style={{ left: winMark.x - 4, top: winMark.y - 4, width: winMark.w + 8, height: winMark.h + 8 }}
        >
          {winMark.title && <span className="halo-window-tag">🎯 {winMark.title}</span>}
        </div>
      )}
      {flashes.map((f) => (
        <div
          key={`f${f.seq}`}
          className="halo-flash-ring"
          style={{ left: f.x, top: f.y, width: f.w, height: f.h }}
        />
      ))}
      {boxes.map((b) => (
        <div
          key={`b${b.seq}`}
          className={`halo-target-box${b.fading ? " fading" : ""}`}
          style={{ left: b.x, top: b.y, width: b.w, height: b.h }}
        >
          <i className="halo-target-corner tl" />
          <i className="halo-target-corner tr" />
          <i className="halo-target-corner bl" />
          <i className="halo-target-corner br" />
          {b.label && <span className="halo-target-label">{b.label}</span>}
        </div>
      ))}
    </div>
  );
}
