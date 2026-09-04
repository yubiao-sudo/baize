/**
 * 全局微交互特效（一次性 init，事件驱动，零轮询）：
 *  - 引力波涟漪：任意按钮点击处双同心环扩散（委托监听，不逐个绑按钮）
 *  - 复制小星星：baize:sparkle 事件 → 从触发点飞向银河高天
 *  - 星尘消散：spawnDust 以元素为源爆出微粒（Onboarding 收场等）
 * 均为 fixed 定位临时 DOM，动画结束即移除，不进 React 树。
 */

let inited = false;

export function initFx() {
  if (inited) return;
  inited = true;

  // 引力波涟漪：捕获阶段委托，命中 button/.btn/[role=button] 即生成双环
  document.addEventListener(
    "pointerdown",
    (e) => {
      const hit = (e.target as HTMLElement | null)?.closest?.(
        "button, .btn, [role='button']"
      );
      if (!hit) return;
      spawnWave(e.clientX, e.clientY);
    },
    true
  );

  // 复制成功小星星（markdown.ts 代码块复制按钮派发）
  window.addEventListener("baize:sparkle", (e) => {
    const { x, y } = (e as CustomEvent<{ x: number; y: number }>).detail ?? {};
    if (typeof x === "number" && typeof y === "number") spawnStar(x, y);
  });
}

/** 双同心引力波 */
export function spawnWave(x: number, y: number) {
  for (const cls of ["", "w2"]) {
    const el = document.createElement("span");
    el.className = `grav-wave ${cls}`.trim();
    el.style.left = `${x}px`;
    el.style.top = `${y}px`;
    document.body.appendChild(el);
    window.setTimeout(() => el.remove(), 850);
  }
}

/** 小星星从 (x,y) 飞向右上（银河方向） */
export function spawnStar(x: number, y: number) {
  const el = document.createElement("span");
  el.className = "copy-star";
  el.style.left = `${x}px`;
  el.style.top = `${y}px`;
  el.style.setProperty("--sx", `${Math.min(320, window.innerWidth * 0.28)}px`);
  el.style.setProperty("--sy", `${-Math.min(360, window.innerHeight * 0.35)}px`);
  document.body.appendChild(el);
  window.setTimeout(() => el.remove(), 950);
}

const DUST_COLORS = [
  "rgb(34, 211, 238)",
  "rgb(129, 140, 248)",
  "rgb(244, 114, 182)",
  "rgb(251, 191, 36)",
];

/** 星尘消散：以元素区域为源爆出 n 颗彩粒向外飘散 */
export function spawnDust(el: HTMLElement, n = 16) {
  const r = el.getBoundingClientRect();
  const cx = r.left + r.width / 2;
  const cy = r.top + r.height / 2;
  for (let i = 0; i < n; i++) {
    const d = document.createElement("span");
    d.className = "dust";
    const ang = Math.random() * Math.PI * 2;
    const dist = 70 + Math.random() * 170;
    d.style.left = `${cx + (Math.random() - 0.5) * r.width * 0.7}px`;
    d.style.top = `${cy + (Math.random() - 0.5) * r.height * 0.7}px`;
    d.style.setProperty("--dx", `${Math.cos(ang) * dist}px`);
    d.style.setProperty("--dy", `${Math.sin(ang) * dist - 40}px`);
    d.style.setProperty("--dust-col", DUST_COLORS[i % DUST_COLORS.length]);
    document.body.appendChild(d);
    window.setTimeout(() => d.remove(), 950);
  }
}
