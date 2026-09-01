import { useEffect, useRef, useState } from "react";
import {
  clearRag,
  feishuSaveCredentials,
  feishuStart,
  feishuStop,
  gatewayStart,
  gatewayStop,
  getDbConnections,
  getFeishuStatus,
  getGatewayStatus,
  getImChannels,
  getMcpConfig,
  getModelConfig,
  getNotifyConfig,
  getRagState,
  getRuntimeConfig,
  getTokenSaverConfig,
  getTtsConfig,
  getVendorPresets,
  getVoice,
  getWechatStatus,
  indexRagDir,
  onFeishuStatus,
  onWechatQr,
  onWechatStatus,
  saveDbConnections,
  searchRag,
  setGatewayConfig,
  setMcpConfig,
  setModelConfig,
  setNotifyConfig,
  setRuntimeConfig,
  setTokenSaverConfig,
  setTtsConfig,
  setVoice,
  testModelProfile,
  wechatLogin,
  wechatLogout,
  wechatStart,
  wechatStop,
} from "../api";
import { reactiveSpeak, speakWithCloud, stopSpeaking } from "../voiceReactive";
import type { TtsConfig, UpdateInfo } from "../api";
import { KOKORO_VOICES, getKokoroVoices, getBrowserPathSetting, setBrowserPathSetting, updateCheck, updateInstall, onUpdateProgress } from "../api";
import {
  getNotifyStyle,
  getSfxVolume,
  isSfxEnabled,
  playSfx,
  previewNotify,
  setNotifyStyle,
  setSfxEnabled,
  setSfxVolume,
  type NotifyStyle,
  type SfxEvent,
} from "../utils/sound";

/** 设置页左侧导航分组：相关设置聚成一页，避免长滚动找不到项 */
const SETTING_PAGES = [
  { id: "model", label: "模型与推理", desc: "模型 · 运行时 · Token" },
  { id: "tools", label: "工具扩展", desc: "MCP 服务器" },
  { id: "gateway", label: "本地 AI 网关", desc: "HTTP 服务" },
  { id: "knowledge", label: "知识与数据", desc: "RAG · 数据库" },
  { id: "voice", label: "语音朗读", desc: "TTS 音色" },
  { id: "notify", label: "通知与音效", desc: "升级 · 提示音" },
  { id: "bots", label: "消息与机器人", desc: "IM · 微信 · 飞书" },
  { id: "about", label: "关于与更新", desc: "版本 · 自更新" },
] as const;
type SettingsPageId = (typeof SETTING_PAGES)[number]["id"];
import type {
  DbConnection,
  EmailConfig,
  McpConfig,
  ModelConfig,
  ModelProfile,
  ModelTier,
  ProviderKind,
  NotifyConfig,
  RagDoc,
  RagHit,
  RuntimeConfig,
  TokenSaverConfig,
  VendorPreset,
  WebhookConfig,
  WeChatStatus,
  FeishuStatus,
  GatewayConfig,
  GatewayStatus,
  ImChannelInfo,
} from "../types";

const field: React.CSSProperties = {
  display: "flex",
  flexDirection: "column",
  gap: 5,
  marginBottom: 12,
};
const label: React.CSSProperties = { fontSize: 12, color: "var(--text-dim)" };
const input: React.CSSProperties = {
  padding: "8px 10px",
  backgroundColor: "var(--input-solid)",
  border: "1px solid var(--border)",
  borderRadius: 8,
  color: "var(--text)",
  outline: "none",
};

export default function SettingsModal({ onClose }: { onClose: () => void }) {
  const [model, setModel] = useState<ModelConfig | null>(null);
  const [mcp, setMcp] = useState<McpConfig | null>(null);
  const [runtime, setRuntime] = useState<RuntimeConfig | null>(null);
  const [notify, setNotify] = useState<NotifyConfig | null>(null);
  const [tokenSaver, setTokenSaver] = useState<TokenSaverConfig | null>(null);
  const [mcpArgsText, setMcpArgsText] = useState("");
  // 桌面浏览器控制：手动指定的 Chrome/Edge 路径（留空自动探测）与探测状态
  const [browserPath, setBrowserPath] = useState("");
  // 应用内自更新状态
  const [upInfo, setUpInfo] = useState<UpdateInfo | null>(null);
  const [upStatus, setUpStatus] = useState("");
  const [upPct, setUpPct] = useState(-1); // -1 = 未在下载
  const [upBusy, setUpBusy] = useState(false);
  // 更新下载进度订阅（组件挂载期持续有效）
  useEffect(() => {
    let un: (() => void) | null = null;
    void onUpdateProgress((p) => {
      setUpPct(p.pct);
      setUpStatus(
        p.phase === "install"
          ? "下载完成，正在静默安装，白泽即将重启…"
          : `正在下载更新 ${p.pct}%`
      );
    }).then((f) => (un = f));
    return () => {
      if (un) un();
    };
  }, []);
  const [browserPathStatus, setBrowserPathStatus] = useState("");
  // 进入工具页时刷新一次探测状态
  useEffect(() => {
    void getBrowserPathSetting().then((r) => {
      setBrowserPath(r.custom ?? "");
      setBrowserPathStatus(
        r.resolved ? "当前使用：" + r.resolved : "未探测到可用浏览器（browser_act 将不可用）"
      );
    });
  }, []);
  const [status, setStatus] = useState("");
  // TTS 音色（系统中文语音），与聊天输入栏的音色选择共用同一持久化
  const [voices, setVoices] = useState<SpeechSynthesisVoice[]>([]);
  const [voiceName, setVoiceName] = useState("");
  // 语音模型配置：local=浏览器内置 | cloud=OpenAI 兼容 | doubao=豆包语音合成（火山引擎）
  const [ttsCfg, setTtsCfg] = useState<TtsConfig>({
    provider: "local",
    base_url: "",
    api_key: "",
    model: "",
    voice: "",
    db_app_id: "",
    db_token: "",
    db_speaker: "",
    db_speech_rate: 0,
  });
  const [ttsStatus, setTtsStatus] = useState("");
  const [error, setError] = useState("");
  // Kokoro 全量音色（后端动态加载 v1.1-zh 中文微调版 100+ 音色，失败用静态列表兜底）
  const [kokoroVoices, setKokoroVoices] = useState(KOKORO_VOICES);
  useEffect(() => {
    if (ttsCfg.provider !== "kokoro") return;
    let cancelled = false;
    void getKokoroVoices().then((list) => {
      // 双保险：动态列表再过滤一次，仅保留中文音色（zf_/zm_ 前缀）
      const zh = list?.filter((v) => /^z[fm]_/i.test(v.id));
      if (!cancelled && zh && zh.length) setKokoroVoices(zh);
    });
    return () => {
      cancelled = true;
    };
  }, [ttsCfg.provider]);
  // 厂商预设 + 连接测试结果（按 profile id）
  const [vendorPresets, setVendorPresets] = useState<VendorPreset[]>([]);
  const [testResults, setTestResults] = useState<Record<string, string>>({});

  // 加载系统语音列表 + 恢复已保存音色
  useEffect(() => {
    if (!("speechSynthesis" in window)) return;
    const load = () => {
      const all = speechSynthesis.getVoices();
      const zh = all.filter((v) => v.lang.toLowerCase().includes("zh"));
      setVoices(zh.length > 0 ? zh : all);
    };
    load();
    speechSynthesis.addEventListener("voiceschanged", load);
    void getVoice().then((n) => n && setVoiceName(n));
    void getTtsConfig()
      .then((c) => setTtsCfg((prev) => ({ ...prev, ...c })))
      .catch(() => {});
    return () => speechSynthesis.removeEventListener("voiceschanged", load);
  }, []);
  const saveVoice = (name: string) => {
    setVoiceName(name);
    void setVoice(name);
    // 通知聊天输入栏的音色选择器同步
    window.dispatchEvent(new CustomEvent("baize:voice-changed"));
  };
  const [testingId, setTestingId] = useState("");

  // 微信机器人
  const [wxStatus, setWxStatus] = useState<WeChatStatus | null>(null);
  const [wxQr, setWxQr] = useState("");
  const [wxBusy, setWxBusy] = useState(false);
  const [wxMsg, setWxMsg] = useState("");

  // 飞书机器人
  const [fsStatus, setFsStatus] = useState<FeishuStatus | null>(null);
  const [fsAppId, setFsAppId] = useState("");
  const [fsAppSecret, setFsAppSecret] = useState("");
  const [fsBusy, setFsBusy] = useState(false);
  const [fsMsg, setFsMsg] = useState("");
  // IM 消息总线（通道列表）
  const [imChannels, setImChannels] = useState<ImChannelInfo[]>([]);

  // 本地 AI 网关（OpenAI 兼容 HTTP 服务，开放模型路由 / 记忆 / 只读工具）
  const [gwStatus, setGwStatus] = useState<GatewayStatus | null>(null);
  const [gwConfig, setGwConfig] = useState<GatewayConfig>({
    enabled: false,
    port: 11436,
    token: "",
  });
  const [gwMsg, setGwMsg] = useState("");
  const [gwBusy, setGwBusy] = useState(false);
  useEffect(() => {
    getGatewayStatus()
      .then((s) => {
        setGwStatus(s);
        setGwConfig((prev) => ({ ...prev, enabled: s.enabled, port: s.port }));
      })
      .catch(() => {});
  }, []);
  const toggleGateway = async () => {
    setGwBusy(true);
    setGwMsg("");
    try {
      const s = gwStatus?.enabled ? await gatewayStop() : await gatewayStart();
      setGwStatus(s);
      setGwConfig((prev) => ({ ...prev, enabled: s.enabled, port: s.port }));
      setGwMsg(s.enabled ? "网关已启动" : "网关已停止");
    } catch (e) {
      setGwMsg(String(e));
    } finally {
      setGwBusy(false);
    }
  };
  const saveGateway = async () => {
    setGwBusy(true);
    setGwMsg("");
    try {
      const s = await setGatewayConfig(gwConfig);
      setGwStatus(s);
      setGwConfig((prev) => ({ ...prev, enabled: s.enabled, port: s.port }));
      setGwMsg("配置已保存" + (s.enabled ? "，网关运行中" : ""));
    } catch (e) {
      setGwMsg(String(e));
    } finally {
      setGwBusy(false);
    }
  };

  // 知识库（RAG）状态
  const [ragDocs, setRagDocs] = useState<RagDoc[]>([]);
  const [ragPath, setRagPath] = useState("");
  const [ragQuery, setRagQuery] = useState("");
  const [ragHits, setRagHits] = useState<RagHit[]>([]);
  const [ragMsg, setRagMsg] = useState("");

  // 音效：总开关 / 音量 / 弹窗提醒风格
  const [sfxOn, setSfxOn] = useState(isSfxEnabled());
  const [sfxVol, setSfxVol] = useState(getSfxVolume());
  const [notifyStyle, setNotifyStyleState] = useState<NotifyStyle>(getNotifyStyle());

  const toggleSfx = (on: boolean) => {
    setSfxOn(on);
    setSfxEnabled(on);
    if (on) playSfx("notify"); // 打开时给一声反馈
  };
  const changeVol = (v: number) => {
    setSfxVol(v);
    setSfxVolume(v);
  };
  const pickNotifyStyle = (s: NotifyStyle) => {
    setNotifyStyleState(s);
    setNotifyStyle(s);
    previewNotify(s); // 试听
  };

  // 左侧导航当前页
  const [page, setPage] = useState<SettingsPageId>("model");
  const pageContentRef = useRef<HTMLDivElement>(null);
  // 切换分类：右侧内容回到顶部
  useEffect(() => {
    pageContentRef.current?.scrollTo({ top: 0 });
  }, [page]);

  // 数据库连接配置
  const [dbConns, setDbConns] = useState<DbConnection[]>([]);
  const [dbName, setDbName] = useState("");
  const [dbConnStr, setDbConnStr] = useState("");
  const [dbMsg, setDbMsg] = useState("");

  const addDbConn = async () => {
    if (!dbName.trim() || !dbConnStr.trim()) return;
    const next = [...dbConns.filter((c) => c.name !== dbName.trim()), { name: dbName.trim(), connection: dbConnStr.trim() }];
    try {
      await saveDbConnections(next);
      setDbConns(next);
      setDbName("");
      setDbConnStr("");
      setDbMsg("已保存");
    } catch (e) {
      setDbMsg(String(e));
    }
  };
  const removeDbConn = async (name: string) => {
    const next = dbConns.filter((c) => c.name !== name);
    await saveDbConnections(next);
    setDbConns(next);
  };

  useEffect(() => {
    getModelConfig().then(setModel).catch((e) => setError(String(e)));
    getVendorPresets().then(setVendorPresets).catch(() => {});
    getMcpConfig()
      .then((c) => {
        setMcp(c);
        setMcpArgsText(c.args.join(", "));
      })
      .catch((e) => setError(String(e)));
    getRuntimeConfig().then(setRuntime).catch((e) => setError(String(e)));
    getNotifyConfig().then(setNotify).catch((e) => setError(String(e)));
    getTokenSaverConfig().then(setTokenSaver).catch((e) => setError(String(e)));
    getRagState().then(setRagDocs).catch(() => {});
    getDbConnections().then(setDbConns).catch(() => {});
  }, []);

  // 微信：加载状态并订阅事件（状态变化 + 扫码二维码）
  useEffect(() => {
    getWechatStatus().then(setWxStatus).catch(() => {});
    let offStatus: () => void = () => {};
    let offQr: () => void = () => {};
    onWechatStatus(setWxStatus).then((f) => (offStatus = f));
    onWechatQr(setWxQr).then((f) => (offQr = f));
    return () => {
      offStatus();
      offQr();
    };
  }, []);

  // 飞书 + IM 总线：加载状态 / 通道列表并订阅事件
  useEffect(() => {
    getImChannels().then(setImChannels).catch(() => {});
    getFeishuStatus()
      .then((s) => {
        setFsStatus(s);
        setFsAppId(s.app_id ?? "");
      })
      .catch(() => {});
    let off: () => void = () => {};
    onFeishuStatus((s) => {
      setFsStatus(s);
      setFsAppId(s.app_id ?? "");
    }).then((f) => (off = f));
    return () => off();
  }, []);

  const doWxLogin = async () => {
    setWxBusy(true);
    setWxMsg("");
    setWxQr("");
    try {
      const s = await wechatLogin();
      setWxStatus(s);
      if (s.status !== "connected") setWxMsg("登录未完成：已取消或超时");
    } catch (e) {
      setWxMsg(String(e));
    } finally {
      setWxBusy(false);
    }
  };

  const doWxStop = async () => {
    setWxBusy(true);
    try {
      setWxStatus(await wechatStop());
    } catch (e) {
      setWxMsg(String(e));
    } finally {
      setWxBusy(false);
    }
  };

  const doWxStart = async () => {
    setWxBusy(true);
    try {
      setWxStatus(await wechatStart());
    } catch (e) {
      setWxMsg(String(e));
    } finally {
      setWxBusy(false);
    }
  };

  const doWxLogout = async () => {
    setWxBusy(true);
    setWxQr("");
    try {
      setWxStatus(await wechatLogout());
    } catch (e) {
      setWxMsg(String(e));
    } finally {
      setWxBusy(false);
    }
  };

  const doWxCancel = async () => {
    setWxQr("");
    try {
      setWxStatus(await wechatStop());
    } catch (e) {
      setWxMsg(String(e));
    }
  };

  const doFsSave = async () => {
    if (!fsAppId.trim() || !fsAppSecret.trim()) {
      setFsMsg("请填写 App ID 和 App Secret");
      return;
    }
    setFsBusy(true);
    setFsMsg("");
    try {
      await feishuSaveCredentials(fsAppId.trim(), fsAppSecret.trim());
      const s = await feishuStart();
      setFsStatus(s);
      setFsAppId(s.app_id ?? "");
    } catch (e) {
      setFsMsg(String(e));
    } finally {
      setFsBusy(false);
    }
  };

  const doFsStart = async () => {
    setFsBusy(true);
    try {
      setFsStatus(await feishuStart());
    } catch (e) {
      setFsMsg(String(e));
    } finally {
      setFsBusy(false);
    }
  };

  const doFsStop = async () => {
    setFsBusy(true);
    try {
      setFsStatus(await feishuStop());
    } catch (e) {
      setFsMsg(String(e));
    } finally {
      setFsBusy(false);
    }
  };

  const loadRag = () => getRagState().then(setRagDocs).catch((e) => setRagMsg(String(e)));

  const doIndexRag = async () => {
    if (!ragPath.trim()) return;
    setRagMsg("索引中…");
    try {
      const r = await indexRagDir(ragPath.trim());
      setRagMsg(`已索引 ${r.chunks} 个分块`);
      await loadRag();
    } catch (e) {
      setRagMsg(String(e));
    }
  };

  const doClearRag = async () => {
    try {
      await clearRag();
      await loadRag();
      setRagHits([]);
      setRagMsg("已清空知识库");
    } catch (e) {
      setRagMsg(String(e));
    }
  };

  const doSearchRag = async () => {
    if (!ragQuery.trim()) return;
    const r = await searchRag(ragQuery.trim());
    setRagHits(r.hits);
  };

  const updModel = (patch: Partial<ModelConfig>) => setModel((c) => (c ? { ...c, ...patch } : c));
  const updProfile = (idx: number, patch: Partial<ModelProfile>) =>
    setModel((c) =>
      c
        ? { ...c, profiles: c.profiles.map((p, i) => (i === idx ? { ...p, ...patch } : p)) }
        : c
    );
  const addProfile = () =>
    setModel((c) =>
      c
        ? {
            ...c,
            profiles: [
              ...c.profiles,
              {
                id: `model-${Date.now()}`,
                name: "新模型",
                tier: "cloud" as ModelTier,
                kind: "openai" as ProviderKind,
                base_url: "https://",
                api_key: "",
                model: "",
                vision_model: null,
                embedding_model: null,
                enabled: true,
                has_key: false,
                multimodal: false,
              },
            ],
          }
        : c
    );
  const addProfileFromPreset = (presetId: string) => {
    const preset = vendorPresets.find((v) => v.id === presetId);
    if (!preset) return;
    setModel((c) =>
      c
        ? {
            ...c,
            profiles: [
              ...c.profiles,
              {
                id: `model-${Date.now()}`,
                name: preset.name,
                tier: preset.tier,
                kind: preset.kind,
                base_url: preset.base_url,
                api_key: "",
                model: preset.models[0] ?? "",
                vision_model: null,
                embedding_model: null,
                enabled: true,
                has_key: false,
                multimodal: false,
              },
            ],
          }
        : c
    );
  };
  const doTestProfile = async (id: string) => {
    setTestingId(id);
    setTestResults((r) => ({ ...r, [id]: "测试中…" }));
    try {
      // 先保存当前表单（含新填的 API Key），再测试，确保测的是最新配置而非旧缓存
      if (model) await setModelConfig(model);
      const result = await testModelProfile(id);
      setTestResults((r) => ({ ...r, [id]: result }));
    } catch (e) {
      setTestResults((r) => ({ ...r, [id]: `失败：${String(e)}` }));
    } finally {
      setTestingId("");
    }
  };
  const clearProfileKey = (idx: number) =>
    setModel((c) =>
      c
        ? {
            ...c,
            profiles: c.profiles.map((p, i) =>
              i === idx ? { ...p, api_key: "", has_key: false } : p
            ),
          }
        : c
    );
  const removeProfile = (idx: number) =>
    setModel((c) => (c ? { ...c, profiles: c.profiles.filter((_, i) => i !== idx) } : c));
  const updMcp = (patch: Partial<McpConfig>) => setMcp((c) => (c ? { ...c, ...patch } : c));
  const updNotify = (patch: Partial<NotifyConfig>) => setNotify((c) => (c ? { ...c, ...patch } : c));
  const updTokenSaver = (patch: Partial<TokenSaverConfig>) =>
    setTokenSaver((c) => (c ? { ...c, ...patch } : c));

  const apply = async () => {
    setStatus("");
    setError("");
    try {
      if (model) {
        await setModelConfig(model);
        // 通知输入框下拉等监听方刷新模型列表
        window.dispatchEvent(new CustomEvent("model-config-changed"));
      }
      if (mcp) {
        const args = mcpArgsText
          .split(/[,，]/)
          .map((s) => s.trim())
          .filter(Boolean);
        await setMcpConfig({ ...mcp, args });
      }
      if (runtime) {
        await setRuntimeConfig(runtime);
      }
      if (notify) {
        await setNotifyConfig(notify);
      }
      if (tokenSaver) {
        await setTokenSaverConfig(tokenSaver);
      }
      setStatus("✓ 已应用，立即生效");
    } catch (e) {
      setError(String(e));
    }
  };

  const loading = !model || !mcp;

  return (
    <div className="rpanel">
      <div
        style={{
          flexShrink: 0,
          display: "flex",
          alignItems: "center",
          gap: 12,
          padding: "12px 16px",
          borderBottom: "1px solid var(--border-soft)",
          background: "var(--bg)",
        }}
      >
        <h3 style={{ margin: 0 }}>设置</h3>
        <span
          style={{
            flex: 1,
            minWidth: 0,
            fontSize: 12,
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
            visibility: status || error ? "visible" : "hidden",
            color: error ? "#fca5a5" : "var(--success)",
          }}
        >
          {error || status || " "}
        </span>
        <button className="acui-btn" onClick={onClose}>
          关闭
        </button>
        <button className="acui-btn primary" onClick={() => void apply()} disabled={loading}>
          应用
        </button>
      </div>
      <div
        style={{
          flex: 1,
          minHeight: 0,
          display: "flex",
          overflow: "hidden",
        }}
      >
        {/* 左侧分类导航 */}
        <nav className="settings-nav">
          {SETTING_PAGES.map((p) => (
            <button
              key={p.id}
              className={`settings-nav-item${page === p.id ? " active" : ""}`}
              onClick={() => setPage(p.id)}
              title={p.desc}
            >
              <span className="settings-nav-text">
                <span className="settings-nav-label">{p.label}</span>
                <span className="settings-nav-desc">{p.desc}</span>
              </span>
            </button>
          ))}
        </nav>
        <div
          ref={pageContentRef}
          style={{
            flex: 1,
            minWidth: 0,
            minHeight: 0,
            overflowY: "auto",
            padding: 20,
          }}
        >
        {loading ? (
          <div style={{ color: "var(--text-dim)" }}>{error || "加载配置中…"}</div>
        ) : (
          <>
            {/* key 随 page 变化：切换分类时整组重挂载，当前页轻声淡入 */}
            <div key={page}>
            {/* 模型与推理页 */}
            <div className="settings-page" style={{ display: page === "model" ? undefined : "none" }}>
            {/* ============ 模型 ============ */}
            <section>
              <h4 style={{ margin: "4px 0 8px", color: "var(--cyan)" }}>
                模型列表（多厂商，可随时切换）
              </h4>
              <div
                style={{
                  display: "flex",
                  gap: 8,
                  alignItems: "center",
                  marginBottom: 12,
                }}
              >
                <select
                  style={{ ...input, flex: 1, marginBottom: 0 }}
                  value=""
                  onChange={(e) => {
                    if (e.target.value) addProfileFromPreset(e.target.value);
                    e.target.value = "";
                  }}
                >
                  <option value="">＋ 从厂商预设添加…</option>
                  {vendorPresets.map((v) => (
                    <option key={v.id} value={v.id}>
                      {v.name}（{v.kind === "ollama" ? "本地" : v.kind}）
                    </option>
                  ))}
                </select>
              </div>
              {model.profiles.map((p, i) => {
                const isActive = model.active === p.id;
                const testResult = testResults[p.id];
                return (
                  <div
                    key={p.id}
                    style={{
                      border: isActive ? "1px solid var(--cyan)" : "1px solid var(--border)",
                      borderRadius: 10,
                      padding: 10,
                      marginBottom: 10,
                      background: isActive ? "rgba(34,211,238,0.06)" : "transparent",
                    }}
                  >
                    <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 8 }}>
                      <input
                        type="radio"
                        name="active-model"
                        checked={isActive}
                        onChange={() => updModel({ active: p.id })}
                        title="设为当前使用模型"
                      />
                      <input
                        style={{ ...input, flex: 1, width: 60, marginBottom: 0 }}
                        value={p.name}
                        onChange={(e) => updProfile(i, { name: e.target.value })}
                        placeholder="显示名（如 豆包）"
                      />
                      <select
                        style={{ ...input, width: 74, padding: "7px 6px", marginBottom: 0 }}
                        value={p.tier}
                        onChange={(e) => {
                          const tier = e.target.value as ModelTier;
                          // 本地强制走 Ollama 协议；切云端保持原有 kind
                          updProfile(i, tier === "local" ? { tier, kind: "ollama" } : { tier });
                        }}
                      >
                        <option value="cloud">云端</option>
                        <option value="local">本地</option>
                      </select>
                      <label
                        style={{
                          display: "flex",
                          alignItems: "center",
                          gap: 4,
                          fontSize: 12,
                          whiteSpace: "nowrap",
                          color: "var(--text-dim)",
                        }}
                      >
                        <input
                          type="checkbox"
                          checked={p.enabled}
                          onChange={(e) => updProfile(i, { enabled: e.target.checked })}
                        />
                        启用
                      </label>
                      <label
                        style={{
                          display: "flex",
                          alignItems: "center",
                          gap: 4,
                          fontSize: 12,
                          whiteSpace: "nowrap",
                          color: "var(--text-dim)",
                          cursor: "pointer",
                        }}
                        title="该模型本身支持图片输入（如 GPT-4o / Claude / Gemini / 多模态本地模型），勾选后视觉调用直接复用它，无需再单独配置视觉模型"
                      >
                        <input
                          type="checkbox"
                          checked={p.multimodal ?? false}
                          onChange={(e) => updProfile(i, { multimodal: e.target.checked })}
                        />
                        多模态
                      </label>
                      <button
                        onClick={() => removeProfile(i)}
                        title="删除该模型"
                        style={{
                          background: "transparent",
                          border: "1px solid var(--border)",
                          color: "var(--text-dim)",
                          borderRadius: 8,
                          width: 26,
                          height: 26,
                          cursor: "pointer",
                          fontSize: 14,
                          lineHeight: 1,
                        }}
                      >
                        ×
                      </button>
                    </div>
                    {p.tier === "cloud" && (
                      <div style={field}>
                        <span style={label}>协议类型</span>
                        <select
                          style={input}
                          value={p.kind}
                          onChange={(e) => updProfile(i, { kind: e.target.value as ProviderKind })}
                        >
                          <option value="openai">OpenAI 兼容（DeepSeek/豆包/通义/Kimi/GLM/OpenRouter）</option>
                          <option value="anthropic">Anthropic（Claude）</option>
                          <option value="gemini">Google Gemini</option>
                        </select>
                      </div>
                    )}
                    <div style={field}>
                      <span style={label}>{p.tier === "local" ? "服务地址" : "API 地址"}</span>
                      <input
                        style={input}
                        value={p.base_url}
                        onChange={(e) => updProfile(i, { base_url: e.target.value })}
                        placeholder={p.tier === "local" ? "http://127.0.0.1:11434" : "https://api.xxx.com/v1"}
                      />
                    </div>
                    <div style={field}>
                      <span style={label}>模型名</span>
                      <input
                        style={input}
                        value={p.model}
                        onChange={(e) => updProfile(i, { model: e.target.value })}
                        placeholder={p.tier === "local" ? "qwen2.5:7b" : "deepseek-chat"}
                      />
                    </div>
                    {p.tier === "cloud" && (
                      <div style={field}>
                        <span style={label}>视觉模型（可选，留空复用「运行时模型」里的视觉配置）</span>
                        <input
                          style={input}
                          value={p.vision_model || ""}
                          onChange={(e) => updProfile(i, { vision_model: e.target.value || null })}
                          placeholder="如 deepseek-v4-flash-vision-exp"
                        />
                      </div>
                    )}
                    {p.tier === "local" && (
                      <div style={field}>
                        <span style={label}>Embedding 模型（可选，用于语义记忆 / 知识库）</span>
                        <input
                          style={input}
                          value={p.embedding_model || ""}
                          onChange={(e) => updProfile(i, { embedding_model: e.target.value || null })}
                          placeholder="如 nomic-embed-text"
                        />
                      </div>
                    )}
                    {p.tier === "cloud" && (
                      <div style={field}>
                        <span style={label}>
                          API Key{p.has_key ? "（已保存密钥，留空则保留原密钥）" : ""}
                        </span>
                        <div style={{ display: "flex", gap: 6 }}>
                          <input
                            style={{ ...input, flex: 1 }}
                            type="password"
                            value={p.api_key}
                            onChange={(e) => updProfile(i, { api_key: e.target.value })}
                            placeholder={p.has_key ? "••••••••（已保存）" : "sk-..."}
                          />
                          {p.has_key && (
                            <button
                              onClick={() => clearProfileKey(i)}
                              title="清除已保存的 API Key"
                              style={{
                                background: "transparent",
                                border: "1px solid var(--border)",
                                color: "var(--text-dim)",
                                borderRadius: 8,
                                padding: "0 10px",
                                cursor: "pointer",
                                fontSize: 12,
                                whiteSpace: "nowrap",
                              }}
                            >
                              清除
                            </button>
                          )}
                        </div>
                      </div>
                    )}
                    <div style={{ display: "flex", alignItems: "center", gap: 8, marginTop: 2 }}>
                      <button
                        onClick={() => doTestProfile(p.id)}
                        disabled={testingId === p.id}
                        title="保存当前配置并发送一条最小消息验证连接"
                        style={{
                          background: "transparent",
                          border: "1px solid var(--cyan)",
                          color: "var(--cyan)",
                          borderRadius: 8,
                          padding: "4px 12px",
                          cursor: testingId === p.id ? "default" : "pointer",
                          fontSize: 12,
                          opacity: testingId === p.id ? 0.6 : 1,
                        }}
                      >
                        {testingId === p.id ? "测试中…" : "测试连接"}
                      </button>
                      {testResult ? (
                        <span
                          style={{
                            fontSize: 11,
                            color: testResult.startsWith("失败") ? "#f87171" : "var(--text-dim)",
                            wordBreak: "break-all",
                          }}
                        >
                          {testResult}
                        </span>
                      ) : null}
                    </div>
                  </div>
                );
              })}

              <button
                onClick={addProfile}
                style={{
                  width: "100%",
                  padding: "8px 0",
                  background: "transparent",
                  border: "1px dashed var(--border)",
                  color: "var(--text-dim)",
                  borderRadius: 8,
                  cursor: "pointer",
                  fontSize: 13,
                }}
              >
                ＋ 添加模型
              </button>
              <div style={{ fontSize: 11, color: "var(--text-faint)", marginTop: 8 }}>
                选中「设为当前使用模型」后点底部「保存」生效；也可在聊天输入框直接切换。调用失败时按列表顺序自动降级到下一个可用模型。
              </div>
            </section>

            {/* ============ 运行时模型 ============ */}
            <section style={{ borderTop: "1px solid var(--border-soft)", paddingTop: 12, marginTop: 8 }}>
              <h4 style={{ margin: "4px 0 8px", color: "#22d3ee" }}>运行时模型（Embedding / 视觉）</h4>
              <label style={{ display: "flex", gap: 8, alignItems: "center", marginBottom: 12, fontSize: 13 }}>
                <input
                  type="checkbox"
                  checked={runtime?.vision_enabled ?? true}
                  onChange={(e) =>
                    setRuntime((r) => (r ? { ...r, vision_enabled: e.target.checked } : r))
                  }
                />
                启用视觉模型（关闭后 GUI 定位走 OCR 兜底、图片描述降级）
              </label>

              <div style={field}>
                <span style={label}>Embedding 模型（语义记忆 / 知识库）</span>
                <input
                  style={input}
                  value={runtime?.embed_model ?? ""}
                  onChange={(e) => setRuntime((r) => (r ? { ...r, embed_model: e.target.value } : r))}
                  placeholder="nomic-embed-text"
                />
              </div>

              {runtime?.vision_enabled !== false && (
                <>
                  <div style={field}>
                    <span style={label}>视觉后端</span>
                    <select
                      style={input}
                      value={runtime?.vision_provider ?? "ollama"}
                      onChange={(e) => setRuntime((r) => (r ? { ...r, vision_provider: e.target.value } : r))}
                    >
                      <option value="ollama">本地 Ollama</option>
                      <option value="deepseek">DeepSeek 云端</option>
                    </select>
                  </div>
                  <div style={field}>
                    <span style={label}>视觉模型（视觉 grounding）</span>
                    <input
                      style={input}
                      value={runtime?.vision_model ?? ""}
                      onChange={(e) => setRuntime((r) => (r ? { ...r, vision_model: e.target.value } : r))}
                      placeholder={runtime?.vision_provider === "deepseek" ? "deepseek-v4-flash-vision-exp" : "llava"}
                    />
                  </div>
                  <div style={{ fontSize: 11, color: "var(--text-faint)", marginTop: -6, marginBottom: 8 }}>
                    {runtime?.vision_provider === "deepseek"
                      ? "复用主 LLM 的 DeepSeek 云端 base_url / API Key；模型名填视觉模型 ID"
                      : "需本地 Ollama 已启动并拉取对应视觉模型（如 ollama pull llava）"}
                  </div>
                </>
              )}
            </section>

            {/* ============ Token 节约 ============ */}
            {tokenSaver && (
              <section style={{ borderTop: "1px solid var(--border-soft)", paddingTop: 12, marginTop: 8 }}>
                <h4 style={{ margin: "4px 0 8px", color: "#f59e0b" }}>Token 节约机制</h4>
                <label style={{ display: "flex", gap: 8, alignItems: "center", marginBottom: 10, fontSize: 13 }}>
                  <input
                    type="checkbox"
                    checked={tokenSaver.enabled}
                    onChange={(e) => updTokenSaver({ enabled: e.target.checked })}
                  />
                  启用 Token 节约（长上下文压缩 + 工具结果截断）
                </label>

                {tokenSaver.enabled && (
                  <>
                    <div style={{ padding: "10px 12px", background: "var(--box-solid)", borderRadius: 8 }}>
                      <label style={{ display: "flex", gap: 8, alignItems: "center", marginBottom: 8, fontSize: 13 }}>
                        <input
                          type="checkbox"
                          checked={tokenSaver.auto_compress}
                          onChange={(e) => updTokenSaver({ auto_compress: e.target.checked })}
                        />
                        长对话自动压缩（超阈值时把早期消息摘要化）
                      </label>
                      <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 8 }}>
                        <div style={field}>
                          <span style={label}>压缩触发阈值（历史总字符数）</span>
                          <input
                            style={input}
                            type="number"
                            min={2000}
                            value={tokenSaver.compress_threshold_chars}
                            onChange={(e) =>
                              updTokenSaver({ compress_threshold_chars: Math.max(2000, Number(e.target.value)) })
                            }
                          />
                        </div>
                        <div style={field}>
                          <span style={label}>保留最近原文（字符数）</span>
                          <input
                            style={input}
                            type="number"
                            min={500}
                            value={tokenSaver.keep_recent_chars}
                            onChange={(e) =>
                              updTokenSaver({ keep_recent_chars: Math.max(500, Number(e.target.value)) })
                            }
                          />
                        </div>
                      </div>
                      <label style={{ display: "flex", gap: 8, alignItems: "center", marginTop: 4, fontSize: 13 }}>
                        <input
                          type="checkbox"
                          checked={tokenSaver.local_only_compress}
                          onChange={(e) => updTokenSaver({ local_only_compress: e.target.checked })}
                        />
                        压缩只用本地模型（免费，不产生云端费用）
                      </label>
                    </div>

                    <div style={{ marginTop: 8, padding: "10px 12px", background: "var(--box-solid)", borderRadius: 8 }}>
                      <div style={field}>
                        <span style={label}>单条工具结果最大字符数（超出做「首尾保留」截断，0 = 不截断）</span>
                        <input
                          style={input}
                          type="number"
                          min={0}
                          value={tokenSaver.max_tool_result_chars}
                          onChange={(e) =>
                            updTokenSaver({ max_tool_result_chars: Math.max(0, Number(e.target.value)) })
                          }
                        />
                      </div>
                      <div style={{ fontSize: 11, color: "var(--text-faint)", marginTop: -6 }}>
                        只限制喂给模型的内容；审计记录与前端展示仍保留完整工具结果，保证透明。
                      </div>
                    </div>

                    <div style={{ marginTop: 8, padding: "10px 12px", background: "var(--box-solid)", borderRadius: 8 }}>
                      <label style={{ display: "flex", gap: 8, alignItems: "center", fontSize: 13 }}>
                        <input
                          type="checkbox"
                          checked={tokenSaver.concise_reply}
                          onChange={(e) => updTokenSaver({ concise_reply: e.target.checked })}
                        />
                        精简回复（结论先行、不复述工具结果、不客套收尾，省输出 token）
                      </label>
                      <div style={{ fontSize: 11, color: "var(--text-faint)", marginTop: 4 }}>
                        关闭后恢复完整详细回复；报告/文档类长内容始终写入文档窗口，不受影响。
                      </div>
                    </div>
                  </>
                )}
              </section>
            )}
            </div>

            {/* 工具扩展页 */}
            <div className="settings-page" style={{ display: page === "tools" ? undefined : "none" }}>
            {/* ============ MCP ============ */}
            <section style={{ borderTop: "1px solid var(--border-soft)", paddingTop: 12, marginTop: 8 }}>
              <h4 style={{ margin: "4px 0 8px", color: "#f59e0b" }}>MCP 工具服务器</h4>
              <label style={{ display: "flex", gap: 8, alignItems: "center", marginBottom: 10, fontSize: 13 }}>
                <input
                  type="checkbox"
                  checked={mcp.enabled}
                  onChange={(e) => updMcp({ enabled: e.target.checked })}
                />
                启用 MCP
              </label>
              <div style={field}>
                <span style={label}>启动命令</span>
                <input
                  style={input}
                  value={mcp.command}
                  onChange={(e) => updMcp({ command: e.target.value })}
                  placeholder="npx"
                />
              </div>
              <div style={field}>
                <span style={label}>参数（逗号分隔；目录参数即允许访问的目录）</span>
                <input
                  style={input}
                  value={mcpArgsText}
                  onChange={(e) => setMcpArgsText(e.target.value)}
                  placeholder="-y, @modelcontextprotocol/server-filesystem, D:\, C:\Users"
                />
              </div>
              <div style={{ fontSize: 11, color: "var(--text-faint)", marginTop: -6, marginBottom: 8 }}>
                示例：`-y, @modelcontextprotocol/server-filesystem, D:\, C:\Users\xxx` 允许访问 D 盘和用户目录
              </div>
            </section>

            {/* ============ 桌面浏览器控制 ============ */}
            <section style={{ borderTop: "1px solid var(--border-soft)", paddingTop: 12, marginTop: 8 }}>
              <h4 style={{ margin: "4px 0 8px", color: "var(--text)" }}>桌面浏览器控制</h4>
              <div style={{ display: "flex", gap: 8, marginBottom: 8 }}>
                <input
                  style={{ ...input, flex: 1 }}
                  value={browserPath}
                  onChange={(e) => setBrowserPath(e.target.value)}
                  placeholder="留空自动探测：注册表 / 默认安装目录（Chrome / Edge / Chromium / Brave）"
                />
                <button
                  className="acui-btn primary"
                  onClick={async () => {
                    try {
                      const r = await setBrowserPathSetting(browserPath);
                      if (!r.ok) {
                        setBrowserPathStatus(
                          (r.error ?? "保存失败") +
                            (r.resolved ? "；当前使用：" + r.resolved : "")
                        );
                      } else {
                        setBrowserPathStatus(
                          (r.note ?? "已保存") + (r.resolved ? "，当前使用：" + r.resolved : "")
                        );
                      }
                    } catch (e) {
                      setBrowserPathStatus("保存失败: " + e);
                    }
                  }}
                >
                  保存
                </button>
              </div>
              <div style={{ fontSize: 11, color: "var(--text-faint)" }}>
                {browserPathStatus ||
                  "browser_act 桌面浏览器控制将使用此路径驱动真实 Chrome/Edge（保留登录态）。留空时按「注册表 → 默认安装目录」自动探测。"}
              </div>
            </section>
            </div>

            {/* 本地 AI 网关页 */}
            <div className="settings-page" style={{ display: page === "gateway" ? undefined : "none" }}>
            <section style={{ borderTop: "1px solid var(--border-soft)", paddingTop: 12, marginTop: 8 }}>
              <h4 style={{ margin: "4px 0 8px", color: "#22d3ee" }}>本地 AI 网关</h4>
              <div style={{ fontSize: 12, color: "var(--text-dim)", marginBottom: 12, lineHeight: 1.6 }}>
                在 <code>127.0.0.1</code> 起一个 OpenAI 兼容 HTTP 服务，让 VS Code 插件、Obsidian 等任何
                OpenAI 兼容客户端都能复用白泽的模型路由、长期记忆与本机只读工具。仅监听回环地址，不暴露公网。
              </div>

              {gwStatus && (
                <div
                  style={{
                    display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap",
                    padding: 10, borderRadius: 8, marginBottom: 12,
                    background: "transparent", border: "1px solid var(--border)",
                  }}
                >
                  <span style={{ fontSize: 12, color: gwStatus.enabled ? "var(--success)" : "var(--text-dim)" }}>
                    {gwStatus.enabled ? "● 运行中" : "○ 已停止"}
                  </span>
                  <span style={{ fontSize: 12, color: "var(--text)" }}>{gwStatus.base_url}</span>
                  {gwStatus.enabled && (
                    <button
                      className="acui-btn"
                      onClick={() => {
                        void navigator.clipboard.writeText(gwStatus.base_url);
                        setGwMsg("已复制 base_url");
                      }}
                    >
                      复制
                    </button>
                  )}
                </div>
              )}

              <div style={{ display: "flex", gap: 8, marginBottom: 12 }}>
                <button className="acui-btn primary" onClick={() => void toggleGateway()} disabled={gwBusy}>
                  {gwStatus?.enabled ? "停止网关" : "启动网关"}
                </button>
              </div>

              <div style={field}>
                <span style={label}>监听端口（默认 11436，避开 Ollama 的 11434）</span>
                <input
                  style={{ ...input, maxWidth: 180 }}
                  type="number"
                  value={gwConfig.port}
                  onChange={(e) => setGwConfig({ ...gwConfig, port: Number(e.target.value) || 11436 })}
                />
              </div>

              <div style={field}>
                <span style={label}>访问令牌（Bearer；留空则不校验，保存会覆盖为当前输入值）</span>
                <input
                  style={input}
                  type="password"
                  value={gwConfig.token}
                  onChange={(e) => setGwConfig({ ...gwConfig, token: e.target.value })}
                  placeholder={gwStatus?.has_token ? "已设置令牌（留空并保存将清除）" : "留空 = 无需令牌"}
                />
              </div>

              <div style={{ display: "flex", gap: 8, alignItems: "center", marginTop: 4 }}>
                <button className="acui-btn" onClick={() => void saveGateway()} disabled={gwBusy}>
                  保存配置
                </button>
                {gwMsg && <span style={{ fontSize: 12, color: "var(--text-dim)" }}>{gwMsg}</span>}
              </div>

              <div style={{ fontSize: 11, color: "var(--text-faint)", marginTop: 12, lineHeight: 1.6 }}>
                端点：/v1/chat/completions（对话）· /v1/models（模型列表）· /api/memory/remember · /api/memory/search · /api/tools · /api/tools/execute（仅只读）
              </div>
            </section>
            </div>

            {/* 关于与更新页 */}
            <div className="settings-page" style={{ display: page === "about" ? undefined : "none" }}>
            {/* ============ 关于与更新 ============ */}
            <section style={{ borderTop: "1px solid var(--border-soft)", paddingTop: 12, marginTop: 8 }}>
              <h4 style={{ margin: "4px 0 8px", color: "var(--text)" }}>关于与更新</h4>
              <div style={{ display: "flex", gap: 8, alignItems: "center", marginBottom: 8, flexWrap: "wrap" }}>
                <span style={{ fontSize: 12, color: "var(--text-dim)" }}>
                  当前版本 v{upInfo?.current ?? "…"}
                  {upInfo ? (upInfo.has_update ? ` · 发现新版本 v${upInfo.latest}` : " · 已是最新") : ""}
                </span>
                <span className="side-spacer" style={{ flex: 1 }} />
                <button
                  className="acui-btn"
                  disabled={upBusy}
                  onClick={async () => {
                    setUpBusy(true);
                    setUpStatus("正在检查更新…");
                    try {
                      const info = await updateCheck();
                      setUpInfo(info);
                      setUpStatus(
                        info.has_update
                          ? `发现新版本 v${info.latest}，可下载更新`
                          : "当前已是最新版本"
                      );
                    } catch (e) {
                      setUpStatus("检查失败: " + e);
                    }
                    setUpBusy(false);
                  }}
                >
                  检查更新
                </button>
              </div>
              {upInfo?.has_update && (
                <div
                  style={{
                    fontSize: 12,
                    color: "var(--text-dim)",
                    marginBottom: 8,
                    whiteSpace: "pre-wrap",
                    maxHeight: 130,
                    overflowY: "auto",
                    background: "rgba(255,255,255,0.03)",
                    borderRadius: 8,
                    padding: 8,
                  }}
                >
                  {upInfo.notes || "（无更新说明）"}
                </div>
              )}
              {upPct >= 0 && (
                <div style={{ marginBottom: 8 }}>
                  <div
                    style={{
                      height: 6,
                      borderRadius: 3,
                      background: "rgba(255,255,255,0.08)",
                      overflow: "hidden",
                    }}
                  >
                    <div
                      style={{
                        height: "100%",
                        width: `${upPct}%`,
                        background: "linear-gradient(90deg,#22d3ee,#3b82f6)",
                        transition: "width .3s",
                      }}
                    />
                  </div>
                  <div style={{ fontSize: 11, color: "var(--text-faint)", marginTop: 4 }}>
                    {upPct >= 100 ? "下载完成，正在静默安装并重启…" : `下载中 ${upPct}%`}
                  </div>
                </div>
              )}
              {upInfo?.has_update && upPct < 0 && (
                <button
                  className="acui-btn primary"
                  disabled={upBusy}
                  onClick={async () => {
                    setUpBusy(true);
                    setUpStatus("正在下载更新…");
                    try {
                      await updateInstall();
                      setUpStatus("安装器已启动，白泽即将退出并完成更新…");
                    } catch (e) {
                      setUpStatus("更新失败: " + e);
                      setUpPct(-1);
                      setUpBusy(false);
                    }
                  }}
                >
                  下载并安装更新
                </button>
              )}
              <div style={{ fontSize: 11, color: "var(--text-faint)", marginTop: 6 }}>{upStatus}</div>
            </section>
            </div>

            {/* 知识与数据页 */}
            <div className="settings-page" style={{ display: page === "knowledge" ? undefined : "none" }}>
            {/* ============ 知识库（RAG） ============ */}
            <section style={{ borderTop: "1px solid var(--border-soft)", paddingTop: 12, marginTop: 8 }}>
              <h4 style={{ margin: "4px 0 8px", color: "#34d399" }}>知识库（RAG）</h4>

              <div style={{ display: "flex", gap: 8, marginBottom: 8 }}>
                <input
                  style={{ ...input, flex: 1 }}
                  value={ragPath}
                  onChange={(e) => setRagPath(e.target.value)}
                  placeholder="要索引的目录路径，如 D:\docs"
                />
                <button className="acui-btn primary" onClick={() => void doIndexRag()}>
                  索引
                </button>
              </div>

              <div style={{ display: "flex", gap: 8, marginBottom: 8 }}>
                <input
                  style={{ ...input, flex: 1 }}
                  value={ragQuery}
                  onChange={(e) => setRagQuery(e.target.value)}
                  placeholder="搜索测试：输入关键词"
                />
                <button className="acui-btn" onClick={() => void doSearchRag()}>
                  搜索
                </button>
              </div>

              {ragMsg && <div style={{ fontSize: 11, color: "var(--text-faint)", marginBottom: 8 }}>{ragMsg}</div>}

              <div style={{ fontSize: 12, color: "var(--text-dim)", marginBottom: 6 }}>
                已索引 {ragDocs.length} 个文档
                {ragDocs.length > 0 && (
                  <button
                    className="acui-btn danger"
                    style={{ marginLeft: 8, padding: "3px 10px", fontSize: 11 }}
                    onClick={() => void doClearRag()}
                  >
                    清空
                  </button>
                )}
              </div>

              <div style={{ maxHeight: 140, overflowY: "auto", background: "var(--box-solid)", borderRadius: 8, padding: 8 }}>
                {ragDocs.length === 0 ? (
                  <div style={{ fontSize: 11, color: "var(--text-faint)" }}>
                    暂无已索引文档。输入目录路径后点击「索引」。
                  </div>
                ) : (
                  ragDocs.map((d) => (
                    <div
                      key={d.path}
                      style={{
                        fontSize: 11,
                        color: "var(--text-dim)",
                        padding: "3px 0",
                        display: "flex",
                        justifyContent: "space-between",
                        gap: 8,
                      }}
                    >
                      <span
                        style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}
                        title={d.path}
                      >
                        {d.path}
                      </span>
                      <span style={{ color: "var(--text-faint)", flexShrink: 0 }}>{d.chunks} 分块</span>
                    </div>
                  ))
                )}
              </div>

              {ragHits.length > 0 && (
                <div style={{ marginTop: 8, fontSize: 11, color: "var(--text-dim)" }}>
                  {ragHits.map((h, i) => (
                    <div key={i} style={{ padding: "4px 0", borderTop: "1px solid var(--border-soft)" }}>
                      <span style={{ color: "var(--cyan)" }}>{h.path}</span>
                      {h.score > 0 && (
                        <span style={{ color: "var(--text-faint)", marginLeft: 6 }}>{h.score}%</span>
                      )}
                      <div
                        style={{
                          color: "var(--text-faint)",
                          overflow: "hidden",
                          textOverflow: "ellipsis",
                          whiteSpace: "nowrap",
                        }}
                      >
                        {h.content}
                      </div>
                    </div>
                  ))}
                </div>
              )}
            </section>

            {/* ============ 数据库连接 ============ */}
            <section style={{ borderTop: "1px solid var(--border-soft)", paddingTop: 12, marginTop: 8 }}>
              <h4 style={{ margin: "4px 0 8px", color: "#34d399" }}>数据库连接</h4>
              <div style={{ display: "flex", gap: 8, marginBottom: 8 }}>
                <input
                  style={{ ...input, flex: "0 0 140px" }}
                  value={dbName}
                  onChange={(e) => setDbName(e.target.value)}
                  placeholder="名称，如 业务库"
                />
                <input
                  style={{ ...input, flex: 1 }}
                  value={dbConnStr}
                  onChange={(e) => setDbConnStr(e.target.value)}
                  placeholder="连接串，如 mysql://user:pass@host:3306/db 或 D:\data.db"
                />
                <button className="acui-btn primary" onClick={() => void addDbConn()}>
                  保存
                </button>
              </div>
              {dbMsg && <div style={{ fontSize: 11, color: "var(--text-faint)", marginBottom: 8 }}>{dbMsg}</div>}
              <div style={{ fontSize: 12, color: "var(--text-dim)", marginBottom: 6 }}>
                已配置 {dbConns.length} 个连接（db_query/db_execute 的 connection 可直接填名称）
              </div>
              <div style={{ maxHeight: 140, overflowY: "auto", background: "var(--box-solid)", borderRadius: 8, padding: 8 }}>
                {dbConns.length === 0 ? (
                  <div style={{ fontSize: 11, color: "var(--text-faint)" }}>暂无连接配置</div>
                ) : (
                  dbConns.map((c) => (
                    <div
                      key={c.name}
                      style={{
                        fontSize: 11,
                        color: "var(--text-dim)",
                        padding: "4px 0",
                        display: "flex",
                        justifyContent: "space-between",
                        gap: 8,
                        alignItems: "center",
                      }}
                    >
                      <span style={{ fontWeight: 600, color: "var(--text)" }}>{c.name}</span>
                      <span
                        style={{
                          flex: 1,
                          overflow: "hidden",
                          textOverflow: "ellipsis",
                          whiteSpace: "nowrap",
                          color: "var(--text-faint)",
                        }}
                        title={c.connection}
                      >
                        {c.connection}
                      </span>
                      <button
                        className="acui-btn danger"
                        style={{ padding: "2px 8px", fontSize: 11 }}
                        onClick={() => void removeDbConn(c.name)}
                      >
                        删除
                      </button>
                    </div>
                  ))
                )}
              </div>
            </section>
            </div>

            {/* 通知与音效页 */}
            <div className="settings-page" style={{ display: page === "notify" ? undefined : "none" }}>
            {/* ============ 通知升级 ============ */}
            {notify && (
              <section style={{ borderTop: "1px solid var(--border-soft)", paddingTop: 12, marginTop: 8 }}>
                <h4 style={{ margin: "4px 0 8px", color: "#f97316" }}>通知升级</h4>
                <label style={{ display: "flex", gap: 8, alignItems: "center", marginBottom: 12, fontSize: 13 }}>
                  <input
                    type="checkbox"
                    checked={notify.enabled}
                    onChange={(e) => updNotify({ enabled: e.target.checked })}
                  />
                  启用通知升级（审批超时后逐级通知）
                </label>

                {notify.enabled && (
                  <>
                    {[
                      { i: 0, label: "L0 应用弹窗", desc: "前端弹窗 + 审批卡片" },
                      { i: 1, label: "L1 系统通知", desc: "Windows Toast / macOS 通知中心" },
                      { i: 2, label: "L2 语音播报", desc: "TTS 语音朗读 + 系统提示音" },
                      { i: 3, label: "L3 邮件通知", desc: "SMTP 邮件通知" },
                      { i: 4, label: "L4 自定义通知", desc: "Webhook 推送（钉钉/飞书/短信）" },
                    ].map(({ i, label, desc }) => (
                      <div
                        key={i}
                        style={{
                          display: "flex",
                          alignItems: "center",
                          gap: 10,
                          marginBottom: 8,
                          padding: "6px 10px",
                          background: "var(--box-solid)",
                          borderRadius: 8,
                          fontSize: 13,
                        }}
                      >
                        <input
                          type="checkbox"
                          checked={notify.levels_enabled[i]}
                          onChange={(e) => {
                            const arr = [...notify.levels_enabled];
                            arr[i] = e.target.checked;
                            updNotify({ levels_enabled: arr });
                          }}
                        />
                        <span style={{ color: "var(--text-dim)", minWidth: 100 }}>{label}</span>
                        <span style={{ color: "var(--text-faint)", fontSize: 11, flex: 1 }}>{desc}</span>
                        <span style={{ color: "var(--text-faint)", fontSize: 11 }}>超时</span>
                        <input
                          style={{
                            ...input,
                            width: 56,
                            padding: "4px 6px",
                            textAlign: "center",
                            marginBottom: 0,
                          }}
                          type="number"
                          min={10}
                          value={notify.timeouts_sec[i]}
                          onChange={(e) => {
                            const arr = [...notify.timeouts_sec];
                            arr[i] = Math.max(10, Number(e.target.value));
                            updNotify({ timeouts_sec: arr });
                          }}
                        />
                        <span style={{ color: "var(--text-faint)", fontSize: 11 }}>秒</span>
                      </div>
                    ))}

                    {/* 语音播报配置（L2 启用时显示） */}
                    {notify.levels_enabled[2] && (
                      <div style={{ marginTop: 12, padding: "10px 12px", background: "var(--box-solid)", borderRadius: 8 }}>
                        <h5 style={{ margin: "0 0 8px", color: "#f97316", fontSize: 13 }}>语音播报配置</h5>
                        <div style={field}>
                          <span style={label}>
                            自定义播报文本（为空则使用 Agent 动态生成）
                          </span>
                          <input
                            style={input}
                            value={notify.voice_text ?? ""}
                            onChange={(e) =>
                              updNotify({ voice_text: e.target.value || null })
                            }
                            placeholder="白泽提醒：你的任务需要确认，快回来！"
                          />
                        </div>
                        <div style={field}>
                          <span style={label}>
                            音频文件路径（.mp3/.wav，用于播放歌曲/音效）
                          </span>
                          <input
                            style={input}
                            value={notify.audio_file ?? ""}
                            onChange={(e) =>
                              updNotify({ audio_file: e.target.value || null })
                            }
                            placeholder="C:\Users\OMEN\Music\alarm.mp3"
                          />
                        </div>
                        <div style={{ fontSize: 11, color: "var(--text-faint)", marginTop: -6 }}>
                          语音播报将循环播放，直到用户点击确认。可同时播放音频文件 + TTS 语音。
                        </div>
                      </div>
                    )}

                    {/* 邮箱配置（L3 启用时显示） */}
                    {notify.levels_enabled[3] && (
                      <div style={{ marginTop: 12, padding: "10px 12px", background: "var(--box-solid)", borderRadius: 8 }}>
                        <h5 style={{ margin: "0 0 8px", color: "#f97316", fontSize: 13 }}>邮箱配置</h5>
                        <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 8 }}>
                          {(["smtp_host", "smtp_port", "username", "password", "from", "to"] as (keyof EmailConfig)[]).map((key) => (
                            <div key={key} style={field}>
                              <span style={{ ...label, fontSize: 11 }}>
                                {key === "smtp_host" ? "SMTP 地址" :
                                 key === "smtp_port" ? "端口" :
                                 key === "username" ? "用户名" :
                                 key === "password" ? "密码" :
                                 key === "from" ? "发件人" : "收件人"}
                              </span>
                              <input
                                style={{ ...input, padding: "5px 8px", fontSize: 12 }}
                                type={key === "password" ? "password" : "text"}
                                value={notify.email?.[key] ?? ""}
                                onChange={(e) => {
                                  const cfg = notify.email ? { ...notify.email } : {
                                    smtp_host: "", smtp_port: 587, username: "", password: "", from: "", to: ""
                                  };
                                  (cfg as Record<string, unknown>)[key] = key === "smtp_port" ? Number(e.target.value) : e.target.value;
                                  updNotify({ email: cfg as EmailConfig });
                                }}
                                placeholder={
                                  key === "smtp_host" ? "smtp.qq.com" :
                                  key === "smtp_port" ? "587" :
                                  key === "username" ? "your@email.com" : ""
                                }
                              />
                            </div>
                          ))}
                        </div>
                      </div>
                    )}

                    {/* Webhook 配置（L4 启用时显示） */}
                    {notify.levels_enabled[4] && (
                      <div style={{ marginTop: 12, padding: "10px 12px", background: "var(--box-solid)", borderRadius: 8 }}>
                        <h5 style={{ margin: "0 0 8px", color: "#f97316", fontSize: 13 }}>Webhook 配置</h5>
                        <div style={field}>
                          <span style={label}>URL</span>
                          <input
                            style={input}
                            value={notify.webhook?.url ?? ""}
                            onChange={(e) => {
                              const cfg = notify.webhook ? { ...notify.webhook } : { url: "", headers: null };
                              cfg.url = e.target.value;
                              updNotify({ webhook: cfg as WebhookConfig });
                            }}
                            placeholder="https://hooks.slack.com/... 或 https://oapi.dingtalk.com/..."
                          />
                        </div>
                        <div style={field}>
                          <span style={label}>自定义请求头（JSON 对象，可选）</span>
                          <input
                            style={input}
                            value={notify.webhook?.headers ?? ""}
                            onChange={(e) => {
                              const cfg = notify.webhook ? { ...notify.webhook } : { url: "", headers: null };
                              cfg.headers = e.target.value || null;
                              updNotify({ webhook: cfg as WebhookConfig });
                            }}
                            placeholder='{"Authorization": "Bearer xxx"}'
                          />
                        </div>
                        <div style={{ fontSize: 11, color: "var(--text-faint)", marginTop: -6 }}>
                          POST JSON，body 包含 title、body、what、detail、timestamp
                        </div>
                      </div>
                    )}
                  </>
                )}
              </section>
            )}

            {/* ============ 音效 ============ */}
            <section style={{ borderTop: "1px solid var(--border-soft)", paddingTop: 12, marginTop: 8 }}>
              <h4 style={{ margin: "4px 0 8px", color: "#a78bfa" }}>音效</h4>
              <label style={{ display: "flex", gap: 8, alignItems: "center", marginBottom: 10, fontSize: 13 }}>
                <input
                  type="checkbox"
                  checked={sfxOn}
                  onChange={(e) => toggleSfx(e.target.checked)}
                />
                启用提示音（任务完成 / 弹窗提醒 / 审批请求 / 出错等）
              </label>
              {sfxOn && (
                <>
                  <div style={{ display: "flex", gap: 10, alignItems: "center", marginBottom: 10, fontSize: 13 }}>
                    <span style={{ whiteSpace: "nowrap" }}>音量</span>
                    <input
                      type="range"
                      min={0}
                      max={100}
                      value={Math.round(sfxVol * 100)}
                      onChange={(e) => changeVol(Number(e.target.value) / 100)}
                      onMouseUp={() => playSfx("notify")}
                      onTouchEnd={() => playSfx("notify")}
                      style={{ flex: 1, maxWidth: 220 }}
                    />
                    <span style={{ color: "var(--text-dim, #94a3b8)", minWidth: 34 }}>{Math.round(sfxVol * 100)}%</span>
                  </div>

                  <div style={{ marginBottom: 10 }}>
                    <div style={{ fontSize: 12, color: "var(--text-dim, #94a3b8)", marginBottom: 6 }}>弹窗提醒风格</div>
                    <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
                      {(
                        [
                          { k: "windchime", label: "风铃" },
                          { k: "arp", label: "清脆琶音" },
                          { k: "soft", label: "柔和单音" },
                        ] as { k: NotifyStyle; label: string }[]
                      ).map(({ k, label }) => (
                        <button
                          key={k}
                          className="acui-btn"
                          style={{
                            padding: "6px 14px",
                            borderColor: notifyStyle === k ? "var(--accent)" : undefined,
                            color: notifyStyle === k ? "#fff" : undefined,
                            background: notifyStyle === k ? "var(--accent-soft)" : undefined,
                          }}
                          onClick={() => pickNotifyStyle(k)}
                        >
                          {label}
                        </button>
                      ))}
                    </div>
                  </div>

                  <div>
                    <div style={{ fontSize: 12, color: "var(--text-dim, #94a3b8)", marginBottom: 6 }}>事件试听</div>
                    <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
                      {(
                        [
                          { e: "task-done", label: "任务完成" },
                          { e: "notify", label: "弹窗提醒" },
                          { e: "permission", label: "审批请求" },
                          { e: "card-pop", label: "卡片弹出" },
                          { e: "message-sent", label: "消息发出" },
                          { e: "error", label: "出错" },
                          { e: "escalation", label: "升级警报" },
                          { e: "startup", label: "启动" },
                        ] as { e: SfxEvent; label: string }[]
                      ).map(({ e, label }) => (
                        <button
                          key={e}
                          className="acui-btn"
                          style={{ padding: "5px 12px", fontSize: 12 }}
                          onClick={() => playSfx(e)}
                        >
                          ▸ {label}
                        </button>
                      ))}
                    </div>
                  </div>
                </>
              )}
            </section>
            </div>

            {/* 消息与机器人页 */}
            <div className="settings-page" style={{ display: page === "bots" ? undefined : "none" }}>
            {/* ============ IM 消息总线（跨通道审批 / 结果回传） ============ */}
            <section style={{ borderTop: "1px solid var(--border-soft)", paddingTop: 12, marginTop: 8 }}>
              <h4 style={{ margin: "4px 0 8px", color: "var(--text)" }}>IM 消息总线</h4>
              <div style={{ display: "flex", gap: 8, flexWrap: "wrap", marginBottom: 6 }}>
                {imChannels.map((c) => (
                  <span
                    key={c.id}
                    style={{
                      display: "inline-flex",
                      alignItems: "center",
                      gap: 6,
                      padding: "4px 10px",
                      fontSize: 12,
                      borderRadius: 999,
                      border: "1px solid var(--border)",
                      color: "var(--text-dim)",
                    }}
                  >
                    <span
                      style={{
                        width: 8,
                        height: 8,
                        borderRadius: "50%",
                        background: c.connected ? "#22c55e" : "#64748b",
                        boxShadow: c.connected ? "0 0 6px #22c55e" : "none",
                      }}
                    />
                    {c.label}
                    {c.connected ? (c.status === "connected" ? "· 已连接" : "· 已配置") : "· 未配置"}
                  </span>
                ))}
                {imChannels.length === 0 && (
                  <span style={{ fontSize: 12, color: "var(--text-faint)" }}>加载通道中…</span>
                )}
              </div>
              <div style={{ fontSize: 11, color: "var(--text-faint)", marginBottom: 4 }}>
                高危操作审批与任务结果会经任意已连接通道回传；回复「允许 / 拒绝」完成二次确认。
              </div>
            </section>

            {/* ============ 微信机器人（手机扫码指挥白泽） ============ */}
            <section style={{ borderTop: "1px solid var(--border-soft)", paddingTop: 12, marginTop: 8 }}>
              <h4 style={{ margin: "4px 0 8px", color: "#22c55e" }}>微信机器人（手机扫码指挥白泽）</h4>

              {/* 连接状态指示灯 */}
              <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 10, fontSize: 13 }}>
                <span
                  style={{
                    width: 8,
                    height: 8,
                    borderRadius: "50%",
                    background:
                      wxStatus?.connected
                        ? "#22c55e"
                        : wxStatus?.status === "qr_pending"
                          ? "#f59e0b"
                          : "#64748b",
                    boxShadow: wxStatus?.connected ? "0 0 8px #22c55e" : "none",
                  }}
                />
                <span style={{ color: "var(--text-dim)" }}>
                  {wxStatus?.status === "connected"
                    ? `已连接${wxStatus?.account_id ? ` · ${wxStatus.account_id}` : ""}`
                    : wxStatus?.status === "qr_pending"
                      ? "等待扫码…"
                      : wxStatus?.status === "disconnected"
                        ? "已断开（凭证保留，可重连）"
                        : "未登录"}
                </span>
              </div>

              {/* 扫码二维码（登录中展示） */}
              {wxQr && (
                <div
                  style={{
                    display: "flex",
                    flexDirection: "column",
                    alignItems: "center",
                    gap: 8,
                    marginBottom: 12,
                    padding: 12,
                    background: "var(--box-solid)",
                    borderRadius: 8,
                  }}
                >
                  <img
                    src={
                      wxQr.startsWith("data:")
                        ? wxQr
                        : wxQr.startsWith("http")
                          ? wxQr
                          : `data:image/png;base64,${wxQr}`
                    }
                    alt="微信登录二维码"
                    style={{ width: 200, height: 200, borderRadius: 8, background: "#fff", padding: 6 }}
                  />
                  <div style={{ fontSize: 11, color: "var(--text-faint)" }}>请用微信扫码登录</div>
                  <button
                    className="acui-btn"
                    style={{ padding: "4px 12px", fontSize: 12 }}
                    onClick={() => void doWxCancel()}
                  >
                    取消
                  </button>
                </div>
              )}

              {/* 操作按钮（按当前状态显隐） */}
              <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
                {!wxStatus?.connected && wxStatus?.status !== "qr_pending" && (
                  <button
                    className="acui-btn primary"
                    disabled={wxBusy}
                    onClick={() => void doWxLogin()}
                  >
                    扫码登录
                  </button>
                )}
                {wxStatus?.status === "disconnected" && (
                  <button className="acui-btn" disabled={wxBusy} onClick={() => void doWxStart()}>
                    重连
                  </button>
                )}
                {wxStatus?.connected && (
                  <button className="acui-btn" disabled={wxBusy} onClick={() => void doWxStop()}>
                    断开
                  </button>
                )}
                {wxStatus?.connected && (
                  <button className="acui-btn danger" disabled={wxBusy} onClick={() => void doWxLogout()}>
                    登出
                  </button>
                )}
              </div>

              {wxMsg && <div style={{ fontSize: 11, color: "#fca5a5", marginTop: 8 }}>{wxMsg}</div>}

              <div style={{ fontSize: 11, color: "var(--text-faint)", marginTop: 8 }}>
                登录后，在微信中向白泽发消息即可远程指挥任务；高危操作会经微信二次确认（回复「允许 / 拒绝」）。
              </div>
            </section>

            {/* ============ 飞书机器人（Lark 自建应用，长连接指挥白泽） ============ */}
            <section style={{ borderTop: "1px solid var(--border-soft)", paddingTop: 12, marginTop: 12 }}>
              <h4 style={{ margin: "4px 0 8px", color: "#38bdf8" }}>飞书机器人（Lark 自建应用）</h4>

              {/* 连接状态指示灯 */}
              <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 10, fontSize: 13 }}>
                <span
                  style={{
                    width: 8,
                    height: 8,
                    borderRadius: "50%",
                    background:
                      fsStatus?.status === "connected"
                        ? "#38bdf8"
                        : fsStatus?.status === "connecting" || fsStatus?.status === "reconnecting"
                          ? "#f59e0b"
                          : "#64748b",
                    boxShadow: fsStatus?.status === "connected" ? "0 0 8px #38bdf8" : "none",
                  }}
                />
                <span style={{ color: "var(--text-dim)" }}>
                  {fsStatus?.status === "connected"
                    ? "已连接"
                    : fsStatus?.status === "connecting"
                      ? "连接中…"
                      : fsStatus?.status === "reconnecting"
                        ? "重连中…"
                        : fsStatus?.status === "disconnected"
                          ? "已断开（凭证保留，可重连）"
                          : fsStatus?.connected
                            ? "已配置，待连接"
                            : "未配置"}
                </span>
              </div>

              {/* 凭证录入（未配置时展示） */}
              {!fsStatus?.connected && (
                <div style={{ marginBottom: 12 }}>
                  <div style={field}>
                    <span style={label}>App ID</span>
                    <input
                      style={input}
                      value={fsAppId}
                      onChange={(e) => setFsAppId(e.target.value)}
                      placeholder="cli_xxxxxxxxxxxxxxxx"
                    />
                  </div>
                  <div style={field}>
                    <span style={label}>App Secret</span>
                    <input
                      style={input}
                      type="password"
                      value={fsAppSecret}
                      onChange={(e) => setFsAppSecret(e.target.value)}
                      placeholder="飞书开放平台应用的 App Secret"
                    />
                  </div>
                </div>
              )}

              {/* 操作按钮（按当前状态显隐） */}
              <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
                {!fsStatus?.connected && (
                  <button className="acui-btn primary" disabled={fsBusy} onClick={() => void doFsSave()}>
                    保存并连接
                  </button>
                )}
                {fsStatus?.connected && fsStatus?.status !== "connected" && (
                  <button className="acui-btn" disabled={fsBusy} onClick={() => void doFsStart()}>
                    连接
                  </button>
                )}
                {fsStatus?.status === "connected" && (
                  <button className="acui-btn danger" disabled={fsBusy} onClick={() => void doFsStop()}>
                    断开
                  </button>
                )}
              </div>

              {fsMsg && <div style={{ fontSize: 11, color: "#fca5a5", marginTop: 8 }}>{fsMsg}</div>}

              <div style={{ fontSize: 11, color: "var(--text-faint)", marginTop: 8 }}>
                在飞书开放平台创建自建应用并订阅 im.message.receive_v1 事件，填入 App ID / App Secret 即可连接；高危操作会经飞书二次确认（回复「允许 / 拒绝」）。
              </div>
            </section>
            </div>

            {/* 语音朗读页 */}
            <div className="settings-page" style={{ display: page === "voice" ? undefined : "none" }}>
            <section style={{ borderTop: "1px solid var(--border-soft)", paddingTop: 12, marginTop: 12 }}>
              <h4 style={{ margin: "4px 0 8px", color: "#38bdf8" }}>语音模型（TTS）</h4>

              <div style={field}>
                <span style={label}>语音后端</span>
                <select
                  style={input}
                  value={ttsCfg.provider}
                  onChange={(e) => setTtsCfg({ ...ttsCfg, provider: e.target.value })}
                >
                  <option value="local">本地语音（浏览器内置，零配置）</option>
                  <option value="kokoro">本地 Kokoro（免费离线，多音色更像真人）</option>
                  <option value="doubao">豆包语音合成（火山引擎，小何等大模型音色）</option>
                  <option value="cloud">云端语音模型（OpenAI 兼容，音色更像真人）</option>
                </select>
              </div>

              {ttsCfg.provider === "kokoro" ? (
                <>
                  <div style={field}>
                    <span style={label}>音色</span>
                    <select
                      style={input}
                      value={ttsCfg.voice ?? ""}
                      onChange={(e) => setTtsCfg({ ...ttsCfg, voice: e.target.value })}
                    >
                      {!kokoroVoices.some((v) => v.id === ttsCfg.voice) && ttsCfg.voice && (
                        <option value={ttsCfg.voice}>{ttsCfg.voice}（当前配置）</option>
                      )}
                      {kokoroVoices.map((v) => (
                        <option key={v.id} value={v.id}>
                          {v.name}
                        </option>
                      ))}
                    </select>
                  </div>
                  <div style={{ fontSize: 11, color: "var(--text-faint)", marginBottom: 10 }}>
                    本地 Kokoro-82M 中文微调模型（v1.1-zh，Apache 2.0 开源），装在 F:\kokoro-tts，全程本机合成零费用。
                    音色仅保留中文：女声 zf_001~zf_099、男声 zm_009~zm_100 共 100 个。首次使用会自动拉起本地服务（约 10-30 秒加载模型）。
                    推荐：zf_001（女声）、zm_009（男声）。
                  </div>
                  <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap", marginBottom: 8 }}>
                    <button
                      className="acui-btn primary"
                      onClick={async () => {
                        try {
                          await setTtsConfig(ttsCfg);
                          setTtsStatus("Kokoro 语音配置已保存");
                          window.dispatchEvent(new CustomEvent("baize:voice-changed"));
                        } catch (e) {
                          setTtsStatus(`保存失败: ${e}`);
                        }
                      }}
                    >
                      保存语音模型
                    </button>
                    <button
                      className="acui-btn"
                      onClick={() => {
                        stopSpeaking();
                        setTtsStatus("正在合成 Kokoro 语音（首次可能需要启动本地服务）…");
                        void (async () => {
                          await setTtsConfig(ttsCfg); // 先保存，后端读取的是持久化配置
                          window.dispatchEvent(new CustomEvent("baize:voice-changed"));
                          await speakWithCloud("你好，我是白泽，本地 Kokoro 语音已就绪。");
                          setTtsStatus("试听完成");
                        })().catch((e) => {
                          const msg = String(e);
                          const hint = /未安装|启动|连接|超时|无法连接/.test(msg)
                            ? "（请确认 F:\\kokoro-tts 已安装，或先运行 start_server.bat 预热）"
                            : "";
                          setTtsStatus(`Kokoro 试听失败: ${msg}${hint}`);
                        });
                      }}
                    >
                      Kokoro 试听
                    </button>
                  </div>
                </>
              ) : ttsCfg.provider === "doubao" ? (
                <>
                  <div style={field}>
                    <span style={label}>App ID</span>
                    <input
                      style={input}
                      value={ttsCfg.db_app_id ?? ""}
                      onChange={(e) => setTtsCfg({ ...ttsCfg, db_app_id: e.target.value })}
                      placeholder="火山引擎豆包语音控制台的应用 APP ID（数字）"
                    />
                  </div>
                  <div style={field}>
                    <span style={label}>Access Token</span>
                    <input
                      style={input}
                      type="password"
                      value={ttsCfg.db_token ?? ""}
                      onChange={(e) => setTtsCfg({ ...ttsCfg, db_token: e.target.value })}
                      placeholder="应用详情页的 Access Token"
                    />
                  </div>
                  <div style={field}>
                    <span style={label}>音色 ID (speaker)</span>
                    <input
                      style={input}
                      value={ttsCfg.db_speaker ?? ""}
                      onChange={(e) => setTtsCfg({ ...ttsCfg, db_speaker: e.target.value })}
                      placeholder="如音色库里的 zh_female_xiaohe_uranus_bigtts（小何）"
                    />
                  </div>
                  <div style={field}>
                    <span style={label}>语速（-50 慢 ~ 100 快，0 正常）</span>
                    <input
                      style={input}
                      type="number"
                      min={-50}
                      max={100}
                      value={ttsCfg.db_speech_rate ?? 0}
                      onChange={(e) =>
                        setTtsCfg({ ...ttsCfg, db_speech_rate: Number(e.target.value) || 0 })
                      }
                    />
                  </div>
                  <div style={{ fontSize: 11, color: "var(--text-faint)", marginBottom: 10 }}>
                    豆包语音合成 2.0（seed-tts-2.0）。App ID 与 Access Token 在
                    火山引擎控制台「豆包语音 → 语音合成大模型 → 应用管理」的应用详情页；音色 ID 从「音色库」复制。
                    播放时水球按真实频谱律动。
                  </div>
                  <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap", marginBottom: 8 }}>
                    <button
                      className="acui-btn primary"
                      onClick={async () => {
                        try {
                          await setTtsConfig(ttsCfg);
                          setTtsStatus("豆包语音配置已保存");
                          window.dispatchEvent(new CustomEvent("baize:voice-changed"));
                        } catch (e) {
                          setTtsStatus(`保存失败: ${e}`);
                        }
                      }}
                    >
                      保存语音模型
                    </button>
                    <button
                      className="acui-btn"
                      onClick={() => {
                        stopSpeaking();
                        setTtsStatus("正在合成豆包语音…");
                        void (async () => {
                          await setTtsConfig(ttsCfg); // 先保存，后端读取的是持久化配置
                          window.dispatchEvent(new CustomEvent("baize:voice-changed"));
                          await speakWithCloud("你好，我是白泽，豆包语音已就绪。");
                          setTtsStatus("试听完成");
                        })().catch((e) =>
                          setTtsStatus(`豆包试听失败: ${e}（请检查配置）`),
                        );
                      }}
                    >
                      豆包试听
                    </button>
                  </div>
                </>
              ) : ttsCfg.provider === "cloud" ? (
                <>
                  <div style={field}>
                    <span style={label}>接口地址 base_url</span>
                    <input
                      style={input}
                      value={ttsCfg.base_url}
                      onChange={(e) => setTtsCfg({ ...ttsCfg, base_url: e.target.value })}
                      placeholder="https://api.siliconflow.cn/v1"
                    />
                  </div>
                  <div style={field}>
                    <span style={label}>API Key</span>
                    <input
                      style={input}
                      type="password"
                      value={ttsCfg.api_key}
                      onChange={(e) => setTtsCfg({ ...ttsCfg, api_key: e.target.value })}
                      placeholder="sk-..."
                    />
                  </div>
                  <div style={field}>
                    <span style={label}>语音模型 model</span>
                    <input
                      style={input}
                      value={ttsCfg.model}
                      onChange={(e) => setTtsCfg({ ...ttsCfg, model: e.target.value })}
                      placeholder="FunAudioLLM/CosyVoice2-0.5B"
                    />
                  </div>
                  <div style={field}>
                    <span style={label}>音色 voice</span>
                    <input
                      style={input}
                      value={ttsCfg.voice}
                      onChange={(e) => setTtsCfg({ ...ttsCfg, voice: e.target.value })}
                      placeholder="FunAudioLLM/CosyVoice2-0.5B:alex"
                    />
                  </div>
                  <div style={{ fontSize: 11, color: "var(--text-faint)", marginBottom: 10 }}>
                    兼容 OpenAI /audio/speech 接口的服务均可接入：硅基流动（CosyVoice，含豆包同源音色）、
                    fun-audio-handling、自建 fish-speakers 等。播放时水球按真实频谱律动。
                  </div>
                  <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap", marginBottom: 8 }}>
                    <button
                      className="acui-btn primary"
                      onClick={async () => {
                        try {
                          await setTtsConfig(ttsCfg);
                          setTtsStatus("语音模型配置已保存");
                          window.dispatchEvent(new CustomEvent("baize:voice-changed"));
                        } catch (e) {
                          setTtsStatus(`保存失败: ${e}`);
                        }
                      }}
                    >
                      保存语音模型
                    </button>
                    <button
                      className="acui-btn"
                      onClick={() => {
                        stopSpeaking();
                        void speakWithCloud("你好，我是白泽，云端语音已就绪。").catch((e) =>
                          setTtsStatus(`云端试听失败: ${e}（请检查配置）`),
                        );
                      }}
                    >
                      云端试听
                    </button>
                  </div>
                </>
              ) : (
                <div style={field}>
                  <span style={label}>朗读音色（系统中文语音）</span>
                  <select
                    style={input}
                    value={voiceName}
                    onChange={(e) => saveVoice(e.target.value)}
                  >
                    {voices.length === 0 && <option value="">（未检测到可用语音）</option>}
                    {voices.map((v) => (
                      <option key={v.name} value={v.name}>
                        {v.name}（{v.lang}）
                      </option>
                    ))}
                  </select>
                </div>
              )}

              <div style={{ display: "flex", gap: 10, alignItems: "center", flexWrap: "wrap" }}>
                {ttsCfg.provider === "local" && (
                  <button
                    className="acui-btn"
                    onClick={() => {
                      const v = voices.find((x) => x.name === voiceName);
                      reactiveSpeak("你好，我是白泽，很高兴为你朗读。", {
                        lang: "zh-CN",
                        rate: 1.0,
                        voice: v,
                      });
                    }}
                  >
                    试听
                  </button>
                )}
                <span style={{ fontSize: 11, color: "var(--text-faint)" }}>
                  打开聊天输入区的语音开关后，白泽每条回复都会朗读；本地语音推荐 Microsoft 开头音色。
                </span>
              </div>
              {ttsStatus && (
                <div style={{ fontSize: 11, color: ttsStatus.includes("失败") ? "#fca5a5" : "var(--success)", marginTop: 8 }}>
                  {ttsStatus}
                </div>
              )}
            </section>
            </div>
            </div>
            </>
        )}
        </div>
      </div>
    </div>
  );
}
