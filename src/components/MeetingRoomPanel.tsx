import { useCallback, useEffect, useRef, useState } from "react";
import {
  getModelConfig,
  interruptMeeting,
  onMeetingError,
  onMeetingSpeaker,
  onMeetingSummaryToken,
  onMeetingToken,
  onMeetingTool,
  onMeetingUtterance,
  onTeamworkEntry,
  onTeamworkStage,
  onTeamworkToken,
  onTeamworkTool,
  runMeeting,
  runTeamwork,
  stopMeeting,
  summarizeMeeting,
} from "../api";
import type {
  MeetingParticipant,
  MeetingToolUse,
  MeetingUtterance,
  ModelConfig,
  TeamEntry,
} from "../types";

const genId = () =>
  "mbr_" + Date.now().toString(36) + Math.random().toString(36).slice(2, 6);

/** 发言成员配色（按索引循环） */
const SPEAKER_COLORS = [
  "#22d3ee",
  "#a78bfa",
  "#34d399",
  "#f59e0b",
  "#fb7185",
  "#38bdf8",
];

/** 默认角色示例 */
const ROLE_EXAMPLES = ["主持人", "技术专家", "产品视角", "批评者", "记录员"];

/**
 * 多 Agent 会议室（独立组件窗口）：
 * 从已保存的模型列表中选择多位成员（各自绑定不同的模型），
 * 设定一个主题后，各成员按序轮流发言、相互接续，实时流式展示整场讨论。
 */
export default function MeetingRoomPanel({ onClose }: { onClose: () => void }) {
  const [modelConfig, setModelConfig] = useState<ModelConfig | null>(null);
  const [participants, setParticipants] = useState<MeetingParticipant[]>([]);
  const [topic, setTopic] = useState("");
  const [rounds, setRounds] = useState(2);
  const [running, setRunning] = useState(false);
  const [messages, setMessages] = useState<MeetingUtterance[]>([]);
  const [active, setActive] = useState<{ id: string; name: string; round: number } | null>(null);
  const [streamText, setStreamText] = useState("");
  const [error, setError] = useState("");
  const [speakErrors, setSpeakErrors] = useState<Record<string, string>>({});
  const [summary, setSummary] = useState("");
  const [summarizing, setSummarizing] = useState(false);
  const [toolActivity, setToolActivity] = useState("");
  const [mode, setMode] = useState<"discuss" | "teamwork">("discuss");
  const [teamEntries, setTeamEntries] = useState<TeamEntry[]>([]);
  const [teamStage, setTeamStage] = useState("");
  const [teamStreamText, setTeamStreamText] = useState("");
  const [teamToolActivity, setTeamToolActivity] = useState("");

  const mounted = useRef(true);
  const activeRef = useRef<{ id: string } | null>(null);
  const listRef = useRef<HTMLDivElement | null>(null);

  // 加载模型配置；首次打开时按可用模型自动排布两位成员
  const load = useCallback(async () => {
    try {
      const cfg = await getModelConfig();
      if (!mounted.current) return;
      setModelConfig(cfg);
      const enabled = cfg.profiles.filter((p) => p.enabled);
      if (enabled.length > 0) {
        setParticipants((prev) => {
          if (prev.length > 0) return prev;
          const seed: MeetingParticipant[] = enabled.slice(0, 2).map((p, i) => ({
            id: genId(),
            name: i === 0 ? "白泽" : p.name,
            role: i === 0 ? "主持人" : ROLE_EXAMPLES[(i + 1) % ROLE_EXAMPLES.length],
            profile_id: p.id,
          }));
          return seed;
        });
      }
    } catch (e) {
      if (mounted.current) setError(String(e));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  // 订阅会议实时事件（speaker / token / error）
  useEffect(() => {
    let disposed = false;
    const un: Array<() => void> = [];
    onMeetingSpeaker((e) => {
      if (disposed) return;
      activeRef.current = { id: e.speaker_id };
      setActive({ id: e.speaker_id, name: e.speaker_name, round: e.round });
      setStreamText("");
      setToolActivity("");
    }).then((f) => !disposed && un.push(f));
    onMeetingToken((e) => {
      if (disposed) return;
      if (activeRef.current?.id === e.speaker_id) {
        setStreamText((prev) => prev + e.token);
        setToolActivity("");
      }
    }).then((f) => !disposed && un.push(f));
    onMeetingTool((e) => {
      if (disposed) return;
      // 工具调用前的过渡思考不保留，避免拼进最终发言
      setStreamText("");
      setToolActivity(`${e.speaker_name} 正在调用 ${e.tool}`);
    }).then((f) => !disposed && un.push(f));
    onMeetingError((e) => {
      if (disposed) return;
      setSpeakErrors((prev) => ({ ...prev, [e.speaker_id]: e.error }));
    }).then((f) => !disposed && un.push(f));
    onMeetingUtterance((u) => {
      if (disposed) return;
      // 每条发言完成后立即追加记录（含被打断的部分内容），实现「说完即存档」
      setMessages((prev) => {
        const dup = prev.some(
          (m) =>
            m.speaker_id === u.speaker_id &&
            m.round === u.round &&
            m.content === u.content
        );
        return dup ? prev : [...prev, u];
      });
    }).then((f) => !disposed && un.push(f));
    onMeetingSummaryToken((e) => {
      if (disposed) return;
      setSummary((prev) => prev + e.token);
    }).then((f) => !disposed && un.push(f));
    return () => {
      disposed = true;
      un.forEach((f) => f());
    };
  }, []);

  // 订阅协作执行实时事件（stage / token / tool / entry）
  useEffect(() => {
    let disposed = false;
    const un: Array<() => void> = [];
    onTeamworkStage((e) => {
      if (disposed) return;
      setTeamStage(e.label);
      setTeamStreamText("");
      setTeamToolActivity("");
    }).then((f) => !disposed && un.push(f));
    onTeamworkToken((e) => {
      if (disposed) return;
      setTeamStreamText((prev) => prev + e.token);
      setTeamToolActivity("");
    }).then((f) => !disposed && un.push(f));
    onTeamworkTool((e) => {
      if (disposed) return;
      setTeamStreamText("");
      setTeamToolActivity(`${e.speaker_name} 正在调用 ${e.tool}`);
    }).then((f) => !disposed && un.push(f));
    onTeamworkEntry((e) => {
      if (disposed) return;
      setTeamEntries((prev) => {
        const dup = prev.some(
          (x) => x.kind === e.kind && x.title === e.title && x.content === e.content
        );
        return dup ? prev : [...prev, e];
      });
      setTeamStage("");
      setTeamStreamText("");
      setTeamToolActivity("");
    }).then((f) => !disposed && un.push(f));
    return () => {
      disposed = true;
      un.forEach((f) => f());
    };
  }, []);

  // 挂载清理
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  // 自动滚动到底部
  useEffect(() => {
    if (listRef.current) {
      listRef.current.scrollTop = listRef.current.scrollHeight;
    }
  }, [messages, streamText, teamEntries, teamStreamText]);

  const enabledProfiles = (modelConfig?.profiles ?? []).filter((p) => p.enabled);

  const addParticipant = () => {
    const used = new Set(participants.map((p) => p.profile_id));
    const next = enabledProfiles.find((p) => !used.has(p.id));
    if (!next) {
      setError("没有更多可用模型可添加，请先在设置中启用新模型");
      return;
    }
    setError("");
    const idx = participants.length;
    setParticipants((prev) => [
      ...prev,
      { id: genId(), name: next.name, role: ROLE_EXAMPLES[idx % ROLE_EXAMPLES.length], profile_id: next.id },
    ]);
  };

  const removeParticipant = (id: string) =>
    setParticipants((prev) => prev.filter((p) => p.id !== id));

  const updateParticipant = (id: string, patch: Partial<MeetingParticipant>) =>
    setParticipants((prev) => prev.map((p) => (p.id === id ? { ...p, ...patch } : p)));

  const start = async () => {
    if (running) return;
    if (!topic.trim()) {
      setError("请先填写会议主题");
      return;
    }
    if (participants.length === 0) {
      setError("请至少添加一位参会成员");
      return;
    }
    setError("");
    setSpeakErrors({});
    setMessages([]);
    setStreamText("");
    setActive(null);
    setSummary("");
    setSummarizing(false);
    setToolActivity("");
    setRunning(true);
    try {
      const result = await runMeeting(topic.trim(), participants, rounds);
      if (!mounted.current) return;
      setMessages(result);
    } catch (e) {
      if (mounted.current) setError(String(e));
    } finally {
      if (mounted.current) {
        setRunning(false);
        setActive(null);
        setStreamText("");
        setToolActivity("");
        activeRef.current = null;
      }
    }
  };

  // 协作执行：负责人拆解任务 → 各成员分工用共享工具执行 → 负责人汇总交付物
  const startTeamwork = async () => {
    if (running) return;
    if (!topic.trim()) {
      setError("请先填写任务目标");
      return;
    }
    if (participants.length === 0) {
      setError("请至少添加一位成员");
      return;
    }
    setError("");
    setTeamEntries([]);
    setTeamStage("");
    setTeamStreamText("");
    setTeamToolActivity("");
    setSummary("");
    setRunning(true);
    try {
      const result = await runTeamwork(topic.trim(), participants);
      if (!mounted.current) return;
      setTeamEntries(result);
    } catch (e) {
      if (mounted.current) setError(String(e));
    } finally {
      if (mounted.current) {
        setRunning(false);
        setTeamStage("");
        setTeamStreamText("");
        setTeamToolActivity("");
      }
    }
  };

  const reset = () => {
    setMessages([]);
    setStreamText("");
    setActive(null);
    setSpeakErrors({});
    setError("");
    setSummary("");
    setSummarizing(false);
    setToolActivity("");
    setTeamEntries([]);
    setTeamStage("");
    setTeamStreamText("");
    setTeamToolActivity("");
  };

  const doInterrupt = () => {
    void interruptMeeting();
  };

  const doStop = () => {
    void stopMeeting();
  };

  const makeSummary = async () => {
    if (messages.length === 0) {
      setError("暂无发言记录，无法总结");
      return;
    }
    setError("");
    setSummarizing(true);
    setSummary("");
    try {
      const full = await summarizeMeeting(topic.trim(), messages);
      if (!mounted.current) return;
      // 流式 token 已实时填充，此处用完整结果兜底校正，避免漏显
      if (full) setSummary(full);
    } catch (e) {
      if (mounted.current) setError(String(e));
    } finally {
      if (mounted.current) setSummarizing(false);
    }
  };

  const total = messages.length + (active ? 1 : 0);

  return (
    <div className="rpanel">
      <div
        style={{
          flex: 1,
          minHeight: 0,
          display: "flex",
          flexDirection: "column",
          overflow: "hidden",
        }}
      >
        {/* 头部 */}
        <div
          style={{
            display: "flex",
            alignItems: "center",
            gap: 10,
            padding: "14px 18px",
            borderBottom: "1px solid var(--border-soft)",
          }}
        >
          <h3 style={{ margin: 0, fontSize: 15, letterSpacing: 1 }}>多 Agent 会议室</h3>
          <span style={{ fontSize: 11, color: "var(--text-faint)" }}>
            {participants.length} 位成员 · {total} 条发言
          </span>
          <span style={{ flex: 1 }} />
          {running && (
            <span
              style={{
                fontSize: 11,
                color: "var(--cyan)",
                animation: "pulse 1.2s infinite",
              }}
            >
              {mode === "discuss" ? "讨论进行中…" : "协作进行中…"}
            </span>
          )}
          <button className="software-close" onClick={onClose} title="关闭">
            ×
          </button>
        </div>

        {/* 主体：左设置 + 右记录 */}
        <div style={{ flex: 1, display: "flex", minHeight: 0 }}>
          {/* 左栏：会议设置 */}
          <div
            style={{
              width: 300,
              minWidth: 260,
              borderRight: "1px solid var(--border-soft)",
              padding: "14px 16px",
              overflowY: "auto",
              display: "flex",
              flexDirection: "column",
              gap: 12,
            }}
          >
            <div className="software-row" style={{ gap: 8 }}>
              <button
                onClick={() => setMode("discuss")}
                disabled={running}
                className={mode === "discuss" ? "software-primary" : "software-refresh"}
                style={{ flex: 1 }}
              >
                圆桌讨论
              </button>
              <button
                onClick={() => setMode("teamwork")}
                disabled={running}
                className={mode === "teamwork" ? "software-primary" : "software-refresh"}
                style={{ flex: 1 }}
              >
                协作执行
              </button>
            </div>

            <div className="software-sec-title">参会成员</div>
            {participants.length === 0 && (
              <div style={{ fontSize: 11, color: "var(--text-faint)" }}>
                尚未添加成员。请在「设置 → 模型列表」中先启用至少一个模型。
              </div>
            )}

            {participants.map((p, i) => (
              <div
                key={p.id}
                style={{
                  border: "1px solid var(--border)",
                  borderRadius: 10,
                  padding: 8,
                  background: "rgba(0,0,0,0.2)",
                  display: "flex",
                  flexDirection: "column",
                  gap: 6,
                }}
              >
                <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
                  <span
                    style={{
                      width: 22,
                      height: 22,
                      borderRadius: "50%",
                      background: SPEAKER_COLORS[i % SPEAKER_COLORS.length],
                      color: "#0b1020",
                      fontWeight: 700,
                      fontSize: 12,
                      display: "flex",
                      alignItems: "center",
                      justifyContent: "center",
                      flex: "0 0 auto",
                    }}
                  >
                    {i + 1}
                  </span>
                  <input
                    className="software-search-input"
                    style={{ flex: 1, minWidth: 0 }}
                    value={p.name}
                    placeholder="成员名"
                    onChange={(e) => updateParticipant(p.id, { name: e.target.value })}
                  />
                  <button
                    onClick={() => removeParticipant(p.id)}
                    title="移除该成员"
                    style={{
                      background: "transparent",
                      border: "1px solid var(--border)",
                      color: "var(--text-dim)",
                      borderRadius: 8,
                      width: 24,
                      height: 24,
                      cursor: "pointer",
                    }}
                  >
                    ×
                  </button>
                </div>
                <input
                  className="software-search-input"
                  value={p.role}
                  placeholder="角色（如 技术专家）"
                  onChange={(e) => updateParticipant(p.id, { role: e.target.value })}
                />
                <select
                  className="mode-select"
                  style={{ width: "100%" }}
                  value={p.profile_id}
                  onChange={(e) => updateParticipant(p.id, { profile_id: e.target.value })}
                >
                  {enabledProfiles.map((prof) => (
                    <option key={prof.id} value={prof.id}>
                      {prof.name}
                      {prof.tier === "local" ? " · 本地" : " · 云端"}
                    </option>
                  ))}
                </select>
              </div>
            ))}

            <button
              className="software-refresh"
              onClick={addParticipant}
              disabled={running}
              style={{ width: "100%", borderStyle: "dashed" }}
            >
              ＋ 添加成员
            </button>

            <div className="software-sec-title">{mode === "discuss" ? "会议主题" : "任务目标"}</div>
            <textarea
              className="software-search-input"
              style={{ minHeight: 76, resize: "vertical" }}
              placeholder={
                mode === "discuss"
                  ? "输入一个话题，让多位 AI 就此展开讨论、技术分析或项目交流…"
                  : "描述一个可交付的目标，负责人会拆解后分配给成员协作完成…"
              }
              value={topic}
              onChange={(e) => setTopic(e.target.value)}
              disabled={running}
            />

            {mode === "discuss" && (
              <div className="software-row">
                <span className="software-kv">讨论轮数</span>
                <select
                  className="mode-select"
                  value={rounds}
                  onChange={(e) => setRounds(Number(e.target.value))}
                  disabled={running}
                >
                  {[1, 2, 3, 4, 5].map((n) => (
                    <option key={n} value={n}>
                      {n} 轮
                    </option>
                  ))}
                </select>
              </div>
            )}

            {error && <div className="software-error">{error}</div>}

            {running && (
              <div style={{ display: "flex", gap: 8 }}>
                <button
                  className="software-refresh"
                  onClick={doInterrupt}
                  title="保留当前成员已生成的部分内容，跳过并轮候下一位"
                  style={{ flex: 1, borderColor: "var(--amber, #f59e0b)", color: "var(--amber, #f59e0b)" }}
                >
                  打断
                </button>
                <button
                  className="software-refresh"
                  onClick={doStop}
                  title="立即停止整场会议，结束后续所有成员"
                  style={{ flex: 1, borderColor: "var(--danger)", color: "var(--danger)" }}
                >
                  停止
                </button>
              </div>
            )}

            <div style={{ display: "flex", gap: 8 }}>
              <button
                className="software-primary"
                onClick={() => void (mode === "teamwork" ? startTeamwork() : start())}
                disabled={running}
                style={{ flex: 1 }}
              >
                {running
                  ? mode === "discuss"
                    ? "讨论中…"
                    : "协作中…"
                  : mode === "discuss"
                    ? "开始会议"
                    : "开始协作"}
              </button>
              {!running && (messages.length > 0 || teamEntries.length > 0) && (
                <button className="software-refresh" onClick={reset}>
                  清空
                </button>
              )}
            </div>

            {mode === "discuss" && !running && messages.length > 0 && (
              <button
                className="software-primary"
                onClick={() => void makeSummary()}
                disabled={summarizing}
                style={{ width: "100%" }}
              >
                {summarizing ? "总结生成中…" : "生成会议总结"}
              </button>
            )}
            <div style={{ fontSize: 10, color: "var(--text-faint)" }}>
              {mode === "discuss"
                ? "各成员按顺序轮流发言，每轮都能看到此前全部发言；发言失败会跳过该成员并提示。可随时「打断」当前发言或「停止」整场会议。"
                : "负责人先拆解任务，各成员分工用共享工具（读文件、查库、检索、访问接口等）执行，最后负责人汇总成完整交付物。可随时「停止」。"}
            </div>
          </div>

          {/* 右栏：发言记录 */}
          <div
            ref={listRef}
            style={{
              flex: 1,
              minWidth: 0,
              padding: "14px 16px",
              overflowY: "auto",
              display: "flex",
              flexDirection: "column",
              gap: 12,
            }}
          >
            {mode === "discuss" && messages.length === 0 && !active && (
              <div
                style={{
                  color: "var(--text-faint)",
                  fontSize: 12,
                  margin: "auto",
                  textAlign: "center",
                  lineHeight: 1.8,
                }}
              >
                会议尚未开始。
                <br />
                左侧选择成员与主题后，点击「开始会议」。
              </div>
            )}

            {mode === "discuss" && messages.map((m, i) => {
              const ci = participants.findIndex((p) => p.id === m.speaker_id);
              const color = SPEAKER_COLORS[(ci >= 0 ? ci : i) % SPEAKER_COLORS.length];
              return (
                <div key={`${m.speaker_id}-${m.round}-${i}`} style={{ animation: "fade-in .2s ease" }}>
                  <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 4 }}>
                    <span
                      style={{
                        width: 20,
                        height: 20,
                        borderRadius: "50%",
                        background: color,
                        color: "#0b1020",
                        fontWeight: 700,
                        fontSize: 11,
                        display: "flex",
                        alignItems: "center",
                        justifyContent: "center",
                      }}
                    >
                      {m.speaker_name.slice(0, 1)}
                    </span>
                    <span style={{ fontSize: 13, fontWeight: 600, color: color }}>{m.speaker_name}</span>
                    <span className="software-badge" style={{ fontSize: 10 }}>
                      第 {m.round} 轮
                    </span>
                    {m.interrupted && (
                      <span
                        className="software-badge"
                        style={{ fontSize: 10, color: "var(--amber, #f59e0b)", borderColor: "var(--amber, #f59e0b)" }}
                      >
                        已打断
                      </span>
                    )}
                  </div>
                  <div
                    style={{
                      fontSize: 13,
                      lineHeight: 1.75,
                      color: "var(--text)",
                      whiteSpace: "pre-wrap",
                      wordBreak: "break-word",
                    }}
                  >
                    {m.content}
                  </div>
                  {m.tools_used && m.tools_used.length > 0 && (
                    <div
                      style={{
                        marginTop: 8,
                        padding: "6px 8px",
                        background: "rgba(34,211,238,0.05)",
                        border: "1px solid var(--border-soft)",
                        borderRadius: 8,
                        fontSize: 12,
                      }}
                    >
                      <div style={{ color: "var(--text-faint)", marginBottom: 4 }}>
                        工具调用（{m.tools_used.length}）
                      </div>
                      {m.tools_used.map((t: MeetingToolUse, idx) => (
                        <div key={idx} style={{ marginLeft: 8, marginBottom: 6 }}>
                          <div style={{ color: "var(--cyan)", fontWeight: 600 }}>{t.tool}</div>
                          <div style={{ fontSize: 11, color: "var(--text-dim)", wordBreak: "break-all" }}>
                            参数：{JSON.stringify(t.args)}
                          </div>
                          <div style={{ fontSize: 11, color: "var(--text)", wordBreak: "break-word" }}>
                            结果：{t.result}
                          </div>
                        </div>
                      ))}
                    </div>
                  )}
                  {speakErrors[m.speaker_id] && (
                    <div style={{ fontSize: 10, color: "var(--danger)", marginTop: 4 }}>
                      上次发言失败：{speakErrors[m.speaker_id]}
                    </div>
                  )}
                </div>
              );
            })}

            {mode === "discuss" && active && (
              <div style={{ animation: "fade-in .2s ease" }}>
                <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 4 }}>
                  <span
                    style={{
                      width: 20,
                      height: 20,
                      borderRadius: "50%",
                      background: "var(--cyan)",
                      color: "#0b1020",
                      fontWeight: 700,
                      fontSize: 11,
                      display: "flex",
                      alignItems: "center",
                      justifyContent: "center",
                    }}
                  >
                    {active.name.slice(0, 1)}
                  </span>
                  <span style={{ fontSize: 13, fontWeight: 600, color: "var(--cyan)" }}>
                    {active.name}
                  </span>
                  <span className="software-badge" style={{ fontSize: 10 }}>
                    第 {active.round} 轮
                  </span>
                  <span style={{ fontSize: 11, color: "var(--text-faint)" }}>正在发言…</span>
                </div>
                <div
                  style={{
                    fontSize: 13,
                    lineHeight: 1.75,
                    color: "var(--text)",
                    whiteSpace: "pre-wrap",
                    wordBreak: "break-word",
                  }}
                >
                  {streamText}
                  <span
                    style={{
                      display: "inline-block",
                      width: 7,
                      height: 14,
                      background: "var(--cyan)",
                      marginLeft: 2,
                      verticalAlign: "-2px",
                      animation: "pulse 1s infinite",
                    }}
                  />
                </div>
                {toolActivity && (
                  <div
                    style={{
                      display: "flex",
                      alignItems: "center",
                      gap: 6,
                      marginTop: 8,
                      padding: "6px 8px",
                      fontSize: 12,
                      color: "var(--amber, #f59e0b)",
                      background: "rgba(245,158,11,0.08)",
                      border: "1px solid var(--amber, #f59e0b)",
                      borderRadius: 8,
                    }}
                  >
                    <span>{toolActivity}</span>
                    <span style={{ marginLeft: "auto", fontSize: 11, color: "var(--text-faint)" }}>
                      处理中…
                    </span>
                  </div>
                )}
              </div>
            )}

            {mode === "discuss" && (summary || summarizing) && (
              <div
                style={{
                  borderTop: "1px solid var(--border-soft)",
                  marginTop: 4,
                  paddingTop: 12,
                }}
              >
                <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 6 }}>
                  <span style={{ fontSize: 12, fontWeight: 700, color: "var(--cyan)" }}>
                    会议总结
                  </span>
                  {summarizing && (
                    <span style={{ fontSize: 11, color: "var(--text-faint)" }}>生成中…</span>
                  )}
                </div>
                <div
                  style={{
                    fontSize: 13,
                    lineHeight: 1.8,
                    color: "var(--text)",
                    whiteSpace: "pre-wrap",
                    wordBreak: "break-word",
                    background: "rgba(34,211,238,0.06)",
                    border: "1px solid var(--border-soft)",
                    borderRadius: 10,
                    padding: "10px 12px",
                  }}
                >
                  {summary}
                  {summarizing && (
                    <span
                      style={{
                        display: "inline-block",
                        width: 7,
                        height: 14,
                        background: "var(--cyan)",
                        marginLeft: 2,
                        verticalAlign: "-2px",
                        animation: "pulse 1s infinite",
                      }}
                    />
                  )}
                </div>
              </div>
            )}

            {mode === "teamwork" && (
              <>
                {teamEntries.length === 0 && !teamStage && !teamStreamText && (
                  <div
                    style={{
                      color: "var(--text-faint)",
                      fontSize: 12,
                      margin: "auto",
                      textAlign: "center",
                      lineHeight: 1.8,
                    }}
                  >
                    协作执行尚未开始。
                    <br />
                    左侧选择成员与任务目标后，点击「开始协作」。
                  </div>
                )}

                {teamEntries.map((e, idx) => (
                  <div key={idx} style={{ animation: "fade-in .2s ease" }}>
                    <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 4 }}>
                      <span style={{ fontSize: 13, fontWeight: 600, color: "var(--cyan)" }}>
                        {e.speaker_name}
                      </span>
                      <span className="software-badge" style={{ fontSize: 10 }}>
                        {e.kind === "plan" && "任务拆解"}
                        {e.kind === "task" && "子任务成果"}
                        {e.kind === "summary" && "最终交付物"}
                      </span>
                      {e.interrupted && (
                        <span
                          className="software-badge"
                          style={{ fontSize: 10, color: "var(--amber, #f59e0b)", borderColor: "var(--amber, #f59e0b)" }}
                        >
                          已打断
                        </span>
                      )}
                    </div>
                    <div
                      style={{
                        fontSize: 13,
                        lineHeight: 1.75,
                        color: "var(--text)",
                        whiteSpace: "pre-wrap",
                        wordBreak: "break-word",
                      }}
                    >
                      <div style={{ fontWeight: 600, marginBottom: 4 }}>{e.title}</div>
                      {e.content}
                    </div>
                    {e.tools_used && e.tools_used.length > 0 && (
                      <div
                        style={{
                          marginTop: 8,
                          padding: "6px 8px",
                          background: "rgba(34,211,238,0.05)",
                          border: "1px solid var(--border-soft)",
                          borderRadius: 8,
                          fontSize: 12,
                        }}
                      >
                        <div style={{ color: "var(--text-faint)", marginBottom: 4 }}>
                          工具调用（{e.tools_used.length}）
                        </div>
                        {e.tools_used.map((t: MeetingToolUse, ti) => (
                          <div key={ti} style={{ marginLeft: 8, marginBottom: 6 }}>
                            <div style={{ color: "var(--cyan)", fontWeight: 600 }}>{t.tool}</div>
                            <div style={{ fontSize: 11, color: "var(--text-dim)", wordBreak: "break-all" }}>
                              参数：{JSON.stringify(t.args)}
                            </div>
                            <div style={{ fontSize: 11, color: "var(--text)", wordBreak: "break-word" }}>
                              结果：{t.result}
                            </div>
                          </div>
                        ))}
                      </div>
                    )}
                  </div>
                ))}

                {(teamStage || teamStreamText || teamToolActivity) && (
                  <div style={{ animation: "fade-in .2s ease" }}>
                    <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 4 }}>
                      <span style={{ fontSize: 13, fontWeight: 600, color: "var(--cyan)" }}>
                        {teamStage || "执行中"}
                      </span>
                      <span style={{ fontSize: 11, color: "var(--text-faint)" }}>处理中…</span>
                    </div>
                    {teamStreamText && (
                      <div
                        style={{
                          fontSize: 13,
                          lineHeight: 1.75,
                          color: "var(--text)",
                          whiteSpace: "pre-wrap",
                          wordBreak: "break-word",
                        }}
                      >
                        {teamStreamText}
                        <span
                          style={{
                            display: "inline-block",
                            width: 7,
                            height: 14,
                            background: "var(--cyan)",
                            marginLeft: 2,
                            verticalAlign: "-2px",
                            animation: "pulse 1s infinite",
                          }}
                        />
                      </div>
                    )}
                    {teamToolActivity && (
                      <div
                        style={{
                          display: "flex",
                          alignItems: "center",
                          gap: 6,
                          marginTop: 8,
                          padding: "6px 8px",
                          fontSize: 12,
                          color: "var(--amber, #f59e0b)",
                          background: "rgba(245,158,11,0.08)",
                          border: "1px solid var(--amber, #f59e0b)",
                          borderRadius: 8,
                        }}
                      >
                            <span>{teamToolActivity}</span>
                        <span style={{ marginLeft: "auto", fontSize: 11, color: "var(--text-faint)" }}>
                          处理中…
                        </span>
                      </div>
                    )}
                  </div>
                )}
              </>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}