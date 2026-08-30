import { useEffect, useRef, useState } from "react";
import { getStepLog, onStepPush } from "../api";

type Danmaku = { id: number; text: string; lane: number };

let seq = 0;

export default function StepBarrage() {
  const [items, setItems] = useState<Danmaku[]>([]);
  const disposed = useRef(false);

  useEffect(() => {
    // 透明弹幕浮窗：清除全局背景，避免掩盖桌面
    document.body.style.margin = "0";
    document.body.style.background = "transparent";
    document.body.style.overflow = "hidden";
    document.documentElement.style.background = "transparent";

    let unlisten: (() => void) | null = null;

    const push = (text: string) => {
      const t = (text ?? "").trim();
      if (!t || disposed.current) return;
      const id = seq++;
      const lane = Math.floor(Math.random() * 2);
      setItems((prev) => [...prev, { id, text: t, lane }]);
      window.setTimeout(() => {
        setItems((prev) => prev.filter((i) => i.id !== id));
      }, 14000);
    };

    (async () => {
      try {
        const existing = await getStepLog();
        existing.forEach(push);
      } catch {
        /* 忽略未取到历史 */
      }
      unlisten = await onStepPush((text) => push(text));
    })();

    return () => {
      disposed.current = true;
      if (unlisten) unlisten();
    };
  }, []);

  return (
    <div className="step-barrage-stage">
      <div className="step-banner">
        <span className="step-banner-dot" />
        <span className="step-banner-text">白泽正在执行 GUI 自动化任务中，请勿操作</span>
        <span className="step-banner-hint">Ctrl+Shift+F12 可紧急解除</span>
      </div>
      {items.map((it) => (
        <div
          key={it.id}
          className="danmaku-item"
          style={{ top: `${it.lane * 26 + 30}px` }}
        >
          <span className="danmaku-dot" />
          <span className="danmaku-text">{it.text}</span>
        </div>
      ))}
    </div>
  );
}