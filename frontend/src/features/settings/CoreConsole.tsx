import { useEffect, useState } from "react";
import { getNotifyConfig, getVoice, setActiveModel, setNotifyConfig, setVoice } from "../../core/api";
import { useHud } from "../../core/store/hud";
import type { NotifyConfig } from "../../core/types";

const LEVEL_NAMES = ["L1 弹窗", "L2 系统通知", "L3 语音播报", "L4 邮件", "L5 Webhook"];
type Tab = "model" | "notify" | "voice";

function Toggle({ on, onChange }: { on: boolean; onChange: (v: boolean) => void }) {
  return (
    <button className={`tog ${on ? "on" : ""}`} onClick={() => onChange(!on)}>
      <i />
    </button>
  );
}

/** 核心控制台：模型链路 / 通知升级链 / 语音合成 */
export default function CoreConsole() {
  const toggleSettings = useHud((s) => s.toggleSettings);
  const models = useHud((s) => s.models);
  const setModels = useHud((s) => s.setModels);
  const notify = useHud((s) => s.notify);

  const [tab, setTab] = useState<Tab>("model");
  const [nc, setNc] = useState<NotifyConfig | null>(null);
  const [voiceName, setVoiceName] = useState("");

  useEffect(() => {
    getNotifyConfig().then(setNc).catch(() => {});
    getVoice().then(setVoiceName).catch(() => {});
  }, []);

  const activate = async (id: string) => {
    try {
      const c = await setActiveModel(id);
      setModels(c);
      notify("info", `已切换模型：${c.profiles.find((p) => p.id === c.active)?.name ?? id}`);
    } catch {
      /* 切换失败保持原样 */
    }
  };

  const saveNotify = async () => {
    if (!nc) return;
    await setNotifyConfig(nc);
    notify("info", "通知升级配置已保存");
  };

  const saveVoice = async () => {
    await setVoice(voiceName);
    notify("info", "语音音色已保存");
  };

  return (
    <div className="console-mask" onClick={toggleSettings}>
      <section className="console" onClick={(e) => e.stopPropagation()}>
        <header className="cs-h">
          <span className="cs-sigil">◈</span>
          <span>核心控制台</span>
          <button className="mini-btn" onClick={toggleSettings}>
            ✕
          </button>
        </header>
        <div className="cs-body">
          <nav className="cs-nav">
            <button className={tab === "model" ? "on" : ""} onClick={() => setTab("model")}>
              模型链路
            </button>
            <button className={tab === "notify" ? "on" : ""} onClick={() => setTab("notify")}>
              通知升级
            </button>
            <button className={tab === "voice" ? "on" : ""} onClick={() => setTab("voice")}>
              语音合成
            </button>
          </nav>

          <div className="cs-main">
            {tab === "model" && (
              <div className="cs-sec">
                <p className="cs-tip">点击链路节点切换当前激活模型，立即生效并持久化。</p>
                {models?.profiles.map((p) => (
                  <button
                    key={p.id}
                    className={`mrow ${p.id === models.active ? "on" : ""}`}
                    onClick={() => void activate(p.id)}
                  >
                    <i className={`tier ${p.tier}`}>{p.tier === "local" ? "本地" : "云端"}</i>
                    <span className="mrow-name">{p.name}</span>
                    <code className="mrow-model">{p.model}</code>
                    <em className="mrow-state">
                      {p.id === models.active ? "● 激活" : p.enabled ? "待命" : "停用"}
                    </em>
                  </button>
                ))}
              </div>
            )}

            {tab === "notify" && nc && (
              <div className="cs-sec">
                <div className="cs-row">
                  <span>通知升级总开关</span>
                  <Toggle on={nc.enabled} onChange={(v) => setNc({ ...nc, enabled: v })} />
                </div>
                {LEVEL_NAMES.map((name, i) => (
                  <div className="cs-row" key={name}>
                    <span>{name}</span>
                    <Toggle
                      on={nc.levels_enabled[i] ?? false}
                      onChange={(v) => {
                        const le = [...nc.levels_enabled];
                        le[i] = v;
                        setNc({ ...nc, levels_enabled: le });
                      }}
                    />
                    <input
                      className="cs-num"
                      type="number"
                      min={1}
                      value={nc.timeouts_sec[i] ?? 0}
                      onChange={(e) => {
                        const ts = [...nc.timeouts_sec];
                        ts[i] = Number(e.target.value) || 0;
                        setNc({ ...nc, timeouts_sec: ts });
                      }}
                    />
                    <span className="cs-unit">秒后升级</span>
                  </div>
                ))}
                <button className="cs-save" onClick={() => void saveNotify()}>
                  保存升级链
                </button>
              </div>
            )}

            {tab === "voice" && (
              <div className="cs-sec">
                <p className="cs-tip">语音播报（L3）使用的音色名称，留空使用系统默认。</p>
                <input
                  className="cs-input"
                  value={voiceName}
                  placeholder="例如：Microsoft Xiaoxiao Online"
                  onChange={(e) => setVoiceName(e.target.value)}
                />
                <button className="cs-save" onClick={() => void saveVoice()}>
                  保存音色
                </button>
              </div>
            )}
          </div>
        </div>
      </section>
    </div>
  );
}