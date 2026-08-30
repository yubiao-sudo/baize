import React, { useEffect, useRef, useState } from "react";
import { useChat } from "../stores/chat";
import { onAcuiCard, onEscalationUpdate, onPermissionChannel, onProactive, resolvePermission } from "../api";
import { playSfx } from "../utils/sound";
import { renderMarkdown } from "../utils/markdown";
import type { AcuiCardData, EscalationUpdate, PermissionRequest, ProactiveCard } from "../types";

/**
 * ACUI 主动卡片：agent 主动"推送"给用户的卡片（白龙马 ACUI 理念的落地）。
 * 三类卡片：
 *  1) awaken 觉醒自检卡（启动时推送一次）
 *  2) permission 权限卡（高危操作时推送）
 *  3) proactive 主动提醒卡（文件监听等后台感知触发）
 */

function AwakenCard({ onClose }: { onClose: () => void }) {
  return (
    <div className="acui-card awaken">
      <div className="acui-head">
        <span className="agent">泽</span>
        <span>白泽已觉醒</span>
      </div>
      <div className="acui-body">常驻循环启动，自检通过。需要我帮你做点什么吗？</div>
      <div className="acui-actions">
        <button className="acui-btn primary" onClick={onClose}>
          开始使用
        </button>
      </div>
    </div>
  );
}

const levelColor = (level: number) => {
  switch (level) {
    case 0: return "#64748b";
    case 1: return "#f59e0b";
    case 2: return "#ef4444";
    case 3: return "#dc2626";
    case 4: return "#991b1b";
    default: return "#64748b";
  }
};

function EscalationBar({ esc }: { esc?: EscalationUpdate }) {
  if (!esc) return null;
  return (
    <div
      className="escalation-bar"
      style={{ borderColor: levelColor(esc.level), margin: "0 14px 6px" }}
    >
      <span className="escalation-icon">{"⚠".repeat(Math.min(esc.level + 1, 3))}</span>
      <span className="escalation-label">
        通知已升级至：{esc.level_label}
        {esc.max_level ? "（已达最高级）" : ""}
      </span>
    </div>
  );
}

/** IM 通道 id → 中文名 */
const CHANNEL_LABELS: Record<string, string> = { wechat: "微信", feishu: "飞书" };

/** 审批回传通道标注：显示本次审批实际推送到了哪些 IM 通道 */
function ChannelNote({ channels }: { channels?: string[] }) {
  if (!channels || channels.length === 0) return null;
  const labels = channels.map((c) => CHANNEL_LABELS[c] ?? c);
  return (
    <div className="acui-channel-note">
      <span className="acui-channel-dot" />
      审批已推送至：{labels.join("、")}
    </div>
  );
}

/** 软件安装专用确认卡：图标首字母 + 名称 + 目标盘 + 推荐理由 */
function InstallApprovalCard({
  req,
  esc,
  channels,
  checked,
  onRemember,
  onDecide,
}: {
  req: PermissionRequest;
  esc?: EscalationUpdate;
  channels?: string[];
  checked: boolean;
  onRemember: (v: boolean) => void;
  onDecide: (approved: boolean) => void;
}) {
  const d = req.detail;
  const name = d?.name?.trim();
  const initial = (name ? name.charAt(0).toUpperCase() : (d?.id?.charAt(0)?.toUpperCase() ?? "?"));
  return (
    <div key={req.id} className="acui-card install">
      <div className="acui-head">
        <span className="install-avatar">{initial}</span>
        <span>安装软件 · 需要确认</span>
      </div>
      <div className="acui-body">
        <div className="install-title">{name ?? d?.id ?? "未知软件"}</div>
        {name && <div className="install-id">{d?.id}</div>}
        <div className="install-loc">
          <span className="install-kv">将安装到</span>
          <span className="install-target" title={d?.target}>
            {d?.target || "系统默认目录"}
          </span>
        </div>
        {d?.reason && <div className="install-reason">{d.reason}</div>}
      </div>
      <ChannelNote channels={channels} />
      <EscalationBar esc={esc} />
      <div className="acui-actions">
        <label style={{ marginRight: "auto", display: "flex", alignItems: "center", gap: 6, fontSize: 12, color: "var(--text)" }}>
          <input
            type="checkbox"
            checked={checked}
            onChange={(e) => onRemember(e.target.checked)}
          />
          记住此决定
        </label>
        <button className="acui-btn danger" onClick={() => onDecide(false)}>
          取消
        </button>
        <button className="acui-btn primary" onClick={() => onDecide(true)}>
          允许安装
        </button>
      </div>
    </div>
  );
}

function PermissionCards() {
  const pending = useChat((s) => s.pending);
  const removePending = useChat((s) => s.removePending);
  const [escalations, setEscalations] = useState<Record<string, EscalationUpdate>>({});
  const [rememberFlags, setRememberFlags] = useState<Record<string, boolean>>({});
  const [channels, setChannels] = useState<Record<string, string[]>>({});

  // 订阅升级状态更新
  useEffect(() => {
    const unlisten = onEscalationUpdate((e) => {
      setEscalations((prev) => ({
        ...prev,
        [e.approval_id]: e,
      }));
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  // 订阅审批回传通道标注（哪个 IM 通道实际送达了审批）
  useEffect(() => {
    const unlisten = onPermissionChannel((e) => {
      setChannels((prev) => ({ ...prev, [e.approval_id]: e.channels }));
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  const decide = async (id: string, approved: boolean, remember: boolean) => {
    await resolvePermission(id, approved, remember);
    removePending(id);
    setEscalations((prev) => {
      const next = { ...prev };
      delete next[id];
      return next;
    });
    setChannels((prev) => {
      const next = { ...prev };
      delete next[id];
      return next;
    });
  };

  return (
    <>
      {pending.map((req) => {
        const esc = escalations[req.id];
        // 软件安装用专用富卡片
        if (req.tool === "software_install" && req.detail) {
          return (
            <InstallApprovalCard
              key={req.id}
              req={req}
              esc={esc}
              channels={channels[req.id]}
              checked={!!rememberFlags[req.id]}
              onRemember={(v) => setRememberFlags((prev) => ({ ...prev, [req.id]: v }))}
              onDecide={(approved) => void decide(req.id, approved, !!rememberFlags[req.id])}
            />
          );
        }
        return (
          <div key={req.id} className="acui-card permission">
            <div className="acui-head">
              <span className="agent">泽</span>
              <span>请求权限 · {req.tool}</span>
            </div>
            <div className="acui-body">
              <div style={{ marginBottom: 4 }}>将执行以下操作（真实载荷）：</div>
              <pre>{JSON.stringify(req.args, null, 2)}</pre>
            </div>
            <ChannelNote channels={channels[req.id]} />
            <EscalationBar esc={esc} />
            <div className="acui-actions">
              <label style={{ marginRight: "auto", display: "flex", alignItems: "center", gap: 6, fontSize: 12, color: "var(--text)" }}>
                <input
                  type="checkbox"
                  checked={!!rememberFlags[req.id]}
                  onChange={(e) =>
                    setRememberFlags((prev) => ({ ...prev, [req.id]: e.target.checked }))
                  }
                />
                记住此决定
              </label>
              <button className="acui-btn danger" onClick={() => void decide(req.id, false, !!rememberFlags[req.id])}>
                拒绝
              </button>
              <button className="acui-btn primary" onClick={() => void decide(req.id, true, !!rememberFlags[req.id])}>
                允许
              </button>
            </div>
          </div>
        );
      })}
    </>
  );
}

/** 万能数据卡片：自动识别 JSON / markdown / 表格 并美化渲染 */
function UniversalDataCard({ data }: { data: string }) {
  const [expanded, setExpanded] = useState(true);
  if (!data?.trim()) return null;

  // 1) 尝试 JSON 解析：对象 → 键值网格；数组 → 表格
  let json: unknown = undefined;
  try {
    json = JSON.parse(data);
  } catch {
    json = undefined;
  }

  let content: React.ReactNode = null;
  if (json !== undefined) {
    if (Array.isArray(json) && json.length > 0 && typeof json[0] === "object" && json[0] !== null) {
      // 数组对象 → 表格
      const rows = json as Record<string, unknown>[];
      const cols = Array.from(
        rows.reduce<Set<string>>((s, r) => {
          Object.keys(r).forEach((k) => s.add(k));
          return s;
        }, new Set()),
      );
      content = (
        <div className="udc-table-wrap">
          <table className="udc-table">
            <thead>
              <tr>
                {cols.map((c) => (
                  <th key={c}>{c}</th>
                ))}
              </tr>
            </thead>
            <tbody>
              {rows.map((r, i) => (
                <tr key={i}>
                  {cols.map((c) => (
                    <td key={c}>{String(r[c] ?? "")}</td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      );
    } else if (typeof json === "object" && json !== null) {
      // 对象 → 键值网格
      const entries = Object.entries(json as Record<string, unknown>);
      content = (
        <div className="udc-kv">
          {entries.map(([k, v]) => (
            <div className="udc-kv-row" key={k}>
              <span className="udc-kv-k">{k}</span>
              <span className="udc-kv-v">
                {typeof v === "object" ? JSON.stringify(v) : String(v)}
              </span>
            </div>
          ))}
        </div>
      );
    } else {
      // JSON 标量
      content = <pre className="udc-pre">{JSON.stringify(json, null, 2)}</pre>;
    }
  } else {
    // 2) 非 JSON：含 markdown 表格/标题/列表 → markdown 渲染；否则纯文本
    const hasMd = /^\s*(\||#|[-*]\s|\d+\.\s)/m.test(data);
    if (hasMd) {
      content = (
        <div
          className="udc-md"
          dangerouslySetInnerHTML={{ __html: renderMarkdown(data) }}
        />
      );
    } else {
      content = <pre className="udc-pre">{data}</pre>;
    }
  }

  return (
    <div className="udc">
      <button
        className="udc-toggle"
        onClick={() => setExpanded((v) => !v)}
        title={expanded ? "收起" : "展开"}
      >
        <span className="udc-label">执行结果</span>
        <span className={`udc-arrow ${expanded ? "open" : ""}`}>▾</span>
      </button>
      {expanded && <div className="udc-content">{content}</div>}
    </div>
  );
}

function ProactiveCards({ cards, onDismiss }: { cards: ProactiveCard[]; onDismiss: (id: string) => void }) {
  const send = useChat((s) => s.send);

  return (
    <>
      {cards.map((p) => (
        <div key={p.id} className="acui-card awaken">
          <div className="acui-head">
            <span className="agent">泽</span>
            <span>{p.title}</span>
          </div>
          <div className="acui-body">
            {p.body}
            {p.files.length > 0 && (
              <div className="acui-files">
                {p.files.map((f) => (
                  <span key={f} className="acui-file" title={f}>
                    {f}
                  </span>
                ))}
              </div>
            )}
            {p.data && <UniversalDataCard data={p.data} />}
          </div>
          <div className="acui-actions">
            <button className="acui-btn" onClick={() => onDismiss(p.id)}>
              忽略
            </button>
            {p.data && (
              <button
                className="acui-btn"
                title="让白泽美化展示这份数据"
                onClick={() => {
                  onDismiss(p.id);
                  void send(
                    `请用 render_card 工具（kind="data"）把以下定时任务执行结果美化成一张易读的数据卡片展示给我：\n\n${p.data}`,
                  );
                }}
              >
                美化
              </button>
            )}
            <button
              className="acui-btn primary"
              onClick={() => {
                onDismiss(p.id);
                const prompt =
                  p.action ??
                  (p.files.length > 0
                    ? `下载文件夹新增了这些文件：${p.files.join("、")}。请帮我查看它们并按类型整理。`
                    : "下载文件夹有新增内容，请帮我查看并按类型整理。");
                void send(prompt);
              }}
            >
              让白泽处理
            </button>
          </div>
        </div>
      ))}
    </>
  );
}

/** ACUI 受控卡片：Agent 通过 render_card 工具渲染（白名单类型） */
function RenderCards({ cards, onDismiss }: { cards: AcuiCardData[]; onDismiss: (id: string) => void }) {
  const send = useChat((s) => s.send);

  return (
    <>
      {cards.map((c) => (
        <div key={c.id} className={`acui-card acui-render acui-${c.kind}`}>
          <div className="acui-head">
            <span className="agent">泽</span>
            <span>{c.title}</span>
          </div>
          <div className="acui-body">
            {c.body}
            {c.kind === "progress" && (
              <div className="acui-progress">
                <div className="acui-progress-bar" style={{ width: `${c.progress ?? 0}%` }} />
              </div>
            )}
            {c.kind === "data" && c.data && <UniversalDataCard data={c.data} />}
          </div>
          <div className="acui-actions">
            {c.kind === "confirm" ? (
              <>
                <button className="acui-btn" onClick={() => onDismiss(c.id)}>
                  取消
                </button>
                <button
                  className="acui-btn primary"
                  onClick={() => {
                    onDismiss(c.id);
                    void send(`用户已确认：${c.title}`);
                  }}
                >
                  确认
                </button>
              </>
            ) : (
              <button className="acui-btn" onClick={() => onDismiss(c.id)}>
                关闭
              </button>
            )}
          </div>
        </div>
      ))}
    </>
  );
}

export default function AcuiCard() {
  const [showAwaken, setShowAwaken] = useState(true);
  const [proactives, setProactives] = useState<ProactiveCard[]>([]);
  const [acuiCards, setAcuiCards] = useState<AcuiCardData[]>([]);
  const pending = useChat((s) => s.pending);

  // 订阅后台主动提醒
  useEffect(() => {
    let disposed = false;
    onProactive((card) => {
      if (disposed) return;
      // 主动提醒/定时提醒：风铃通知音
      playSfx("notify");
      setProactives((p) => (p.some((x) => x.id === card.id) ? p : [...p, card]));
    });
    return () => {
      disposed = true;
    };
  }, []);

  // 订阅 ACUI 受控卡片（render_card 工具）
  useEffect(() => {
    let disposed = false;
    onAcuiCard((card) => {
      if (disposed) return;
      // 卡片弹入：轻快的泡泡音
      playSfx("card-pop");
      setAcuiCards((c) => [...c, card]);
    });
    return () => {
      disposed = true;
    };
  }, []);

  // 审批请求卡：新审批到来时「叩叩」提示（首帧加载的历史审批不出声）
  const pendingCount = pending.length;
  const prevPendingRef = useRef(pendingCount);
  useEffect(() => {
    if (pendingCount > prevPendingRef.current) playSfx("permission");
    prevPendingRef.current = pendingCount;
  }, [pendingCount]);

  // 觉醒卡 6 秒后自动收起
  useEffect(() => {
    if (showAwaken) {
      const t = setTimeout(() => setShowAwaken(false), 6000);
      return () => clearTimeout(t);
    }
  }, [showAwaken]);

  const dismissProactive = (id: string) => setProactives((p) => p.filter((x) => x.id !== id));
  const dismissAcui = (id: string) => setAcuiCards((c) => c.filter((x) => x.id !== id));

  if (!showAwaken && pending.length === 0 && proactives.length === 0 && acuiCards.length === 0) {
    return null;
  }

  return (
    <div className="card-stack">
      {showAwaken && <AwakenCard onClose={() => setShowAwaken(false)} />}
      <ProactiveCards cards={proactives} onDismiss={dismissProactive} />
      <RenderCards cards={acuiCards} onDismiss={dismissAcui} />
      <PermissionCards />
    </div>
  );
}
