import { useEffect, useMemo, useState } from "react";
import { useDock } from "../../kernel/store/dock";
import {
  getNotificationConfig, saveNotificationConfig,
  getGuiConfig, saveGuiConfig,
  getPipelineConfig, savePipelineConfig,
  listModels, setModelEnabled, setModelProfile,
  createModel, deleteModel,
  getWorkModeConfig,
} from "../../kernel/api";
import type { GuiConfig, ModelConfig, NotificationConfig, PipelineConfig, WorkModeConfig } from "../../kernel/types";

type Tab = "notify" | "gui" | "pipeline" | "model" | "mode";

export function SettingsConsole() {
  const open = useDock((s) => s.settingsOpen);
  const toggle = useDock((s) => s.toggleSettings);
  const [tab, setTab] = useState<Tab>("notify");
  const [nc, setNc] = useState<NotificationConfig | null>(null);
  const [gc, setGc] = useState<GuiConfig | null>(null);
  const [pc, setPc] = useState<PipelineConfig | null>(null);
  const [mc, setMc] = useState<ModelConfig[]>([]);
  const [wc, setWc] = useState<WorkModeConfig | null>(null);

  const load = () => {
    Promise.all([
      getNotificationConfig().then((x) => setNc(x)).catch(() => {}),
      getGuiConfig().then((x) => setGc(x)).catch(() => {}),
      getPipelineConfig().then((x) => setPc(x)).catch(() => {}),
      listModels().then((x) => setMc(x)).catch(() => {}),
      getWorkModeConfig().then((x) => setWc(x)).catch(() => {}),
    ]).then(() => {});
  };
  useEffect(() => { if (open) load(); }, [open]);

  if (!open) return null;
  return (
    <div className="panel-overlay" onClick={(e) => { if (e.target === e.currentTarget) toggle(false); }}>
      <div className="panel-float" onClick={(e) => e.stopPropagation()}>
        <div className="pf-head">
          <span className="pad-sigil" />
          <h3>水晶控制台</h3>
          <span style={{ fontFamily: "var(--mono)", fontSize: 11, color: "var(--faint)", letterSpacing: 2 }}>CRYSTAL · CONSOLE</span>
          <button className="iconbtn" onClick={() => toggle(false)}>✕</button>
        </div>
        <div className="pf-body">
          <nav className="pf-nav">
            {[
              ["notify", "升级通知链"],
              ["gui", "显示与接管"],
              ["pipeline", "流水线"],
              ["model", "模型管理"],
              ["mode", "工作模式"],
            ].map(([k, l]) => (
              <button key={k} className={tab === k ? "on" : ""} onClick={() => setTab(k as Tab)}>{l}</button>
            ))}
          </nav>
          <div className="pf-main">
            {tab === "notify" && nc && (
              <NotifyConfigPanel v={nc} save={(v) => { saveNotificationConfig(v).then(load).catch(() => {}); setNc(v); }} />
            )}
            {tab === "gui" && gc && (
              <GuiPanel v={gc} save={(v) => { saveGuiConfig(v).then(load).catch(() => {}); setGc(v); }} />
            )}
            {tab === "pipeline" && pc && (
              <PipePanel v={pc} save={(v) => { savePipelineConfig(v).then(load).catch(() => {}); setPc(v); }} />
            )}
            {tab === "model" && (
              <ModelPanel models={mc} reload={load} />
            )}
            {tab === "mode" && wc && (
              <ModePanel v={wc} />
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

function Row({ children }: { children: React.ReactNode }) {
  return <div className="setting-row">{children}</div>;
}
function Sw({ on, flip }: { on: boolean; flip: () => void }) {
  return <button className={`switch ${on ? "on" : ""}`} onClick={flip}><i /></button>;
}

function NotifyConfigPanel({ v, save }: { v: NotificationConfig; save: (v: NotificationConfig) => void }) {
  const tiers = ["L1_POPUP", "L2_SYSTEM", "L3_VOICE", "L4_EMAIL", "L5_WEBHOOK"] as const;
  const tierName: Record<string, string> = {
    L1_POPUP: "L1 窗口弹层", L2_SYSTEM: "L2 系统通知", L3_VOICE: "L3 语音播报",
    L4_EMAIL: "L4 邮件", L5_WEBHOOK: "L5 Webhook",
  };
  return (
    <div>
      <div className="setting-tip">五级升级链：低级别未响应 → 自动升级到下一级。每项可独立开关并配置超时。</div>
      {tiers.map((t) => {
        const row = v.levels[t];
        const patch = (up: Partial<typeof row>) => save({ ...v, levels: { ...v.levels, [t]: { ...row, ...up } } });
        return (
          <div key={t} style={{ paddingBottom: 6, marginBottom: 6, borderBottom: "1px dashed var(--line)" }}>
            <Row>
              <span><strong style={{ color: "var(--violet-hi)" }}>{tierName[t]}</strong> · 启用</span>
              <Sw on={row.enabled} flip={() => patch({ enabled: !row.enabled })} />
            </Row>
            <Row>
              <span>超时（秒）</span>
              <input className="setting-num" type="number" value={row.timeout_seconds}
                onChange={(e) => patch({ timeout_seconds: Number(e.target.value) || 0 })} />
              <span className="setting-unit">s</span>
            </Row>
          </div>
        );
      })}
      <Row>
        <span>邮件 SMTP 地址</span>
        <input className="setting-input" value={v.email.smtp_host || ""}
          onChange={(e) => save({ ...v, email: { ...v.email, smtp_host: e.target.value } })} />
      </Row>
      <Row>
        <span>Webhook URL</span>
        <input className="setting-input" value={v.webhook.url || ""}
          onChange={(e) => save({ ...v, webhook: { ...v.webhook, url: e.target.value } })} />
      </Row>
      <button className="setting-btn">保存配置</button>
    </div>
  );
}

function GuiPanel({ v, save }: { v: GuiConfig; save: (v: GuiConfig) => void }) {
  return (
    <div>
      <div className="setting-tip">屏幕接管：锁定期间屏蔽物理键鼠输入，按 Ctrl+Shift+F12 紧急释放。</div>
      <Row>
        <span>GUI 屏幕接管</span>
        <Sw on={v.screen_control?.enable ?? false}
          flip={() => save({ ...v, screen_control: { ...(v.screen_control ?? {} as any), enable: !(v.screen_control?.enable ?? false) } })} />
      </Row>
      <Row>
        <span>OCR 语言包</span>
        <input className="setting-input" value={v.ocr_langs ?? "chi_sim+eng"}
          onChange={(e) => save({ ...v, ocr_langs: e.target.value })} />
      </Row>
      <Row>
        <span>视觉模型超时（秒）</span>
        <input className="setting-num" type="number" value={v.visual_timeout_seconds ?? 15}
          onChange={(e) => save({ ...v, visual_timeout_seconds: Number(e.target.value) || 15 })} />
        <span className="setting-unit">s</span>
      </Row>
      <Row>
        <span>显示步骤弹幕</span>
        <Sw on={v.show_step ?? false} flip={() => save({ ...v, show_step: !(v.show_step ?? false) })} />
      </Row>
      <Row>
        <span>步骤弹幕字体大小</span>
        <input className="setting-num" type="number" value={v.step_font_size ?? 14}
          onChange={(e) => save({ ...v, step_font_size: Number(e.target.value) || 14 })} />
        <span className="setting-unit">px</span>
      </Row>
      <button className="setting-btn">保存配置</button>
    </div>
  );
}

function PipePanel({ v, save }: { v: PipelineConfig; save: (v: PipelineConfig) => void }) {
  return (
    <div>
      <div className="setting-tip">终止循环判定：指纹相同或重复调用即视为循环，避免死循环消耗 token。</div>
      <Row>
        <span>循环检测 · 同参连续次数</span>
        <input className="setting-num" type="number" value={v.same_call_streak ?? 3}
          onChange={(e) => save({ ...v, same_call_streak: Number(e.target.value) || 3 })} />
        <span className="setting-unit">次</span>
      </Row>
      <Row>
        <span>循环检测 · 同指纹出现次数</span>
        <input className="setting-num" type="number" value={v.fingerprint_repeats ?? 5}
          onChange={(e) => save({ ...v, fingerprint_repeats: Number(e.target.value) || 5 })} />
        <span className="setting-unit">次 / 12 轮</span>
      </Row>
      <Row>
        <span>用例生成 · 非纯文本抽取</span>
        <Sw on={v.extract_binary_attachments ?? true}
          flip={() => save({ ...v, extract_binary_attachments: !(v.extract_binary_attachments ?? true) })} />
      </Row>
      <button className="setting-btn">保存配置</button>
    </div>
  );
}

function ModelPanel({ models, reload }: { models: ModelConfig[]; reload: () => void }) {
  const [show, setShow] = useState(false);
  const [form, setForm] = useState<Partial<ModelConfig>>({ name: "", model_id: "", base_url: "", api_key: "", tier: "cloud" });
  const doCreate = async () => {
    if (!form.name || !form.model_id) return;
    await createModel(form as ModelConfig).catch(() => {});
    setShow(false); reload();
  };
  return (
    <div>
      <div className="setting-tip">为每个模型独立配置提供商、密钥、启用状态；切换会触发事件广播通知所有运行时。</div>
      {models.map((m) => (
        <div key={m.id} className={`model-item-card ${m.enabled ? "on" : ""}`}>
          <span style={{ width: 6, height: 6, borderRadius: 999, background: m.enabled ? (m.tier === "cloud" ? "var(--orchid)" : "var(--violet-hi)") : "var(--faint)" }} />
          <div>
            <div className="miname">{m.name}</div>
            <div style={{ fontSize: 11, color: "var(--faint)" }}>API: {m.provider} · Tier: {m.tier}</div>
          </div>
          <span className="mimodel">{m.model_id}</span>
          <i className="mistate">{m.enabled ? "ENABLED" : "DISABLED"}</i>
          <button className="iconbtn" onClick={() => { setModelEnabled(m.id, !m.enabled).then(reload).catch(() => {}); }}
            style={{ marginLeft: 8 }}>{m.enabled ? "⏻" : "⏽"}</button>
          <button className="iconbtn danger" onClick={() => { if (confirm("删除该模型配置？")) deleteModel(m.id).then(reload).catch(() => {}); }}>✕</button>
        </div>
      ))}
      {!show && <button className="setting-btn" onClick={() => setShow(true)}>＋ 新增模型</button>}
      {show && (
        <div style={{ marginTop: 12, padding: 14, border: "1px dashed var(--line-mid)", borderRadius: 14 }}>
          <Row><span>显示名称</span><input className="setting-input" value={form.name ?? ""} onChange={(e) => setForm({ ...form, name: e.target.value })} /></Row>
          <Row><span>模型 ID</span><input className="setting-input" value={form.model_id ?? ""} onChange={(e) => setForm({ ...form, model_id: e.target.value })} /></Row>
          <Row><span>Provider</span><input className="setting-input" value={form.provider ?? ""} onChange={(e) => setForm({ ...form, provider: e.target.value })} /></Row>
          <Row><span>Base URL</span><input className="setting-input" value={form.base_url ?? ""} onChange={(e) => setForm({ ...form, base_url: e.target.value })} /></Row>
          <Row><span>API Key</span><input className="setting-input" type="password" value={form.api_key ?? ""} onChange={(e) => setForm({ ...form, api_key: e.target.value })} /></Row>
          <div style={{ display: "flex", gap: 8 }}>
            <button className="setting-btn" onClick={() => setShow(false)}>取消</button>
            <button className="setting-btn" style={{ color: "var(--aurora-g)", borderColor: "rgba(110,244,197,0.4)", background: "rgba(110,244,197,0.08)" }} onClick={doCreate}>保存</button>
          </div>
        </div>
      )}
    </div>
  );
}

function ModePanel({ v }: { v: WorkModeConfig }) {
  return (
    <div>
      <div className="setting-tip">工作模式控制可使用的工具白名单、系统提示词、文档与工具模板。切换后会回收旧命名空间下的工具。</div>
      <div style={{ display: "flex", gap: 10 }}>
        {v.modes?.map((m) => (
          <div key={m.id} style={{
            padding: 14, borderRadius: 14,
            background: v.current_mode_id === m.id
              ? "linear-gradient(135deg, var(--violet-dim), rgba(255,142,203,0.08))"
              : "var(--glass)",
            border: "1px solid",
            borderColor: v.current_mode_id === m.id ? "var(--line-orchid)" : "var(--line)",
            flex: 1,
          }}>
            <div style={{ fontSize: 14, fontWeight: 600, color: "var(--ink)", marginBottom: 4 }}>{m.name}</div>
            <div style={{ fontSize: 11, color: "var(--faint)", fontFamily: "var(--mono)", letterSpacing: 1 }}>{v.current_mode_id === m.id ? "当前激活" : "模式 ID: " + m.id}</div>
            <p style={{ fontSize: 12, color: "var(--ink-dim)", marginTop: 6, lineHeight: 1.6 }}>{m.description}</p>
            <div style={{ marginTop: 8, fontSize: 11, color: "var(--faint)" }}>
              允许工具 {m.allowed_tools?.length ?? 0} 个 · 模板 {m.tool_templates?.length ?? 0} · 文档 {m.doc_templates?.length ?? 0}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}