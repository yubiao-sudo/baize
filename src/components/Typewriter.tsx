import { useEffect, useState } from "react";

// 打字节奏：每 tick 揭示的字数 / 间隔毫秒（数值越大越快）
const CHUNK = 3;
const INTERVAL = 20;

/**
 * 逐字打字机效果：按节奏逐步揭示文本，未完成时显示闪烁光标。
 * 组件挂载后动画一次；text 不变则不重新动画。
 */
export default function Typewriter({
  text,
  onTick,
}: {
  text: string;
  onTick?: () => void;
}) {
  const [count, setCount] = useState(0);
  const done = count >= text.length;

  useEffect(() => {
    setCount(0);
    if (!text) return;
    let raf = 0;
    const start = performance.now();
    const step = (now: number) => {
      // 按经过时间换算应揭示字数，与显示器刷新对齐，帧率波动不影响节奏
      const target = Math.min(text.length, Math.floor((now - start) / INTERVAL) * CHUNK);
      setCount(target);
      onTick?.();
      if (target < text.length) {
        raf = requestAnimationFrame(step);
      }
    };
    raf = requestAnimationFrame(step);
    return () => cancelAnimationFrame(raf);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [text]);

  return (
    <>
      {text.slice(0, count)}
      {!done && <span className="caret">▍</span>}
    </>
  );
}
