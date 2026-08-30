import { useState } from "react";
import { useChat } from "../../kernel/store/chat";
import { approvePermission, denyPermission } from "../../kernel/api";
import type { PermissionRequest } from "../../kernel/types";

function tierOf(req: PermissionRequest): "ro" | "write" | "hr" {
  if (req.level === "HIGH_RISK" || req.level === "CRITICAL") return "hr";
  if (req.level === "WRITE") return "write";
  return "ro";
}

const TIER_LABEL: Record<string, string> = {
  READ: "只读安全",
  LOW: "常规读取",
  WRITE: "写入操作",
  SYSTEM: "系统写入",
  HIGH_RISK: "高危操作",
  CRITICAL: "关键操作",
};

export function ApprovalRail() {
  const pending = useChat((s) => s.pending);
  const removePending = useChat((s) => s.removePending);
  const [mem, setMem] = useState<Record<string, boolean>>({});

  const pass = async (req: PermissionRequest) => {
    removePending(req.id);
    await approvePermission(req.id, mem[req.id] ?? false).catch(() => {});
  };
  const deny = async (req: PermissionRequest) => {
    removePending(req.id);
    await denyPermission(req.id, mem[req.id] ?? false).catch(() => {});
  };

  if (pending.length === 0) return null;

  return (
    <div className="lockrail">
      {pending.map((req) => {
        const t = tierOf(req);
        const inst = req.install_info;
        return (
          <div key={req.id} className={`lockcard ${t}`}>
            <div className="lock-head">
              <span className="lock-grade">{TIER_LABEL[req.level ?? "READ"]}</span>
              <span className="lock-title">{req.tool_name}</span>
              {req.channel && <span className="lock-chan">通道 #{req.channel}</span>}
            </div>
            {inst && (
              <div className="lock-install">
                <div className="il2row"><span>软件</span><b>{inst.name}</b></div>
                <div className="il2row"><span>厂商</span><b>{inst.vendor || "—"}</b></div>
                <div className="il2row"><span>版本</span><b>{inst.version || "—"}</b></div>
              </div>
            )}
            <pre className="lock-args">{JSON.stringify(req.args ?? {}, null, 2)}</pre>
            <div className="lock-actions">
              <label className="lock-remember">
                <input
                  type="checkbox"
                  checked={mem[req.id] ?? false}
                  onChange={(e) => setMem((m) => ({ ...m, [req.id]: e.target.checked }))}
                />
                记住本次会话
              </label>
              <button className="lockbtn deny" onClick={() => deny(req)}>拒 绝</button>
              <button className="lockbtn pass" onClick={() => pass(req)}>允 许</button>
            </div>
          </div>
        );
      })}
    </div>
  );
}