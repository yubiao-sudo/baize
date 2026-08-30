// 前端与后端之间的消息类型（仅保留 user/assistant 的纯文本历史）
export interface ChatMsg {
  role: "user" | "assistant";
  content: string;
  /** 执行流 JSON（thoughts + todos），仅 assistant 消息可能有值 */
  trace?: string;
  /** 用户消息附带的本地图片/文件路径 */
  attachments?: string[];
  /** 「对话分支」：同一问题多模型并行对比结果（仅对比分支的 assistant 消息有值） */
  branches?: ModelAnswer[];
}

// 「对话分支」单模型应答（与后端 model::ModelAnswer 对齐）
export interface ModelAnswer {
  name: string;
  model: string;
  tier: "local" | "cloud";
  content?: string | null;
  error?: string | null;
}

// 与后端 security::PermissionRequest 对齐
export interface PermissionRequest {
  id: string;
  tool: string;
  args: unknown;
  class: "ReadOnly" | "Write" | "HighRisk";
  /** 富信息：软件安装时附带目标位置/推荐理由/软件名 */
  detail?: InstallDetail | null;
}

// 审批回传通道标注（后端 permission-channel 事件）
export interface PermissionChannelUpdate {
  approval_id: string;
  /** 实际推送成功的通道 id 列表（wechat / feishu）；空数组表示仅桌面端审批 */
  channels: string[];
}

// 软件安装的富信息（后端 install_preview 返回）
export interface InstallDetail {
  id: string;
  name: string;
  target: string;
  drive: string;
  reason: string;
  free_gb: number;
}

// 主动提醒卡片（后端 proactive 事件）
export interface ProactiveCard {
  id: string;
  title: string;
  body: string;
  files: string[];
  /** 可选：点击「让白泽处理」时发送的自定义指令（如续跑任务） */
  action?: string;
  /** 可选：定时任务完成时携带的完整执行结果数据，用于万能卡片展示 */
  data?: string;
}

// ACUI 受控卡片（后端 render_card 工具 → acui-card 事件）
export interface AcuiCardData {
  id: string;
  kind: string; // text | progress | confirm | data
  title: string;
  body: string;
  progress?: number;
  /** data 类型卡片的结构化数据内容（JSON/markdown/表格 markdown） */
  data?: string;
}

// 知识库文档（后端 get_rag_state）
export interface RagDoc {
  path: string;
  chunks: number;
}

// 知识库检索命中（后端 search_rag）
export interface RagHit {
  path: string;
  content: string;
  score: number;
}

// 数据库连接配置（后端 DbConnection）
export interface DbConnection {
  name: string;
  connection: string;
}

// 记忆条目（与后端 memory::MemoryRow 对齐）
export interface MemoryRow {
  mem_id: string;
  content: string;
  kind: string;
  salience: number;
}

// 记忆图谱（后端 memory_graph）
export interface MemoryEdge {
  from: string;
  to: string;
  weight: number;
}
export interface MemoryGraph {
  nodes: MemoryRow[];
  edges: MemoryEdge[];
}

// MCP 配置（与后端 mcp::McpConfig 对齐）
export interface McpConfig {
  enabled: boolean;
  command: string;
  args: string[];
}

// 单个已保存的模型配置（与后端 model::ModelProfile 对齐）
export type ModelTier = "local" | "cloud";
export interface ModelProfile {
  id: string;
  name: string;
  tier: ModelTier;
  base_url: string;
  api_key: string;
  model: string;
  enabled: boolean;
}

// 模型配置（与后端 model::ModelConfig 对齐）
export interface ModelConfig {
  local_enabled: boolean;
  local_url: string;
  local_model: string;
  cloud_enabled: boolean;
  cloud_name: string;
  cloud_base_url: string;
  cloud_api_key: string;
  cloud_model: string;
  priority: "local" | "cloud";
  profiles: ModelProfile[];
  active: string;
}

// 多 Agent 会议室：参会成员（与后端 commands::MeetingParticipant 对齐）
export interface MeetingParticipant {
  id: string;
  name: string;
  role: string;
  profile_id: string;
}

// 多 Agent 会议室：单条发言（与后端 commands::MeetingUtterance 对齐）
export interface MeetingUtterance {
  speaker_id: string;
  speaker_name: string;
  round: number;
  profile_id: string;
  content: string;
  /** 是否因「打断/停止」被截断 */
  interrupted?: boolean;
  /** 本次发言过程中调用过的共享工具 */
  tools_used?: MeetingToolUse[];
}

// 会议中某成员调用的一次共享工具（与后端 commands::MeetingToolUse 对齐）
export interface MeetingToolUse {
  tool: string;
  args: Record<string, unknown>;
  result: string;
}

// 协作执行：负责人拆解出的一个子任务（与后端 commands::TeamTask 对齐）
export interface TeamTask {
  title: string;
  assignee: string;
  detail: string;
}

// 协作执行：一个阶段/子任务的产出条目（与后端 commands::TeamEntry 对齐）
export interface TeamEntry {
  kind: string; // "plan" | "task" | "summary"
  speaker_name: string;
  title: string;
  content: string;
  tools_used: MeetingToolUse[];
  interrupted: boolean;
}

// Token 节约配置（与后端 token_saver::TokenSaverConfig 对齐）
export interface TokenSaverConfig {
  enabled: boolean;
  auto_compress: boolean;
  compress_threshold_chars: number;
  keep_recent_chars: number;
  max_tool_result_chars: number;
  local_only_compress: boolean;
}

// 文生图能力检测结果（与后端 text_to_image::ImageCapability 对齐）
export interface ImageCapability {
  supported: boolean;
  model: string;
  tier: "local" | "cloud" | "";
  source: "name" | "probe" | "none";
  hint: string;
}

// 思考流事件（后端 thought 事件 + 前端种子）
export interface ThoughtEvent {
  id?: string;
  /** 所属会话 id：用于把思考流绑定到当前会话 */
  convId?: string;
  ts: number;
  kind:
    | "thinking"
    | "tool_call"
    | "tool_result"
    | "tool_progress"
    | "permission"
    | "awaken"
    | "memory"
    | "phase"
    | "model"
    | "focus"
    | "plan"
    | "critic"
    | "rag"
    | "mode"
    | "author_tool"
    | "test_pipeline"
    | "saying";
  label: string;
  detail: string;
  /** 进度百分比（0-100），仅 tool_progress 类事件携带，用于渲染进度条 */
  progress?: number;
  /** 进度阶段（check / locate / installing / done / failed） */
  phase?: string;
  /** 软件厂商（publisher），仅 tool_progress 安装事件携带 */
  vendor?: string;
  /** 软件版本号，仅 tool_progress 安装事件携带 */
  version?: string;
  /** 软件官网（用于推导应用图标域名），仅 tool_progress 安装事件携带 */
  homepage?: string;
}

// 内置浏览器窗口状态（与后端 browser::BrowserState 对齐）
export interface SearchResult {
  title: string;
  url: string;
  summary: string;
}

// 浏览器标签页
export interface Tab {
  id: string;
  kind: string;
  title: string;
  content: string;
  active: boolean;
}

export interface BrowserState {
  url: string;
  html: string | null;
  title: string;
  results?: SearchResult[] | null;
  tabs?: Tab[];
}

// 内置 Markdown 文档窗口状态（与后端 markdown::MarkdownState 对齐）
export interface MarkdownDoc {
  id: string;
  title: string;
  content: string;
  active: boolean;
}

export interface MarkdownState {
  docs: MarkdownDoc[];
}

// 任务步骤（与后端 task::Todo 对齐）
export interface Todo {
  id: number;
  title: string;
  status: "pending" | "in_progress" | "completed";
}

// 会话（与后端 memory::ConversationRow 对齐）
export interface Conversation {
  id: string;
  title: string;
  created_at: number;
}

// 运行时配置（与后端 commands::RuntimeConfig 对齐）
export interface RuntimeConfig {
  embed_model: string;
  vision_model: string;
}

// 消息行（与后端 memory::MessageRow 对齐）
export interface MessageRow {
  role: string;
  content: string;
  created_at: number;
  /** 执行流 JSON（thoughts + todos），仅 assistant 消息可能有值 */
  trace: string | null;
  /** 附件路径 JSON 数组字符串 */
  attachments: string | null;
}

// 通知升级事件（后端 escalation-update 事件）
export interface EscalationUpdate {
  approval_id: string;
  level: number;
  level_label: string;
  max_level: boolean;
}

// 通知升级级别事件（后端 escalation-level 事件）
export interface EscalationLevelEvent {
  level: number;
  level_label: string;
  title: string;
  body: string;
  detail: string;
  action?: string;
  tts_text?: string;
  audio_file?: string | null;
  repeat?: boolean;
}

// 通知升级配置（与后端 notify::NotifyConfig 对齐）
export interface EmailConfig {
  smtp_host: string;
  smtp_port: number;
  username: string;
  password: string;
  from: string;
  to: string;
}

export interface WebhookConfig {
  url: string;
  headers: string | null;
}

export interface NotifyConfig {
  enabled: boolean;
  timeouts_sec: number[];
  levels_enabled: boolean[];
  email: EmailConfig | null;
  webhook: WebhookConfig | null;
  voice_text: string | null;
  audio_file: string | null;
}

// 工具自研模板（与后端 workmode::ToolTemplate 对齐）
export interface ToolTemplate {
  name: string;
  description: string;
  hint: string;
}

// 文档模板（与后端 workmode::DocTemplate 对齐）
export interface DocTemplate {
  id: string;
  title: string;
  outline: string[];
}

// 工作模式（与后端 workmode::WorkMode 对齐）
export interface WorkModeInfo {
  id: string;
  label: string;
  description: string;
  system_prompt: string;
  allowed_tools: string[];
  tool_templates: ToolTemplate[];
  doc_templates: DocTemplate[];
  skills: string[];
}

// 当前工作模式状态（与后端 get_work_mode 返回对齐）
export interface WorkModeState {
  current: string | null;
  label: string | null;
  authored: string[];
}

// ─────────── 微信机器人（与后端 wechat.rs 返回对齐）───────────

// 微信连接状态快照（wechat_get_status / wechat-status 事件）
export interface WeChatStatus {
  /** idle（未登录）| qr_pending（等待扫码）| connected（已连接）| disconnected（已断开，保留凭证） */
  status: "idle" | "qr_pending" | "connected" | "disconnected";
  /** 是否已登录凭证 */
  connected: boolean;
  /** ilink bot 账号 id（已登录时非空） */
  account_id: string | null;
}

// ─────────── 飞书机器人（与后端 feishu.rs 返回对齐）───────────

// 飞书连接状态快照（feishu_get_status / feishu-status 事件）
export interface FeishuStatus {
  /** idle | connecting | connected | reconnecting | disconnected */
  status: string;
  /** 是否已配置 app_id / app_secret */
  connected: boolean;
  /** 已配置的 app_id（未配置为 null） */
  app_id: string | null;
}

// ─────────── IM 消息总线（im_list 返回）───────────

// 单个通道在总线中的描述
export interface ImChannelInfo {
  /** wechat / feishu */
  id: string;
  /** 中文名：微信 / 飞书 */
  label: string;
  /** 是否已配置凭证 / 已登录 */
  connected: boolean;
  /** 原始状态字符串 */
  status: string | null;
}

// 单条 IM 消息收发日志（im_log 返回）
export interface ImLogEntry {
  /** 毫秒时间戳 */
  ts: number;
  /** in = 收到（手机发来的指令）/ out = 发出（白泽回传的审批/结果） */
  direction: "in" | "out";
  /** wechat / feishu */
  channel: string;
  /** 中文名：微信 / 飞书 */
  channel_label: string;
  /** 对端标识（微信用户 id / 飞书 chat_id） */
  peer: string;
  text: string;
}

// ─────────── 软件管家（与后端 software.rs 返回对齐）───────────

// 可用的包管理器（detect_package_managers 返回）
export interface PackageManagerInfo {
  id: string;
  label: string;
  available: boolean;
}

// env_check 的环境探测结果
export interface EnvCheckResult {
  os: string;
  is_admin: boolean;
  package_managers: PackageManagerInfo[];
  runtimes: { [name: string]: string | null };
}

// 候选软件包（software_search / software_list 返回）
export interface SoftwarePackage {
  name: string;
  id: string;
  version?: string;
  source?: string;
  publisher?: string;
  location?: string;
}

// software_search 返回
export interface SoftwareSearchResult {
  pm: string;
  packages: SoftwarePackage[];
  raw?: string;
}

// system_get 返回
export interface SystemConfig {
  os: string;
  os_version: string;
  env: { [key: string]: string };
  machine_env: { [key: string]: string };
  path: string[];
  machine_path: string[];
  startup: { [key: string]: string };
  machine_startup: { [key: string]: string };
}

// disk_info 返回（磁盘与装机习惯 + 推荐安装位置）
export interface Disk {
  drive: string;
  label?: string;
  total_gb?: number;
  free_gb?: number;
}

export interface InstallRoot {
  drive: string;
  path: string;
  reason: string;
  free_gb: number;
}

export interface DiskInfo {
  os: string;
  disks: Disk[];
  install_root: InstallRoot;
}

// ─────────── 定时任务编排（与后端 scheduler.rs 对齐）───────────

// 定时任务（后端 scheduler::ScheduledJob）
export interface ScheduledJob {
  id: string;
  /** 显示名（自然语言任务摘录 / 命令摘要） */
  title: string;
  /** cron 表达式，5 段：分 时 日 月 周 */
  cron_expr: string;
  /** "command"（PowerShell 直连）| "agent"（交给白泽 Agent 的自然语言任务） */
  task_type: string;
  /** 载荷：command 类型为 PowerShell 命令；agent 类型为自然语言任务描述 */
  command: string;
  created_at: number;
  enabled: boolean;
  last_run_at: number | null;
  last_result: string | null;
}

// 单次执行日志（后端 scheduler::JobRun）
export interface JobRun {
  id: string;
  job_id: string;
  job_title: string;
  started_at: number;
  finished_at: number | null;
  /** "running" | "success" | "failed" */
  status: string;
  result: string;
}

// ─────────── 可编排工作流（与后端 workflow.rs 对齐）───────────

// 工作流阶段（提示词模板，`{input}` 为上一阶段输出）
export interface WorkflowStage {
  name: string;
  prompt: string;
}

// 工作流定义
export interface Workflow {
  id: string;
  name: string;
  description: string;
  stages: WorkflowStage[];
}

// 工作流单次执行日志
export interface WorkflowRun {
  id: string;
  workflow_id: string;
  workflow_name: string;
  started_at: number;
  finished_at: number | null;
  /** "running" | "success" | "failed" */
  status: string;
  result: string;
}

// ─────────── 任务广场（与后端 plaza.rs 对齐）───────────

// 自研工具载荷（命令 或 脚本）
export interface DiyToolSpec {
  /** shell 命令（占位符替换） */
  command?: string | null;
  /** 脚本语言：python | nodejs */
  lang?: string | null;
  /** 脚本代码 */
  code?: string | null;
  /** 工具入参 JSON Schema */
  parameters?: unknown;
}

// 广场条目：统一描述工具 / 工作流 / 技能 / 自研工具
export interface PlazaItem {
  id: string;
  name: string;
  description: string;
  /** "tool" | "workflow" | "skill" */
  kind: string;
  /** "builtin" | "diy" | "market" */
  source: string;
  category: string;
  tags: string[];
  icon: string;
  /** "trusted" | "authored" | "untrusted" */
  trust: string;
  /** 输出路由：document / terminal / browser / execution_flow / notification / todo / clipboard */
  outputs: string[];
  callable: boolean;
  /** 工具入参 JSON Schema（工具类型） */
  parameters?: unknown | null;
  /** 自研工具载荷 */
  diy?: DiyToolSpec | null;
}
