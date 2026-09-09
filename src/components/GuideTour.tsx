import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { CSSProperties } from "react";

/**
 * 新手引导（Spotlight Tour）：
 * 遮罩压暗全局，仅高亮 data-guide 锚点元素（box-shadow 挖洞法），旁出说明卡。
 * 首次进入主界面自动播放（baize_guide_done 标记），顶栏 ？ 可随时重放。
 * 步骤支持动作前置（自动展开功能菜单/右侧面板，结束恢复原状）；
 * 欢迎页提供典型用法示例，点击直接填入输入框让用户试一把。
 */

/** 引导步骤定义：target 缺省 = 居中卡 */
type Step = {
  target?: string;
  title: string;
  body: string;
  /** 进入该步前自动展开「⋯」功能菜单 */
  expandMenu?: boolean;
  /** 进入该步前自动展开右侧面板 */
  expandRight?: boolean;
  /** 强制说明卡方位（缺省按空间自动判断） */
  prefer?: "top" | "bottom" | "left" | "right";
};

/** 欢迎页示例：点击 → 填入输入框 → 跳到输入框步骤 */
const EXAMPLES = [
  "帮我整理桌面上的文件，按类型分文件夹",
  "搜索今天的 AI 新闻并总结成 5 条要点",
  "每天早上 9 点汇报我的日程安排",
];

const STEPS: Step[] = [
  {
    title: "欢迎使用白泽",
    body: "白泽是会执行任务的桌面智能体：你说需求，它调用终端、浏览器、文件、办公软件等工具直接把事情做完。先看一圈界面，也可以点下面的例子立刻试试。",
  },
  {
    target: "status",
    title: "状态灯",
    body: "顶栏状态灯实时显示白泽正在做什么：待命、思考中、执行工具、朗读中……任务全程心中有数。",
    prefer: "bottom",
  },
  {
    target: "input",
    title: "在这里下指令",
    body: "像和人说话一样描述任务即可，白泽会自己拆解步骤并逐个执行。也支持语音输入——点输入框旁的话筒按钮。",
    prefer: "top",
  },
  {
    target: "chat",
    title: "会话区",
    body: "回答流式呈现，执行过程以步骤卡片实时展示。生成中也可以自由上下滚动查看，鼠标移出会自动收起为一条输入框。",
    prefer: "top",
  },
  {
    target: "sidebar",
    title: "会话与项目",
    body: "左侧管理会话历史与项目空间，点「新会话」随时开新任务，历史会话随时回看。",
    prefer: "right",
  },
  {
    target: "menu",
    title: "功能菜单",
    body: "「⋯」里是白泽的全部能力入口：终端、浏览器、日程、邮件、工作流、任务中心、设置等。",
    expandMenu: true,
    prefer: "bottom",
  },
  {
    target: "rightpanel",
    title: "工作面板",
    body: "右侧面板与任务联动：执行流、记忆星图、文件工作区都在这里，可随时收起给会话腾空间。",
    expandRight: true,
    prefer: "left",
  },
  {
    title: "准备好了",
    body: "随时点顶栏的「?」可以重看本引导。小提示：Ctrl+K 打开命令面板，直呼白泽即可唤起语音。现在，交给白泽吧。",
  },
];

/** 找到锚点元素的中心矩形（不存在时回退屏幕中央） */
function rectOf(target?: string): { x: number; y: number; w: number; h: number; center: boolean } {
  if (target) {
    const el = document.querySelector(`[data-guide="${target}"]`);
    if (el) {
      const r = el.getBoundingClientRect();
      if (r.width > 0 && r.height > 0) {
        return { x: r.left, y: r.top, w: r.width, h: r.height, center: false };
      }
    }
  }
  const w = window.innerWidth;
  const h = window.innerHeight;
  return { x: w / 2 - 20, y: h / 2 - 20, w: 40, h: 40, center: true };
}

export default function GuideTour({
  open,
  onClose,
  setMenuOpen,
  setShowRight,
  showRight,
}: {
  open: boolean;
  onClose: () => void;
  setMenuOpen: (v: boolean) => void;
  setShowRight: (v: boolean) => void;
  showRight: boolean;
}) {
  const [idx, setIdx] = useState(0);
  const [rect, setRect] = useState<{ x: number; y: number; w: number; h: number; center: boolean }>(() =>
    rectOf(undefined),
  );
  const restoreRef = useRef<{ menu: boolean; right: boolean } | null>(null);
  const step = STEPS[Math.min(idx, STEPS.length - 1)];

  // 每步：执行动作前置 + 重新量取高亮位置（延迟两帧+动作动画后再量一次，保证布局稳定）
  useLayoutEffect(() => {
    if (!open) return;
    const s = STEPS[Math.min(idx, STEPS.length - 1)];
    if (s.expandMenu) setMenuOpen(true);
    if (s.expandRight) setShowRight(true);
    let t2: number | undefined;
    const measure = () => setRect(rectOf(s.target));
    requestAnimationFrame(() => {
      measure();
      t2 = window.setTimeout(measure, 380);
    });
    return () => {
      if (t2) clearTimeout(t2);
    };
  }, [idx, open, setMenuOpen, setShowRight]);

  // 进入/退出引导：记录并恢复菜单与右面板状态；Esc 退出；窗口变化重新量取
  useEffect(() => {
    if (!open) return;
    // 记录进入前的原始状态（此刻 idx=0，动作前置尚未执行）
    restoreRef.current = { menu: false, right: showRight };
    setIdx(0);
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") close();
    };
    const onResize = () => setRect(rectOf(STEPS[Math.min(idx, STEPS.length - 1)].target));
    window.addEventListener("keydown", onKey);
    window.addEventListener("resize", onResize);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("resize", onResize);
      // 引导结束：收回动作前置展开的菜单/面板（恢复进入前的原始状态）
      setMenuOpen(false);
      const r = restoreRef.current;
      if (r) setShowRight(r.right);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  const close = useCallback(() => {
    localStorage.setItem("baize_guide_done", "1");
    onClose();
  }, [onClose]);

  if (!open) return null;

  const next = () => {
    if (idx + 1 >= STEPS.length) close();
    else setIdx(idx + 1);
  };
  const prev = () => setIdx(Math.max(0, idx - 1));

  const onExample = (text: string) => {
    window.dispatchEvent(new CustomEvent("baize:guide-fill", { detail: { text } }));
    setIdx(2); // 跳到「输入框」步骤，让用户看到文字已就位
  };

  // 说明卡定位：中心卡居中；锚点卡按剩余空间择优（用户指定优先）
  const pad = 14;
  const vw = window.innerWidth;
  const vh = window.innerHeight;
  const isCenter = rect.center || !step.target;
  let cardStyle: CSSProperties = {};
  let place: Step["prefer"] = step.prefer;
  if (!isCenter) {
    const space = {
      top: rect.y,
      bottom: vh - rect.y - rect.h,
      left: rect.x,
      right: vw - rect.x - rect.w,
    };
    if (!place) {
      place = space.bottom > 220 ? "bottom" : space.top > 220 ? "top" : space.right > 340 ? "right" : "left";
    }
    if (place === "top") {
      cardStyle = { left: Math.min(Math.max(rect.x + rect.w / 2 - 170, 12), vw - 352), top: Math.max(rect.y - pad - 168, 12) };
    } else if (place === "bottom") {
      cardStyle = { left: Math.min(Math.max(rect.x + rect.w / 2 - 170, 12), vw - 352), top: rect.y + rect.h + pad };
    } else if (place === "right") {
      cardStyle = { left: rect.x + rect.w + pad, top: Math.min(Math.max(rect.y - 40, 12), vh - 220) };
    } else {
      cardStyle = { left: Math.max(rect.x - pad - 340, 12), top: Math.min(Math.max(rect.y - 40, 12), vh - 220) };
    }
  }

  return (
    <div className="guide-root" role="dialog" aria-label="新手引导">
      {/* 高亮挖洞层：box-shadow 无限扩散形成遮罩，洞口跟随目标 */}
      <div
        className={`guide-hole ${isCenter ? "center" : ""}`}
        style={
          isCenter
            ? undefined
            : { left: rect.x - 6, top: rect.y - 6, width: rect.w + 12, height: rect.h + 12 }
        }
        onClick={next}
      />
      {isCenter ? (
        <div className="guide-card center" key={`c${idx}`}>
          <div className="guide-brand" aria-hidden="true">泽</div>
          <h3>{step.title}</h3>
          <p>{step.body}</p>
          {idx === 0 && (
            <div className="guide-examples">
              {EXAMPLES.map((t) => (
                <button key={t} onClick={() => onExample(t)}>
                  <span className="ex-icon" aria-hidden="true">✦</span>
                  {t}
                </button>
              ))}
            </div>
          )}
          <div className="guide-nav">
            {idx > 0 && (
              <button className="g-btn" onClick={prev}>
                上一步
              </button>
            )}
            <button className="g-btn ghost" onClick={close}>
              跳过
            </button>
            <button className="g-btn primary" onClick={next}>
              {idx === 0 ? "开始导览" : idx === STEPS.length - 1 ? "完成" : "下一步"}
            </button>
          </div>
          <div className="guide-progress">
            {STEPS.map((_, i) => (
              <span key={i} className={i === idx ? "on" : ""} />
            ))}
          </div>
        </div>
      ) : (
        <div className="guide-card" style={cardStyle} key={`a${idx}`}>
          <h3>{step.title}</h3>
          <p>{step.body}</p>
          <div className="guide-nav">
            {idx > 0 && (
              <button className="g-btn" onClick={prev}>
                上一步
              </button>
            )}
            <button className="g-btn ghost" onClick={close}>
              跳过
            </button>
            <button className="g-btn primary" onClick={next}>
              {idx + 1 === STEPS.length - 1 ? "最后一步" : idx + 1 === STEPS.length ? "完成" : "下一步"}
            </button>
          </div>
          <span className="guide-count">
            {idx + 1} / {STEPS.length}
          </span>
        </div>
      )}
    </div>
  );
}
