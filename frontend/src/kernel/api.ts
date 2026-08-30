import { invoke as rawInvoke } from "@tauri-apps/api/core";
import { listen as rawListen } from "@tauri-apps/api/event";

/** 浏览器预览模式（非 Tauri WebView）下优雅降级：invoke 报错、listen 静默空转 */
const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!inTauri) return Promise.reject(new Error("Tauri 运行时不可用（浏览器预览模式）"));
  return rawInvoke<T>(cmd, args);
}

function listen<T>(event: string, handler: (e: { payload: T }) => void): Promise<() => void> {
  if (!inTauri) return Promise.resolve(() => {});
  return rawListen<T>(event, handler);
}
import type {
  AcuiCardData,
  BrowserState,
  ChatMsg,
  Conversation,
  EscalationLevelEvent,
  EscalationUpdate,
  MarkdownState,
  McpConfig,
  MemoryGraph,
  MemoryRow,
  MessageRow,
  ModelAnswer,
  ModelConfig,
  NotifyConfig,
  PermissionRequest,
  ProactiveCard,
  RagDoc,
  DbConnection,
  RagHit,
  RuntimeConfig,
  ThoughtEvent,
  Todo,
  TokenSaverConfig,
  ImageCapability,
  WorkModeInfo,
  WorkModeState,
  EnvCheckResult,
  SoftwareSearchResult,
  SystemConfig,
  DiskInfo,
  ScheduledJob,
  JobRun,
  Workflow,
  WorkflowRun,
  PlazaItem,
  WeChatStatus,
  FeishuStatus,
  ImChannelInfo,
  ImLogEntry,
  PermissionChannelUpdate,
  MeetingParticipant,
  MeetingUtterance,
  TeamEntry,
} from "./types";

export async function chat(
  convId: string,
  message: string,
  history: ChatMsg[],
  attachments: string[] = []
): Promise<string> {
  return invoke<string>("chat", { convId, message, history, attachments });
}

/** 对话分支：同一问题并行对比所有可用模型，返回各模型应答 */
export async function compareModels(
  message: string,
  history: ChatMsg[]
): Promise<ModelAnswer[]> {
  return invoke<ModelAnswer[]>("compare_models", { message, history });
}

/** 多 Agent 会议室：成员按序轮流发言，返回完整发言记录 */
export async function runMeeting(
  topic: string,
  participants: MeetingParticipant[],
  rounds: number
): Promise<MeetingUtterance[]> {
  return invoke<MeetingUtterance[]>("run_meeting", { topic, participants, rounds });
}

/** 订阅「某成员即将发言」事件（会议室内实时展示） */
export async function onMeetingSpeaker(
  cb: (e: { speaker_id: string; speaker_name: string; round: number }) => void
): Promise<() => void> {
  const unlisten = await listen<{ speaker_id: string; speaker_name: string; round: number }>(
    "meeting-speaker",
    (e) => cb(e.payload)
  );
  return unlisten;
}

/** 订阅「发言 token 实时流」事件 */
export async function onMeetingToken(
  cb: (e: { speaker_id: string; token: string }) => void
): Promise<() => void> {
  const unlisten = await listen<{ speaker_id: string; token: string }>(
    "meeting-token",
    (e) => cb(e.payload)
  );
  return unlisten;
}

/** 订阅「某成员调用共享工具」事件（会议室内实时展示工具活动） */
export async function onMeetingTool(
  cb: (e: { speaker_id: string; speaker_name: string; tool: string; args: unknown }) => void
): Promise<() => void> {
  const unlisten = await listen<{
    speaker_id: string;
    speaker_name: string;
    tool: string;
    args: unknown;
  }>("meeting-tool", (e) => cb(e.payload));
  return unlisten;
}

/** 订阅「某成员发言失败」事件 */
export async function onMeetingError(
  cb: (e: { speaker_id: string; speaker_name: string; error: string }) => void
): Promise<() => void> {
  const unlisten = await listen<{ speaker_id: string; speaker_name: string; error: string }>(
    "meeting-error",
    (e) => cb(e.payload)
  );
  return unlisten;
}

/** 订阅「某成员完成一条发言」事件（含正常与被打断，交互实时追加） */
export async function onMeetingUtterance(
  cb: (e: MeetingUtterance) => void
): Promise<() => void> {
  const unlisten = await listen<MeetingUtterance>("meeting-utterance", (e) => cb(e.payload));
  return unlisten;
}

/** 订阅「会议总结 token 实时流」事件 */
export async function onMeetingSummaryToken(
  cb: (e: { token: string }) => void
): Promise<() => void> {
  const unlisten = await listen<{ token: string }>("meeting-summary-token", (e) =>
    cb(e.payload)
  );
  return unlisten;
}

/** 打断当前正在发言的成员（继续下一位） */
export async function interruptMeeting(): Promise<void> {
  return invoke<void>("meeting_interrupt");
}

/** 停止整场会议 */
export async function stopMeeting(): Promise<void> {
  return invoke<void>("meeting_stop");
}

/** 用激活模型对整场发言生成总结 */
export async function summarizeMeeting(
  topic: string,
  utterances: MeetingUtterance[]
): Promise<string> {
  return invoke<string>("summarize_meeting", { topic, utterances });
}

// ---------------- 协作执行（负责人拆解 → 分工执行 → 汇总交付） ----------------

/** 协作执行：负责人拆解任务、各成员用共享工具执行、最终汇总，返回完整条目 */
export async function runTeamwork(
  topic: string,
  participants: MeetingParticipant[]
): Promise<TeamEntry[]> {
  return invoke<TeamEntry[]>("run_teamwork", { topic, participants });
}

/** 订阅「协作执行阶段变化」事件（plan / task / summary） */
export async function onTeamworkStage(
  cb: (e: { stage: string; label: string }) => void
): Promise<() => void> {
  const unlisten = await listen<{ stage: string; label: string }>(
    "teamwork-stage",
    (e) => cb(e.payload)
  );
  return unlisten;
}

/** 订阅「协作执行 token 实时流」事件 */
export async function onTeamworkToken(
  cb: (e: { speaker_id: string; token: string }) => void
): Promise<() => void> {
  const unlisten = await listen<{ speaker_id: string; token: string }>(
    "teamwork-token",
    (e) => cb(e.payload)
  );
  return unlisten;
}

/** 订阅「协作执行中调用共享工具」事件 */
export async function onTeamworkTool(
  cb: (e: { speaker_id: string; speaker_name: string; tool: string; args: unknown }) => void
): Promise<() => void> {
  const unlisten = await listen<{
    speaker_id: string;
    speaker_name: string;
    tool: string;
    args: unknown;
  }>("teamwork-tool", (e) => cb(e.payload));
  return unlisten;
}

/** 订阅「协作执行完成一条产出」事件（规划 / 任务 / 汇总） */
export async function onTeamworkEntry(
  cb: (e: TeamEntry) => void
): Promise<() => void> {
  const unlisten = await listen<TeamEntry>("teamwork-entry", (e) => cb(e.payload));
  return unlisten;
}

export async function listConversations(): Promise<Conversation[]> {
  return invoke<Conversation[]>("list_conversations");
}

export async function createConversation(title: string): Promise<Conversation> {
  return invoke<Conversation>("create_conversation", { title });
}

export async function deleteConversation(id: string): Promise<boolean> {
  return invoke<boolean>("delete_conversation", { id });
}

export async function getMessages(convId: string): Promise<MessageRow[]> {
  return invoke<MessageRow[]>("get_messages", { convId });
}

/** 导出对话：弹出另存为对话框，返回保存路径（用户取消返回 null） */
export async function exportConversation(convId: string): Promise<string | null> {
  return invoke<string | null>("export_conversation", { convId });
}

export async function listFiles(path: string): Promise<unknown> {
  return invoke("list_files", { path });
}

export async function readFile(path: string): Promise<unknown> {
  return invoke("read_file", { path });
}

export async function getPendingPermissions(): Promise<PermissionRequest[]> {
  return invoke<PermissionRequest[]>("get_pending_permissions");
}

export async function resolvePermission(id: string, approved: boolean, remember?: boolean): Promise<boolean> {
  return invoke<boolean>("resolve_permission", { id, approved, remember: remember ?? false });
}

/** 订阅后端推送的审批请求；返回取消订阅函数 */
export async function onPermissionRequest(
  cb: (req: PermissionRequest) => void
): Promise<() => void> {
  const unlisten = await listen<PermissionRequest>("permission-request", (e) => cb(e.payload));
  return unlisten;
}

/** 订阅后端推送的审批回传通道标注（哪个通道实际送达了审批） */
export async function onPermissionChannel(
  cb: (e: PermissionChannelUpdate) => void
): Promise<() => void> {
  const unlisten = await listen<PermissionChannelUpdate>("permission-channel", (e) => cb(e.payload));
  return unlisten;
}

export async function getModelConfig(): Promise<ModelConfig> {
  return invoke<ModelConfig>("get_model_config");
}

export async function setModelConfig(config: ModelConfig): Promise<ModelConfig> {
  return invoke<ModelConfig>("set_model_config", { config });
}

/** 全局切换当前激活模型（输入框下拉切换，立即生效并持久化） */
export async function setActiveModel(id: string): Promise<ModelConfig> {
  return invoke<ModelConfig>("set_active_model", { id });
}

export async function getTokenSaverConfig(): Promise<TokenSaverConfig> {
  return invoke<TokenSaverConfig>("get_token_saver_config");
}

export async function setTokenSaverConfig(config: TokenSaverConfig): Promise<void> {
  return invoke<void>("set_token_saver_config", { config });
}

export async function detectImageModel(): Promise<ImageCapability> {
  return invoke<ImageCapability>("detect_image_model");
}

export async function generateImage(prompt: string, size?: string): Promise<string> {
  return invoke<string>("generate_image", { prompt, size });
}

export async function getMcpConfig(): Promise<McpConfig> {
  return invoke<McpConfig>("get_mcp_config");
}

export async function setMcpConfig(config: McpConfig): Promise<McpConfig> {
  return invoke<McpConfig>("set_mcp_config", { config });
}

export async function getMemories(): Promise<MemoryRow[]> {
  return invoke<MemoryRow[]>("get_memories");
}

export async function getMemoryGraph(): Promise<MemoryGraph> {
  return invoke<MemoryGraph>("get_memory_graph");
}

// ---------------- 知识库管理 ----------------

export async function getRagState(): Promise<RagDoc[]> {
  return invoke<RagDoc[]>("get_rag_state");
}

export async function indexRagDir(path: string): Promise<{ ok: boolean; path: string; chunks: number }> {
  return invoke("index_rag_dir", { path });
}

export async function clearRag(): Promise<boolean> {
  return invoke<boolean>("clear_rag");
}

export async function searchRag(query: string): Promise<{ count: number; hits: RagHit[] }> {
  return invoke("search_rag", { query });
}

// ---------------- 数据库连接配置 ----------------

export async function getDbConnections(): Promise<DbConnection[]> {
  return invoke<DbConnection[]>("get_db_connections");
}

export async function saveDbConnections(connections: DbConnection[]): Promise<boolean> {
  return invoke<boolean>("save_db_connections", { connections });
}

/** 订阅后端「主动提醒」事件 */
export async function onProactive(cb: (card: ProactiveCard) => void): Promise<() => void> {
  const unlisten = await listen<ProactiveCard>("proactive", (e) => cb(e.payload));
  return unlisten;
}

/** 订阅 ACUI 受控卡片事件 */
export async function onAcuiCard(cb: (card: AcuiCardData) => void): Promise<() => void> {
  const unlisten = await listen<AcuiCardData>("acui-card", (e) => cb(e.payload));
  return unlisten;
}

/** 订阅后端「思考流」事件 */
export async function onThought(cb: (t: ThoughtEvent) => void): Promise<() => void> {
  const unlisten = await listen<{
    kind: string;
    label: string;
    detail: string;
    progress?: number;
    phase?: string;
    vendor?: string;
    version?: string;
    homepage?: string;
  }>(
    "thought",
    (e) => {
      const p = e.payload;
      cb({
        ts: Date.now(),
        kind: (p.kind as ThoughtEvent["kind"]) ?? "thinking",
        label: p.label ?? "",
        detail: p.detail ?? "",
        ...(typeof p.progress === "number" ? { progress: p.progress } : {}),
        ...(p.phase ? { phase: p.phase } : {}),
        ...(p.vendor ? { vendor: p.vendor } : {}),
        ...(p.version ? { version: p.version } : {}),
        ...(p.homepage ? { homepage: p.homepage } : {}),
      });
    }
  );
  return unlisten;
}

/** 订阅「记忆召回」事件（携带实际召回的记忆 id，供意识网络精确高亮） */
export async function onMemoryRecall(cb: (ids: string[]) => void): Promise<() => void> {
  const unlisten = await listen<{ ids: string[] }>("memory-recall", (e) => cb(e.payload.ids));
  return unlisten;
}

// ---------------- 内置浏览器 / Markdown 文档（独立窗口） ----------------

export async function getBrowserState(): Promise<BrowserState> {
  return invoke<BrowserState>("get_browser_state");
}

export async function switchBrowserTab(id: string): Promise<boolean> {
  return invoke<boolean>("switch_browser_tab", { id });
}

export async function closeBrowserTab(id: string): Promise<boolean> {
  return invoke<boolean>("close_browser_tab", { id });
}

/** 前端「预览」按钮：把完整 HTML 页面代码打开到内置浏览器窗口 */
export async function previewHtml(html: string, title?: string): Promise<string> {
  return invoke<string>("preview_html", { html, title: title ?? "HTML 预览" });
}

export async function getMarkdownState(): Promise<MarkdownState> {
  return invoke<MarkdownState>("get_markdown_state");
}

export async function switchMarkdownTab(id: string): Promise<boolean> {
  return invoke<boolean>("switch_markdown_tab", { id });
}

export async function closeMarkdownTab(id: string): Promise<boolean> {
  return invoke<boolean>("close_markdown_tab", { id });
}

/** 保存文档：弹出另存为对话框，返回保存路径（用户取消返回 null） */
export async function saveMarkdown(title: string, content: string): Promise<string | null> {
  return invoke<string | null>("save_markdown", { title, content });
}

/** 弹出文件选择对话框（多选），返回绝对路径列表（取消返回 null） */
export async function pickFiles(): Promise<string[] | null> {
  return invoke<string[] | null>("pick_files");
}

/** 弹出文件夹选择对话框，返回绝对路径（取消返回 null） */
export async function pickFolder(): Promise<string | null> {
  return invoke<string | null>("pick_folder");
}

/** 设置当前工作空间（后端强绑定）；传 null/空串清除 */
export async function setWorkspace(path: string | null): Promise<void> {
  await invoke("set_workspace", { path: path ?? "" });
}

/** 订阅后端推送的浏览器窗口更新（导航 / 搜索 / 渲染 HTML） */
export async function onBrowserUpdate(cb: (s: BrowserState) => void): Promise<() => void> {
  const unlisten = await listen<BrowserState>("browser-update", (e) => cb(e.payload));
  return unlisten;
}

/** 订阅后端推送的文档窗口更新（写入 / 追加） */
export async function onMarkdownUpdate(cb: (s: MarkdownState) => void): Promise<() => void> {
  const unlisten = await listen<MarkdownState>("markdown-update", (e) => cb(e.payload));
  return unlisten;
}

/** 停止当前正在进行的对话 */
export async function stopChat(): Promise<boolean> {
  return invoke<boolean>("stop_chat");
}

/** 读取持久化的语音音色 */
export async function getVoice(): Promise<string> {
  return invoke<string>("get_voice");
}

/** 持久化语音音色 */
export async function setVoice(voice: string): Promise<void> {
  await invoke("set_voice", { voice });
}

/** 读取运行时配置（embedding/vision 模型） */
export async function getRuntimeConfig(): Promise<RuntimeConfig> {
  return invoke<RuntimeConfig>("get_runtime_config");
}

/** 保存运行时配置 */
export async function setRuntimeConfig(config: RuntimeConfig): Promise<void> {
  await invoke("set_runtime_config", { config });
}

/** 订阅后端推送的任务拆解列表 */
export async function onTodoList(cb: (todos: Todo[]) => void): Promise<() => void> {
  const unlisten = await listen<{ todos: Todo[] }>("todo-list", (e) => cb(e.payload.todos));
  return unlisten;
}

/** 订阅后端推送的任务步骤状态更新 */
export async function onTodoUpdate(cb: (todos: Todo[]) => void): Promise<() => void> {
  const unlisten = await listen<{ todos: Todo[] }>("todo-update", (e) => cb(e.payload.todos));
  return unlisten;
}

/** 订阅后端流式回答的 token 片段 */
export async function onChatToken(cb: (token: string) => void): Promise<() => void> {
  const unlisten = await listen<{ token: string }>("chat-token", (e) => cb(e.payload.token));
  return unlisten;
}

/** 订阅后端「本轮为工具调用、清空过渡内容」事件 */
export async function onChatRoundReset(cb: () => void): Promise<() => void> {
  const unlisten = await listen("chat-round-reset", () => cb());
  return unlisten;
}

// ---------------- 通知升级 ----------------

/** 订阅后端推送的升级状态更新 */
export async function onEscalationUpdate(
  cb: (e: EscalationUpdate) => void
): Promise<() => void> {
  const unlisten = await listen<EscalationUpdate>("escalation-update", (e) => cb(e.payload));
  return unlisten;
}

/** 订阅后端推送的升级级别通知（带具体动作） */
export async function onEscalationLevel(
  cb: (e: EscalationLevelEvent) => void
): Promise<() => void> {
  const unlisten = await listen<EscalationLevelEvent>("escalation-level", (e) => cb(e.payload));
  return unlisten;
}

/** 订阅后端推送的升级取消事件（用户已响应） */
export async function onEscalationCancelled(
  cb: (e: { approval_id: string }) => void
): Promise<() => void> {
  const unlisten = await listen<{ approval_id: string }>("escalation-cancelled", (e) => cb(e.payload));
  return unlisten;
}

/** 读取通知升级配置 */
export async function getNotifyConfig(): Promise<NotifyConfig> {
  return invoke<NotifyConfig>("get_notify_config");
}

/** 保存通知升级配置 */
export async function setNotifyConfig(config: NotifyConfig): Promise<void> {
  await invoke("set_notify_config", { config });
}

// ---------------- 工作模式（软件测试工程师 / 开发工程师） ----------------

export async function getWorkModes(): Promise<WorkModeInfo[]> {
  return invoke<WorkModeInfo[]>("get_work_modes");
}

export async function getWorkMode(): Promise<WorkModeState> {
  return invoke<WorkModeState>("get_work_mode");
}

export async function setWorkMode(id: string): Promise<{ id: string; label: string }> {
  return invoke("set_work_mode", { id });
}

/** 订阅后端推送的模式切换事件 */
export async function onWorkModeChange(
  cb: (m: { id: string; label: string }) => void
): Promise<() => void> {
  const unlisten = await listen<{ id: string; label: string }>("workmode-change", (e) => cb(e.payload));
  return unlisten;
}

// ---------------- 内置终端 ----------------

/** 打开内置终端窗口并启动会话 */
export async function openTerminalWindow(): Promise<void> {
  await invoke("open_terminal_window");
}

/** 启动/复用终端 PTY 会话（前端终端窗口挂载时调用） */
export async function termSpawn(): Promise<void> {
  await invoke("term_spawn");
}

/** 向终端写入输入（用户按键回送） */
export async function termWrite(data: string): Promise<void> {
  await invoke("term_write", { data });
}

/** 调整终端行列尺寸 */
export async function termResize(rows: number, cols: number): Promise<void> {
  await invoke("term_resize", { rows, cols });
}

/** 结束终端会话 */
export async function termClose(): Promise<void> {
  await invoke("term_close");
}

/** 订阅终端输出数据 */
export async function onTermData(cb: (data: string) => void): Promise<() => void> {
  const unlisten = await listen<string>("term-data", (e) => cb(e.payload));
  return unlisten;
}

// ---------------- 软件管家 ----------------

/** 环境探测：包管理器 / 运行时 / 管理员权限 */
export async function envCheck(): Promise<EnvCheckResult> {
  return invoke<EnvCheckResult>("env_check");
}

/** 搜索软件（走系统包管理器） */
export async function softwareSearch(query: string): Promise<SoftwareSearchResult> {
  return invoke<SoftwareSearchResult>("software_search", { query });
}

/** 已安装软件列表 */
export async function softwareList(): Promise<SoftwareSearchResult> {
  return invoke<SoftwareSearchResult>("software_list");
}

/** 读系统配置（环境变量 / PATH / 启动项） */
export async function systemGet(): Promise<SystemConfig> {
  return invoke<SystemConfig>("system_get");
}

/** 磁盘空间与装机习惯 + 推荐安装位置 */
export async function diskInfo(): Promise<DiskInfo> {
  return invoke<DiskInfo>("disk_info");
}

// ---------------- 定时任务编排 ----------------

/** 列出全部定时任务 */
export async function scheduleListJobs(): Promise<ScheduledJob[]> {
  return invoke<ScheduledJob[]>("schedule_list_jobs");
}

/** 新增定时任务 */
export async function scheduleAddJob(
  cronExpr: string,
  title: string,
  taskType: string,
  task: string
): Promise<ScheduledJob> {
  return invoke<ScheduledJob>("schedule_add_job", {
    cronExpr,
    title,
    taskType,
    task,
  });
}

/** 编辑定时任务 */
export async function scheduleUpdateJob(
  id: string,
  cronExpr: string,
  title: string,
  taskType: string,
  task: string
): Promise<ScheduledJob> {
  return invoke<ScheduledJob>("schedule_update_job", {
    id,
    cronExpr,
    title,
    taskType,
    task,
  });
}

/** 删除定时任务（含执行日志） */
export async function scheduleDeleteJob(id: string): Promise<boolean> {
  return invoke<boolean>("schedule_delete_job", { id });
}

/** 暂停 / 恢复定时任务 */
export async function scheduleSetEnabled(id: string, enabled: boolean): Promise<boolean> {
  return invoke<boolean>("schedule_set_enabled", { id, enabled });
}

/** 查询执行日志（jobId 空查全部） */
export async function scheduleJobLogs(jobId: string, limit = 20): Promise<JobRun[]> {
  return invoke<JobRun[]>("schedule_job_logs", { jobId, limit });
}

/** 清空某任务的执行日志 */
export async function scheduleClearLogs(jobId: string): Promise<number> {
  return invoke<number>("schedule_clear_logs", { jobId });
}

// ---------------- 可编排工作流 ----------------

/** 列出全部工作流（内置 + 用户自定义） */
export async function workflowList(): Promise<Workflow[]> {
  return invoke<Workflow[]>("list_workflows");
}

/** 创建或更新工作流（同名 id 覆盖） */
export async function workflowSave(wf: Workflow): Promise<string> {
  return invoke<string>("add_workflow", { wf });
}

/** 删除自定义工作流（含执行日志） */
export async function workflowDelete(id: string): Promise<boolean> {
  return invoke<boolean>("workflow_delete", { id });
}

/** 执行工作流（结果写入右侧文档窗口），返回最终输出 */
export async function workflowRun(id: string, input: string): Promise<string> {
  return invoke<string>("run_workflow", { id, input });
}

/** 查询执行日志（workflowId 空查全部） */
export async function workflowRuns(workflowId: string, limit = 20): Promise<WorkflowRun[]> {
  return invoke<WorkflowRun[]>("workflow_runs", { workflowId, limit });
}

/** 清空某工作流的执行日志 */
export async function workflowClearRuns(workflowId: string): Promise<number> {
  return invoke<number>("workflow_clear_runs", { workflowId });
}

// ---------------- 任务广场 ----------------

/** 列出任务广场全部条目（内置工具 + 工作流 + 技能 + 自研） */
export async function plazaList(): Promise<PlazaItem[]> {
  return invoke<PlazaItem[]>("plaza_list");
}

/** 保存（新建/覆盖）一个条目；自研 tool 会同步动态加载 */
export async function plazaSaveItem(item: PlazaItem): Promise<string> {
  return invoke<string>("plaza_save_item", { item });
}

/** 删除一个自研条目（内置条目不可删除） */
export async function plazaDeleteItem(id: string): Promise<boolean> {
  return invoke<boolean>("plaza_delete_item", { id });
}

/** 直接运行广场中的一个工具，返回执行结果 */
export async function plazaRun(name: string, args: unknown): Promise<unknown> {
  return invoke("plaza_run", { name, args: args ?? {} });
}

/** 市场仓库目录（未安装也可浏览） */
export async function plazaMarketCatalog(): Promise<PlazaItem[]> {
  return invoke<PlazaItem[]>("plaza_market_catalog");
}

/** 从市场仓库安装一个工具到广场（标记未受信，执行需审批） */
export async function plazaMarketInstall(id: string): Promise<string> {
  return invoke<string>("plaza_market_install", { id });
}

// ---------------- 桌面步骤弹幕浮窗 ----------------

/** 读取当前步骤弹幕历史（浮窗加载时补齐） */
export async function getStepLog(): Promise<string[]> {
  return invoke<string[]>("get_step_log");
}

/** 订阅后端推送的步骤弹幕（浮窗实时滚动） */
export async function onStepPush(cb: (text: string) => void): Promise<() => void> {
  const unlisten = await listen<{ text: string }>("step-push", (e) => cb(e.payload.text));
  return unlisten;
}

// ---------------- 微信机器人 ----------------

/** 查询微信连接状态 */
export async function getWechatStatus(): Promise<WeChatStatus> {
  return invoke<WeChatStatus>("wechat_get_status");
}

/** 扫码登录（阻塞至扫码成功 / 超时 / 取消；期间经 wechat-qr 事件推送二维码） */
export async function wechatLogin(): Promise<WeChatStatus> {
  return invoke<WeChatStatus>("wechat_login");
}

/** 断开长轮询（保留凭证） */
export async function wechatStop(): Promise<WeChatStatus> {
  return invoke<WeChatStatus>("wechat_stop");
}

/** 重新启动长轮询（凭证已存在时） */
export async function wechatStart(): Promise<WeChatStatus> {
  return invoke<WeChatStatus>("wechat_start");
}

/** 登出并清空凭证 */
export async function wechatLogout(): Promise<WeChatStatus> {
  return invoke<WeChatStatus>("wechat_logout");
}

/** 订阅微信连接状态变化 */
export async function onWechatStatus(cb: (s: WeChatStatus) => void): Promise<() => void> {
  const unlisten = await listen<WeChatStatus>("wechat-status", (e) => cb(e.payload));
  return unlisten;
}

/** 订阅扫码登录二维码（内容为 URL/图片，需再渲染成二维码图） */
export async function onWechatQr(cb: (content: string) => void): Promise<() => void> {
  const unlisten = await listen<{ url: string }>("wechat-qr", (e) => cb(e.payload.url));
  return unlisten;
}

// ---------------- 飞书机器人（Lark 自建应用） ----------------

/** 查询飞书连接状态 */
export async function getFeishuStatus(): Promise<FeishuStatus> {
  return invoke<FeishuStatus>("feishu_get_status");
}

/** 保存 / 更新飞书自建应用凭证（app_id / app_secret） */
export async function feishuSaveCredentials(
  appId: string,
  appSecret: string
): Promise<FeishuStatus> {
  return invoke<FeishuStatus>("feishu_save_credentials", { appId, appSecret });
}

/** 启动飞书长连接 */
export async function feishuStart(): Promise<FeishuStatus> {
  return invoke<FeishuStatus>("feishu_start");
}

/** 停止飞书长连接（保留凭证） */
export async function feishuStop(): Promise<FeishuStatus> {
  return invoke<FeishuStatus>("feishu_stop");
}

/** 订阅飞书连接状态变化 */
export async function onFeishuStatus(cb: (s: FeishuStatus) => void): Promise<() => void> {
  const unlisten = await listen<FeishuStatus>("feishu-status", (e) => cb(e.payload));
  return unlisten;
}

// ---------------- IM 消息总线 ----------------

/** 枚举所有 IM 通道状态（微信 / 飞书，统一入口） */
export async function getImChannels(): Promise<ImChannelInfo[]> {
  return invoke<ImChannelInfo[]>("im_list");
}

/** 获取 IM 消息收发日志（手机发来的指令 + 白泽回传的审批/结果） */
export async function getImLog(): Promise<ImLogEntry[]> {
  return invoke<ImLogEntry[]>("im_log");
}
