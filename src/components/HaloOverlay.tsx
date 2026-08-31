import { useEffect, useRef, useState } from "react";
import { getHaloLast, logFrontend, onHaloEvent } from "../api";

/**
 * 目标应用光圈覆盖层：全屏透明穿透置顶窗，渲染两类视觉标记——
 *  - window：目标应用边缘光圈环绕（青色呼吸辉光），提示「这是正在被操作的应用」
 *  - flash ：被点击组件/按钮位置的光环闪烁（琥珀色，1.2s 自动消散）
 * 事件坐标为物理屏幕像素，页面按 devicePixelRatio 换算成 CSS 坐标。
 */

type Rect = { x: number; y: number; w: number; h: number };

type WindowMark = Rect & { title: string; seq: number };
type FlashMark = Rect & { seq: number };

let seq = 0;

export default function HaloOverlay() {
  const [winMark, setWinMark] = useState<WindowMark | null>(null);
  const [flashes, setFlashes] = useState<FlashMark[]>([]);
  const winSeqRef = useRef(0);
  const disposed = useRef(false);

  useEffect(() => {
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

    let unlisten: (() => void) | null = null;
    (async () => {
      void logFrontend(
        `[halo] 覆盖层就绪 dpr=${dpr()} viewport=${window.innerWidth}x${window.innerHeight}`
      );
      // 补拉最近一次目标光圈事件：覆盖窗页面挂载晚于首个 halo-event 时，
      // 订阅前的点亮事件会丢失，这里保证「任务开头那圈青色呼吸光」能显示
      try {
        const last = await getHaloLast();
        if (last && !disposed.current && last.type === "window") {
          const [x, y, w, h] = last.rect ?? [];
          if ([x, y, w, h].every((n) => typeof n === "number")) {
            void logFrontend(`[halo] 补拉最近点亮 ${JSON.stringify(last.rect)}`);
            winSeqRef.current += 1;
            const seqId = winSeqRef.current;
            setWinMark({ ...toCss(x, y, w, h), title: last.title ?? "", seq: seqId });
            window.setTimeout(() => {
              setWinMark((cur) => (cur && cur.seq === seqId ? null : cur));
            }, 60000);
          }
        }
      } catch {
        /* 忽略补拉失败 */
      }
      unlisten = await onHaloEvent((e) => {
        if (disposed.current) return;
        void logFrontend(`[halo] 收到事件 type=${e.type} ${JSON.stringify(e.rect ?? [e.x, e.y])}`);
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
        }
      });
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
    </div>
  );
}
