import { useEffect, useState } from "react";
import { useChat } from "../../core/store/chat";
import { useHud } from "../../core/store/hud";

const LEVEL_NAMES = ["弹窗", "系统通知", "语音", "邮件", "WEBHOOK"];

/** 顶部状态带：徽记 / 推理状态 / 升级链 / 模型 / IM / 时钟 */
export default function StatusRail() {
  const busy = useChat((s) => s.busy);
  const comparing = useChat((s) => s.comparing);
  const models = useHud((s) => s.models);
  const im = useHud((s) => s.im);
  const escalation = useHud((s) => s.escalation);
  const [now, setNow] = useState(() => new Date());

  useEffect(() => {
    const t = setInterval(() => setNow(new Date()), 1000);
    return () => clearInterval(t);
  }, []);

  const active = models?.profiles.find((p) => p.id === models.active);
  const hh = now.toTimeString().slice(0, 8);

  return (
    <header className="statusrail">
      <div className="sr-brand">
        <span className="sr-sigil">◈</span>
        <span className="sr-name">BAIZE</span>
        <span className="sr-sub">// TALOS-Ⅱ 工业终端</span>
      </div>

      <div className="sr-mid">
        {(busy || comparing) && (
          <span className="sr-busy">
            <span className="pulse-dot" />
            {comparing ? "多模型推演中" : "推理中"}
          </span>
        )}
        {escalation && (
          <span className="sr-esc" title={`通知升级 · ${escalation.label}`}>
            {LEVEL_NAMES.map((name, i) => (
              <i key={name} className={`pip ${i < escalation.level ? "on" : ""}`} />
            ))}
            <em>{escalation.label}</em>
          </span>
        )}
      </div>

      <div className="sr-right">
        <span className="sr-model" title="当前激活模型">
          <i className={`tier ${active?.tier ?? "cloud"}`}>
            {active?.tier === "local" ? "本地" : "云端"}
          </i>
          {active?.name ?? "未配置"}
        </span>
        <span
          className={`sr-im ${im.wechat === "connected" ? "on" : im.wechat === "qr_pending" ? "pend" : ""}`}
          title={`微信 · ${im.wechat}`}
        >
          微信
        </span>
        <span
          className={`sr-im ${im.feishu === "connected" ? "on" : ""}`}
          title={`飞书 · ${im.feishu}`}
        >
          飞书
        </span>
        <span className="sr-clock">{hh}</span>
      </div>
    </header>
  );
}