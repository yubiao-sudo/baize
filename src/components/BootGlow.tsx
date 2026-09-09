import { useEffect, useState } from "react";

/**
 * 启动流光：白泽进入桌面时，在侧边卡片上播放一次的点火动画。
 *
 * 编排（总时长约 3.9s，纯 CSS keyframes 延迟衔接）：
 *  1. 点燃上升（0 → 1.5s）：一层光幕从卡片底部向上烧过，遮罩扫过整张卡片；
 *  2. 顶部汇聚（1.2 → 2.4s）：光头抵达顶边，在顶边中央凝成一个亮点；
 *  3. 两端分散（1.6 → 2.3s）：亮点沿顶边向左右两端铺开成一条光带；
 *  4. 沿边下行（2.25 → 3.35s）：光从两个上角出发，沿左右边框向下流淌；
 *  5. 底部消散（3.35 → 3.9s）：光头流出底边，整体余晖淡出。
 *
 * 起播时机：主窗口以 visible=false 创建、首帧上屏后才 show，且 index.html 的
 * #splash 启动遮罩（打字 + 水球交接）还要停留 2~3s——CSS 动画在元素挂载瞬间
 * 就开始计时，安装版上等遮罩揭幕时 3.9s 早已播完（症状：启动看不到流光）。
 * 因此这里轮询等待「窗口可见 + 遮罩已移除」再挂载动画元素，12s 兜底强制起播。
 *
 * 配色跟随设置页的流光样式（localStorage baize_glow_style → g-* 类）。
 * 播放完毕后组件自卸载，不留任何常驻开销；StrictMode 双挂载只是重播一遍，无副作用。
 */
export default function BootGlow() {
  const [started, setStarted] = useState(false);
  const [alive, setAlive] = useState(true);

  // 等待「真正可见」：窗口 show 完成 + splash 遮罩移除
  useEffect(() => {
    let t1 = 0;
    const iv = window.setInterval(() => {
      if (document.visibilityState === "visible" && !document.getElementById("splash")) {
        window.clearInterval(iv);
        t1 = window.setTimeout(() => setStarted(true), 500); // 留出水球交接动画的缓冲
      }
    }, 120);
    const cap = window.setTimeout(() => {
      window.clearInterval(iv);
      setStarted(true);
    }, 12000);
    return () => {
      window.clearInterval(iv);
      window.clearTimeout(t1);
      window.clearTimeout(cap);
    };
  }, []);

  useEffect(() => {
    if (!started) return;
    const t = window.setTimeout(() => setAlive(false), 4300);
    return () => window.clearTimeout(t);
  }, [started]);

  if (!started || !alive) return null;
  const style = localStorage.getItem("baize_glow_style") || "aurora";
  const cls = style === "aurora" ? "" : ` g-${style}`;

  return (
    <div className={`side-boot-glow${cls}`} aria-hidden>
      <div className="bg-rise" />
      <div className="top-node" />
      <div className="top-spread" />
      <div className="side-l" />
      <div className="side-r" />
    </div>
  );
}
