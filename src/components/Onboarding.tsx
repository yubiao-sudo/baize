import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  clipboardSetText,
  envDetectAll,
  envGetState,
  envSetOnboarding,
  onEnvCheckItem,
  openDataFolder,
} from "../api";
import type { EnvItem } from "../types";
import { spawnDust } from "../utils/fx";

/**
 * 首次启动环境自检（全屏引导层）+ 非首次启动的环境提示卡。
 *
 * 流程：挂载即调 env_detect_all，后端逐项 emit baize:env-check → 分组逐项出结论；
 * 环形进度 + 扫描光束 + 检测项点亮闪光；硬件项（CPU/内存/显卡/WebView2）为真实实测；
 * 数据绑定项展示「安装目录\data」实际落盘路径，可一键打开。
 * 全部必需项通过 → 「进入白泽」；有缺失 → 修复指引（复制命令）+「仍然进入」软拦截；
 * Esc 可跳过。完成/跳过都写 onboarding_done，后续启动不再弹全屏。
 */

/** 检测项清单（id 与后端 environment.rs 一致；顺序决定分组内展示顺序） */
const EXPECTED: { id: string; name: string; level: EnvItem["level"] }[] = [
  { id: "powershell", name: "Windows PowerShell", level: "required" },
  { id: "hardware", name: "处理器与内存", level: "info" },
  { id: "gpu", name: "显卡", level: "info" },
  { id: "network", name: "网络连通", level: "required" },
  { id: "webview2", name: "WebView2 运行时", level: "info" },
  { id: "datadir", name: "数据目录绑定", level: "required" },
  { id: "ocr", name: "Windows OCR 引擎", level: "required" },
  { id: "disk", name: "磁盘空间", level: "required" },
  { id: "admin", name: "运行权限", level: "info" },
  { id: "python", name: "Python", level: "optional" },
  { id: "kokoro", name: "Kokoro 本地语音", level: "optional" },
  { id: "tesseract", name: "Tesseract OCR", level: "optional" },
  { id: "audio", name: "音频设备", level: "optional" },
  { id: "node", name: "Node.js", level: "optional" },
  { id: "git", name: "Git", level: "optional" },
];

/** 检测分组：按「核心 → 硬件实测 → 数据绑定 → 增强」分区展示 */
const GROUPS: { title: string; icon: string; ids: string[] }[] = [
  { title: "核心环境", icon: "◈", ids: ["powershell", "network", "ocr", "disk"] },
  { title: "硬件实测", icon: "▣", ids: ["hardware", "gpu", "webview2", "admin"] },
  { title: "数据绑定", icon: "⛁", ids: ["datadir"] },
  {
    title: "增强能力",
    icon: "✦",
    ids: ["python", "kokoro", "tesseract", "audio", "node", "git"],
  },
];

const STATUS_ICON: Record<string, string> = { ok: "✓", warn: "!", missing: "✕" };
const STATUS_LABEL: Record<string, string> = { ok: "通过", warn: "注意", missing: "缺失" };

/** 生成检测行展示用的次要文本：结论 → 厂商 → 状态名 */
function rowSub(it: EnvItem | null, fallback: string): string {
  if (!it) return "检测中…";
  return it.detail || it.vendor || fallback;
}

export default function Onboarding({
  onFinish,
}: {
  onFinish: (status: "done" | "skipped") => void;
}) {
  const [items, setItems] = useState<Record<string, EnvItem>>({});
  const [phase, setPhase] = useState<"running" | "done">("running");
  const [copied, setCopied] = useState("");
  const onFinishRef = useRef(onFinish);
  onFinishRef.current = onFinish;

  const start = useCallback(() => {
    setItems({});
    setPhase("running");
    // 结束以 resolve 为准（事件只负责逐项刷新 UI）；失败也进入完成态，避免卡住
    envDetectAll()
      .then(() => setPhase("done"))
      .catch(() => setPhase("done"));
  }, []);

  useEffect(() => {
    let off: (() => void) | undefined;
    onEnvCheckItem((it) => setItems((m) => ({ ...m, [it.id]: it }))).then((f) => (off = f));
    start();
    return () => off?.();
  }, [start]);

  const finish = useCallback((s: "done" | "skipped") => {
    envSetOnboarding(s)
      .catch(() => {})
      .finally(() => onFinishRef.current(s));
  }, []);

  // 星尘消散收场：卡片模糊缩小淡出 + 从卡片爆出一把彩粒，再交给 finish 真正关闭
  const cardRef = useRef<HTMLDivElement>(null);
  const finishingRef = useRef(false);
  const finishFx = useCallback(
    (s: "done" | "skipped") => {
      if (finishingRef.current) return;
      finishingRef.current = true;
      const card = cardRef.current;
      if (card) {
        card.classList.add("dissolve");
        spawnDust(card, 20);
        window.setTimeout(() => finish(s), 430);
      } else {
        finish(s);
      }
    },
    [finish]
  );

  // Esc 跳过引导
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") finishFx("skipped");
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [finishFx]);

  const get = (id: string) => items[id] ?? null;
  const received = Object.keys(items).length;
  const total = EXPECTED.length;
  const done = phase === "done";
  // 是否完整收齐了所有检测项（检测被并发拦截/失败时未收齐，不能误判「就绪」）
  const complete = done && received >= total;
  const requiredMissing = EXPECTED.filter((e) => {
    const it = items[e.id];
    return it && e.level === "required" && it.status === "missing";
  }).map((e) => items[e.id]);
  const optionalMissing = EXPECTED.filter((e) => {
    const it = items[e.id];
    return it && e.level === "optional" && it.status !== "ok";
  }).map((e) => items[e.id]);

  // 卡片超出 86vh 时内容会被裁掉：检测进行中自动钉底跟随（用户上翻即暂停），
  // 检测完成后再平滑滚到底，确保修复提示与底部按钮可见。
  const stickRef = useRef(true);
  useEffect(() => {
    const el = cardRef.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      const el = cardRef.current;
      if (!el) return;
      if (e.deltaY < 0) stickRef.current = false; // 用户主动上翻才解锁
      else if (el.scrollHeight - el.scrollTop - el.clientHeight < 60) stickRef.current = true; // 滚回底部重新跟随
    };
    el.addEventListener("wheel", onWheel, { passive: true });
    return () => el.removeEventListener("wheel", onWheel);
  }, []);
  useEffect(() => {
    const el = cardRef.current;
    if (el && stickRef.current) el.scrollTop = el.scrollHeight;
  }, [received]);
  useEffect(() => {
    if (!done) return;
    const el = cardRef.current;
    if (!el) return;
    const t = setTimeout(() => el.scrollTo({ top: el.scrollHeight, behavior: "smooth" }), 400);
    return () => clearTimeout(t);
  }, [done]);
  const ready = complete && requiredMissing.length === 0;

  const copyFix = async (it: EnvItem) => {
    if (!it.fix_cmd) return;
    try {
      await clipboardSetText(it.fix_cmd);
      setCopied(it.id);
      setTimeout(() => setCopied(""), 1500);
    } catch {
      /* 剪贴板不可用时忽略 */
    }
  };

  const dataItem = get("datadir");

  /** 环形进度（SVG 描边动画） */
  const pct = Math.round((received / total) * 100);
  const ring = useMemo(() => {
    const r = 15;
    const c = 2 * Math.PI * r;
    return { r, c, off: c * (1 - (done ? 1 : received / total)) };
  }, [received, total, done]);

  return (
    <div className="onb-root">
      <div className={`onb-card${ready ? " is-ready" : ""}`} ref={cardRef}>
        <button className="onb-skip" onClick={() => finishFx("skipped")} title="跳过引导 (Esc)">
          跳过 (Esc)
        </button>
        <div className="onb-head">
          <div className="onb-logo-wrap">
            <span className="onb-logo">泽</span>
            <span className="onb-logo-ring" />
          </div>
          <div>
            <div className="onb-title">
              白泽 · 环境自检
              <span className="onb-sys-tag">{ready ? "SYSTEM READY" : "INITIALIZING"}</span>
            </div>
            <div className="onb-subtitle">正在真实探测这台电脑的运行条件 · 全程在本机完成</div>
          </div>
        </div>

        <div className="onb-progress">
          <svg className="onb-ring" viewBox="0 0 40 40" width="40" height="40">
            <circle className="onb-ring-bg" cx="20" cy="20" r={ring.r} />
            <circle
              className="onb-ring-fill"
              cx="20"
              cy="20"
              r={ring.r}
              strokeDasharray={ring.c}
              strokeDashoffset={ring.off}
            />
            <text className="onb-ring-text" x="20" y="21" textAnchor="middle" dominantBaseline="central">
              {pct}
            </text>
          </svg>
          <div className="onb-progress-bar">
            <div
              className="onb-progress-fill"
              style={{ width: `${done ? 100 : Math.round((received / total) * 100)}%` }}
            />
          </div>
          <span className="onb-progress-text">
            {done ? "检测完成" : `检测中 ${received}/${total}`}
          </span>
        </div>

        {/* 扫描光束悬浮于检测列表之上，检测进行中来回扫 */}
        <div className={`onb-groups${done ? " scanned" : ""}`}>
          {GROUPS.map((g) => (
            <div className="onb-group" key={g.title}>
              <div className="onb-group-title">
                <span className="onb-group-icon">{g.icon}</span>
                {g.title}
              </div>
              {g.ids.map((id) => {
                const e = EXPECTED.find((x) => x.id === id)!;
                const it = get(id);
                const st = it ? it.status : "pending";
                return (
                  <div className={`onb-row st-${st}`} key={id}>
                    <span className="onb-ico">{it ? STATUS_ICON[st] : "◌"}</span>
                    <div className="onb-mid">
                      <div className="onb-name">
                        {e.name}
                        {it?.version && <span className="onb-ver">{it.version}</span>}
                      </div>
                      <div className="onb-sub">{rowSub(it, STATUS_LABEL[st])}</div>
                    </div>
                    <span className="onb-tag">{it ? STATUS_LABEL[st] : "…"}</span>
                  </div>
                );
              })}
              {/* 数据绑定组：路径卡 + 打开按钮 */}
              {g.title === "数据绑定" && dataItem && dataItem.status === "ok" && (
                <div className="onb-databind">
                  <span className="onb-databind-dot" />
                  <div className="onb-databind-mid">
                    <div className="onb-databind-path" title={dataItem.path}>
                      {dataItem.path}
                    </div>
                    <div className="onb-databind-sub">
                      对话 / 记忆 / 用量 / 登录态全部保存在安装目录内 · 卸载不残留
                    </div>
                  </div>
                  <button
                    className="onb-btn"
                    onClick={() => void openDataFolder().catch(() => {})}
                  >
                    打开
                  </button>
                </div>
              )}
            </div>
          ))}
        </div>

        {done && requiredMissing.length > 0 && (
          <div className="onb-fix">
            <div className="onb-fix-title">必需环境缺失（{requiredMissing.length} 项）</div>
            {requiredMissing.map((it) => (
              <div className="onb-fix-item" key={it.id}>
                <div className="onb-fix-text">
                  <b>{it.name}</b>：{it.hint || it.detail}
                </div>
                {it.fix_cmd && (
                  <button className="onb-btn" onClick={() => void copyFix(it)}>
                    {copied === it.id ? "已复制" : "复制命令"}
                  </button>
                )}
              </div>
            ))}
          </div>
        )}

        {done && requiredMissing.length === 0 && optionalMissing.length > 0 && (
          <div className="onb-opt-note">
            部分增强能力未就绪（{optionalMissing.map((i) => i.name).join("、")}），不影响核心功能，
            可随时在 设置 → 环境检测 中查看补装指引
          </div>
        )}

        <div className="onb-footer">
          {done ? (
            !complete ? (
              <>
                <span className="onb-footer-msg warn">检测未完整结束，请重新检测</span>
                <button className="onb-btn primary" onClick={start}>
                  重新检测
                </button>
              </>
            ) : requiredMissing.length === 0 ? (
              <>
                <span className="onb-footer-msg ok">环境就绪，白泽已可以正常工作</span>
                <button className="onb-btn primary" onClick={() => finishFx("done")}>
                  进入白泽
                </button>
              </>
            ) : (
              <>
                <span className="onb-footer-msg warn">
                  修复后可点「重新检测」；缺失期间核心对话仍可使用
                </span>
                <button className="onb-btn ghost" onClick={start}>
                  重新检测
                </button>
                <button className="onb-btn primary" onClick={() => finishFx("done")}>
                  仍然进入
                </button>
              </>
            )
          ) : (
            <span className="onb-footer-msg">正在逐项真实探测，通常几秒内完成…</span>
          )}
        </div>
      </div>
    </div>
  );
}

/**
 * 非首次启动的环境提示卡：仅当 onboarding 已完成但缓存报告中必需项仍缺失时出现
 * （上次跳过/带病进入的用户）。非阻塞，复检通过自动消失。
 */
export function EnvNotice() {
  const [missing, setMissing] = useState<string[] | null>(null);
  const [rechecking, setRechecking] = useState(false);

  useEffect(() => {
    (async () => {
      try {
        const st = await envGetState();
        if (!st.onboarding_done) return; // 首次启动由全屏引导处理
        const bad = (st.report?.items ?? []).filter(
          (i) => i.level === "required" && i.status === "missing"
        );
        if (bad.length > 0) setMissing(bad.map((i) => i.name));
      } catch {
        /* 读不到报告就不打扰 */
      }
    })();
  }, []);

  const recheck = async () => {
    setRechecking(true);
    try {
      const list = await envDetectAll();
      const bad = list.filter((i) => i.level === "required" && i.status === "missing");
      setMissing(bad.length ? bad.map((i) => i.name) : null);
    } catch {
      /* 复检失败保留原提示 */
    } finally {
      setRechecking(false);
    }
  };

  if (missing === null) return null;
  return (
    <div className="env-notice">
      <span className="env-notice-dot" />
      <span>环境未就绪：{missing.join("、")}</span>
      <button onClick={() => void recheck()} disabled={rechecking}>
        {rechecking ? "检测中…" : "重新检测"}
      </button>
      <button className="x" title="本次不再提示" onClick={() => setMissing(null)}>
        ×
      </button>
    </div>
  );
}
