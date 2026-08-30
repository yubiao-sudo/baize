import { useState } from "react";
import { resolvePermission } from "../../core/api";
import { useChat } from "../../core/store/chat";
import { useHud } from "../../core/store/hud";
import type { PermissionRequest } from "../../core/types";

const CLASS_META: Record<PermissionRequest["class"], { label: string; cls: string }> = {
  ReadOnly: { label: "只读", cls: "ro" },
  Write: { label: "写入", cls: "wr" },
  HighRisk: { label: "高危", cls: "hr" },
};

const CHANNEL_NAMES: Record<string, string> = { wechat: "微信", feishu: "飞书" };

function previewArgs(args: unknown): string {
  try {
    const s = JSON.stringify(args);
    if (!s) return "";
    return s.length > 160 ? `${s.slice(0, 160)}…` : s;
  } catch {
    return String(args);
  }
}

function InterlockCard({ req }: { req: PermissionRequest }) {
  const [remember, setRemember] = useState(false);
  const channels = useHud((s) => s.channels[req.id] ?? []);
  const meta = CLASS_META[req.class];
  const args = previewArgs(req.args);

  const resolve = (ok: boolean) => {
    useChat.getState().removePending(req.id);
    void resolvePermission(req.id, ok, remember);
  };

  return (
    <div className={`il-card ${meta.cls}`}>
      <div className="il-h">
        <span className="il-badge">{meta.label}</span>
        <span className="il-title">权限联锁 · {req.tool}</span>
        {channels.length > 0 && (
          <span className="il-chan">已推送 {channels.map((c) => CHANNEL_NAMES[c] ?? c).join("/")}</span>
        )}
      </div>
      {args && <pre className="il-args">{args}</pre>}
      {req.detail && (
        <div className="il-install">
          <div className="il-row">
            <span>名称</span>
            <b>{req.detail.name}</b>
          </div>
          <div className="il-row">
            <span>位置</span>
            <b>
              {req.detail.target}（{req.detail.drive} 剩余 {req.detail.free_gb}GB）
            </b>
          </div>
          <div className="il-row">
            <span>理由</span>
            <b>{req.detail.reason}</b>
          </div>
        </div>
      )}
      <div className="il-actions">
        <label className="il-remember">
          <input
            type="checkbox"
            checked={remember}
            onChange={(e) => setRemember(e.target.checked)}
          />
          记住本次选择
        </label>
        <button className="il-btn deny" onClick={() => resolve(false)}>
          拒绝
        </button>
        <button className="il-btn pass" onClick={() => resolve(true)}>
          批准
        </button>
      </div>
    </div>
  );
}

/** 审批联锁：顶部拦截卡栈 */
export default function Interlock() {
  const pending = useChat((s) => s.pending);
  if (pending.length === 0) return null;
  return (
    <div className="interlock">
      {pending.map((r) => (
        <InterlockCard key={r.id} req={r} />
      ))}
    </div>
  );
}