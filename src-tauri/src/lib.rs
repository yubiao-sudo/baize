mod acui;
mod agent;
mod background;
mod browser;
mod calendar;
mod capability;
mod clipboard;
mod commands;
mod datapipeline;
mod heartbeat;
mod document;
mod email;
mod embedding;
mod feishu;
mod grep;
mod im;
mod updater;
mod markdown;
mod maintenance;
mod mcp;
mod memory;
mod mic;
mod model;
mod multimodal;
mod notify;
mod ocr;
mod panel;
mod plaza;
mod plugin;
mod popup;
mod proactive;
mod rag;
mod read_document;
mod replay;
mod scheduler;
mod security;
mod skill;
mod software;
mod som;
mod spreadsheet;
mod subagent;
mod takeover;
mod task;
mod terminal;
mod test_engineer;
mod text_to_image;
mod text_tools;
mod token_saver;
mod tools;
mod tts;
pub(crate) mod vault;
mod visual_diff;
mod visual_grounding;
mod watchdog;
mod wechat;
mod windows;
mod workflow;
mod workmode;

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use capability::Capability;
use memory::MemoryStore;
use model::{ModelConfig, ModelRouter};
use security::SecurityManager;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::Manager;
use tools::ToolRegistry;

/// 全局共享状态：经 `tauri::Builder::manage` 注入后，各命令通过 `State<AppState>` 访问。
pub struct AppState {
    pub tools: Arc<ToolRegistry>,
    pub model: Arc<ModelRouter>,
    pub security: SecurityManager,
    pub store: Arc<MemoryStore>,
    pub capability: Arc<dyn Capability>,
    pub focus: Mutex<Option<String>>,
    pub browser: Arc<Mutex<browser::BrowserState>>,
    pub markdown: Arc<Mutex<markdown::MarkdownState>>,
    /// 内置终端（PTY 会话）
    pub terminal: Arc<terminal::TerminalState>,
    /// 本地知识库 RAG 索引
    pub rag: Arc<rag::RagIndex>,
    /// 任务步骤列表（任务连续执行）
    pub todos: Arc<Mutex<Vec<task::Todo>>>,
    /// 本轮执行流思考日志（chat 开始时清空，结束后固化为 trace 持久化到消息，供事后回看）
    pub thought_log: Arc<Mutex<Vec<serde_json::Value>>>,
    /// 停止对话标志：置位后当前 chat 尽快返回
    pub cancel: Arc<AtomicBool>,
    /// 通知升级管理器
    pub escalation: Arc<notify::EscalationManager>,
    /// 定时任务调度器（cron）
    pub scheduler: Arc<scheduler::SchedulerState>,
    /// 工作模式注册表（软件测试工程师 / 开发工程师 等）
    pub workmodes: Arc<workmode::WorkModeRegistry>,
    /// 可编排工作流注册表（内置 + 用户自定义，持久化 + 执行日志）
    pub workflows: Arc<workflow::WorkflowRegistry>,
    /// 任务广场注册表（聚合工具 / 工作流 / 技能 / 自研条目）
    pub plaza: Arc<plaza::PlazaRegistry>,
    /// 自主看护 Agent 状态（任务注册表 + 触发器轮询 + 行动引擎）
    pub watchdog: Arc<watchdog::WatchdogState>,
    /// 微信机器人（iLink 协议，手机扫码指挥白泽）
    pub wechat: Arc<wechat::WeChatState>,
    /// 飞书机器人（Lark 自建应用，WebSocket 长连接指挥白泽）
    pub feishu: Arc<feishu::FeishuState>,
    /// 跨 IM 消息总线（统一调度审批回传 / 结果回传）
    pub im_bus: Arc<im::ImBus>,
}

impl AppState {
    fn new() -> Self {
        let store = Arc::new(MemoryStore::open("baize.db").expect("无法打开本地数据库 baize.db"));

        // 运行时配置：从持久化恢复 embedding/vision 模型
        if let Ok(Some(embed)) = store.get_setting("embed_model") {
            embedding::set_embed_model(embed);
        }
        if let Ok(Some(vision)) = store.get_setting("vision_model") {
            visual_grounding::set_vision_model(vision);
        }
        if let Ok(Some(provider)) = store.get_setting("vision_provider") {
            visual_grounding::set_vision_provider(&provider);
        }
        if let Ok(Some(enabled)) = store.get_setting("vision_enabled") {
            if let Ok(b) = enabled.parse::<bool>() {
                visual_grounding::set_vision_enabled(b);
            }
        }

        // Token 节约配置：从持久化恢复（进程内缓存，供热路径使用）
        if let Ok(Some(json)) = store.get_setting("token_saver_config") {
            if let Ok(cfg) = serde_json::from_str::<crate::token_saver::TokenSaverConfig>(&json) {
                crate::token_saver::set_config(cfg);
            }
        }

        // TTS 语音模型配置：从持久化恢复
        if let Ok(Some(json)) = store.get_setting("tts_config") {
            tts::restore(&json);
        }

        // 数据库连接配置：从持久化恢复
        if let Ok(Some(json)) = store.get_setting("db_connections") {
            if let Ok(list) = serde_json::from_str::<Vec<tools::DbConnection>>(&json) {
                tools::refresh_db_connections(&list);
            }
        }

        // 桌面浏览器路径：从持久化恢复（Chrome 发现的设置指定优先级最高）
        if let Ok(Some(p)) = store.get_setting("browser_chrome_path") {
            crate::browser::set_custom_browser_path(Some(&p));
        }

        // 工作模式注册表（内置软件测试工程师 / 开发工程师）
        let workmodes = Arc::new(workmode::WorkModeRegistry::with_builtins());
        // 恢复上次激活的工作模式
        if let Ok(Some(id)) = store.get_setting("work_mode_current") {
            let _ = workmodes.activate(&id);
        }

        // 可编排工作流注册表（内置样例 + 用户自定义，持久化到 SQLite）
        let workflows = Arc::new(workflow::WorkflowRegistry::new(store.clone()));

        let tools = Arc::new(ToolRegistry::new());
        // 内置只读文件工具
        tools.register(Box::new(tools::FileListTool));
        tools.register(Box::new(tools::FileReadTool));
        tools.register(Box::new(document::IngestDocumentTool));
        tools.register(Box::new(read_document::ReadDocumentTool));
        tools.register(Box::new(read_document::DocumentDepsTool));
        tools.register(Box::new(tools::ShellTool));
        // 本机 PowerShell 直连（不走 Docker 沙箱）
        tools.register(Box::new(tools::PsExecTool));
        // P1「网」：HTTP 客户端 + 多渠道推送
        tools.register(Box::new(tools::HttpRequestTool));
        tools.register(Box::new(tools::NotifyTool));
        // 面板控制：agent 自主打开/关闭顶栏功能页面（描述内嵌面板目录，模型据此知晓页面用途）
        tools.register(Box::new(panel::PanelControlTool));
        // P1「网」：邮件 + 数据库
        tools.register(Box::new(tools::MailSendTool::new(store.clone())));
        tools.register(Box::new(tools::MailFetchTool::new(store.clone())));
        tools.register(Box::new(tools::DbQueryTool));
        tools.register(Box::new(tools::DbExecuteTool));
        tools.register(Box::new(tools::DbSchemaTool));
        // 写文件工具（开发工程师落地：写/改/建目录/移动）
        tools.register(Box::new(tools::WriteFileTool));
        tools.register(Box::new(tools::EditFileTool));
        tools.register(Box::new(tools::CreateDirectoryTool));
        tools.register(Box::new(tools::MoveFileTool));
        tools.register(Box::new(tools::UndoTool));
        // 技能学习库（内置技能 + 已学习技能持久化到 skills 表）；skill_run 需 AppHandle，在 setup 注册
        let skill_lib = Arc::new(skill::SkillLibrary::new(store.clone()));
        tools.register(Box::new(skill::SkillListTool::new(skill_lib.clone())));
        tools.register(Box::new(skill::SkillGetTool::new(skill_lib.clone())));
        tools.register(Box::new(skill::SkillLearnTool::new(skill_lib.clone())));
        tools.register(Box::new(skill::SkillDeleteTool::new(skill_lib.clone())));

        // 长期记忆层：记忆的主动记录 / 检索 / 列出 / 删除 / 整理 / 画像 / 语义记忆
        tools.register(Box::new(memory::tools::RememberTool::new(store.clone())));
        tools.register(Box::new(memory::tools::MemorySearchTool::new(store.clone())));
        tools.register(Box::new(memory::tools::MemoryListTool::new(store.clone())));
        tools.register(Box::new(memory::tools::MemoryDeleteTool::new(store.clone())));
        tools.register(Box::new(memory::tools::MemoryConsolidateTool::new(store.clone())));
        tools.register(Box::new(memory::tools::MemoryProfileTool::new(store.clone())));
        tools.register(Box::new(memory::tools::MemorySemanticAddTool::new(store.clone())));
        tools.register(Box::new(memory::tools::MemoryForgetTool::new(store.clone())));
        // 软件管家：找软件 / 装软件 / 配置系统
        tools.register(Box::new(software::EnvCheckTool));
        tools.register(Box::new(software::SoftwareSearchTool));
        tools.register(Box::new(software::SoftwareInfoTool));
        tools.register(Box::new(software::SoftwareListTool));
        tools.register(Box::new(software::SoftwareLocateTool));
        tools.register(Box::new(software::DiskInfoTool));
        // SoftwareInstallTool 需要 AppHandle（安装时向前端实时推送进度），在 setup 中注册
        // SoftwareUninstallTool 需要 Capability（卸载时轮询点击确认弹窗），在 capability 创建后注册
        tools.register(Box::new(software::SystemGetTool));
        tools.register(Box::new(software::SystemSetTool));
        // 本地知识库 RAG
        let rag_index = Arc::new(rag::RagIndex::new(store.clone()));
        tools.register(Box::new(rag::RagIndexTool::new(rag_index.clone())));
        // RagSearchTool 需要 AppHandle（检索时向前端发事件），在 setup 中注册

        // Computer Use 能力（M2 只读：无障碍树感知 + 窗口枚举 + 截屏）
        let capability = capability::create_capability();
        tools.register(Box::new(capability::ReadScreenTool::new(capability.clone())));
        tools.register(Box::new(capability::ListWindowsTool::new(capability.clone())));
        tools.register(Box::new(capability::ReadWindowTool::new(capability.clone())));
        // 可交互元素地图：GUI 两阶段模式（先分析应用结构 → 一轮批量派发操作）
        tools.register(Box::new(capability::UiAnalyzeTool::new(capability.clone())));
        // GUI 回退：操作日志回退（回退原则第 3 级）
        tools.register(Box::new(capability::GuiUndoTool::new(capability.clone())));
        // 回退原则第 1 级：批量派发前登记预期状态，批尾自动校验正确性
        tools.register(Box::new(agent::ExpectedStateTool));
        tools.register(Box::new(agent::PlanConfirmTool));
        // 万能聊天卡片：模型推送 HTML 卡片在聊天框中精美展示结构化信息（天气/日程等）
        tools.register(Box::new(agent::ChatCardTool));
        // 快捷启动应用：开始菜单索引 + UWP + 等窗返回（GUI 任务提速）
        tools.register(Box::new(tools::LaunchAppTool));
        tools.register(Box::new(tools::ExplorerOpenTool));
        tools.register(Box::new(capability::CaptureScreenTool::new(capability.clone())));
        // 全屏元素标注：UIA 控件 + OCR 文字行一次汇总（批量规划 GUI 步骤用）
        tools.register(Box::new(capability::ScreenElementsTool::new(capability.clone())));
        // 视觉回归对比：基线截图 vs 当前截图 像素级 diff（current 省略时现场截屏）
        tools.register(Box::new(visual_diff::VisualDiffTool::new(capability.clone())));
        tools.register(Box::new(capability::ClickAtTool::new(capability.clone())));
        tools.register(Box::new(capability::TypeTextTool::new(capability.clone())));
        tools.register(Box::new(capability::FindElementTool::new(capability.clone())));
        tools.register(Box::new(capability::ClickElementTool::new(capability.clone())));
        tools.register(Box::new(capability::GroundElementTool::new(capability.clone())));
        // P0「手」：完整鼠标/键盘操作
        tools.register(Box::new(capability::MouseClickTool::new(capability.clone())));
        tools.register(Box::new(capability::MouseDragTool::new(capability.clone())));
        // 滚轮滚动（垂直/水平）、中键、悬停：补齐鼠标操作面
        tools.register(Box::new(capability::WheelScrollTool::new(capability.clone())));
        tools.register(Box::new(capability::MiddleClickTool::new(capability.clone())));
        tools.register(Box::new(capability::HoverTool::new(capability.clone())));
        tools.register(Box::new(capability::KeyPressTool::new(capability.clone())));
        tools.register(Box::new(capability::KeyDownTool::new(capability.clone())));
        tools.register(Box::new(capability::KeyUpTool::new(capability.clone())));
        tools.register(Box::new(capability::PasteTextTool::new(capability.clone())));
        tools.register(Box::new(capability::SaveDialogTool::new(capability.clone())));
        tools.register(Box::new(capability::WaitStableTool::new(capability.clone())));
        // 游戏自动化原语：区域 OCR / 局面缓存增量 diff / 宏序列
        tools.register(Box::new(capability::RegionOcrTool));
        tools.register(Box::new(capability::BoardDiffTool));
        tools.register(Box::new(capability::MacroTool));
        // 窗口控制（防遮挡）：最小化其他窗口 / 置顶 / 聚焦目标窗口
        tools.register(Box::new(capability::WindowMinimizeAllTool::new(capability.clone())));
        tools.register(Box::new(capability::WindowSetTopmostTool::new(capability.clone())));
        tools.register(Box::new(capability::WindowFocusTool::new(capability.clone())));
        // 一键清屏准备：聚焦+置顶+最小化其余+验证（GUI 任务开始时一次调用替代上面组合）
        tools.register(Box::new(capability::WindowPrepareTool::new(capability.clone())));
        // 弹窗处理：自动检测并关闭第三方应用启动时的广告/更新/欢迎/协议弹窗
        tools.register(Box::new(popup::ClosePopupTool::new(capability.clone())));
        // 软件卸载（需要 Capability：卸载过程中轮询点击确认弹窗）
        tools.register(Box::new(software::SoftwareUninstallTool::new(
            capability.clone(),
        )));
        // GUI 操作回放：关键帧留存 + 失败回看定位
        tools.register(Box::new(replay::ReplayKeyframesTool::new()));

        // 多模态交互：图片描述 / 屏幕理解 / 视频分析 / 语音转写（本地优先）
        tools.register(Box::new(multimodal::ImageDescribeTool));
        tools.register(Box::new(multimodal::ScreenUnderstandTool::new(capability.clone())));
        tools.register(Box::new(multimodal::VideoAnalyzeTool));
        tools.register(Box::new(multimodal::SttTranscribeTool));
        tools.register(Box::new(mic::MicRecordTool));

        // 跨应用数据管道 + 可视化：采集 / 清洗 / 聚合 / 导出（可视化与报告需 AppHandle，在 setup 注册）
        tools.register(Box::new(datapipeline::DataIngestTool));
        tools.register(Box::new(datapipeline::DataCleanTool));
        tools.register(Box::new(datapipeline::DataAggregateTool));
        tools.register(Box::new(datapipeline::DataExportTool));

        // P2「时」与「知」：定时任务调度 + 本地 OCR + 凭据 Vault
        let scheduler_state = scheduler::SchedulerState::new(store.clone());
        tools.register(Box::new(scheduler::ScheduleTool::new(scheduler_state.clone())));
        tools.register(Box::new(scheduler::ScheduleListTool::new(scheduler_state.clone())));
        tools.register(Box::new(scheduler::ScheduleCancelTool::new(scheduler_state.clone())));
        tools.register(Box::new(scheduler::ScheduleSetEnabledTool::new(
            scheduler_state.clone(),
            scheduler::SetEnabledKind::Pause,
        )));
        tools.register(Box::new(scheduler::ScheduleSetEnabledTool::new(
            scheduler_state.clone(),
            scheduler::SetEnabledKind::Resume,
        )));
        tools.register(Box::new(scheduler::ScheduleLogsTool::new(scheduler_state.clone())));
        tools.register(Box::new(ocr::OcrImageTool));
        // 日历感知调度：读取本地 Outlook/系统日历（本地优先，无需联网）
        tools.register(Box::new(calendar::CalendarEventsTool));
        // 邮件能力：本地 Outlook 读收件箱 / 发邮件
        tools.register(Box::new(email::ListMailTool));
        tools.register(Box::new(email::SendMailTool));
        tools.register(Box::new(vault::VaultSetTool::new(store.clone())));
        tools.register(Box::new(vault::VaultGetTool::new(store.clone())));
        tools.register(Box::new(vault::VaultListTool::new(store.clone())));
        tools.register(Box::new(vault::VaultDeleteTool::new(store.clone())));
        // P2「体」：剪贴板 + 表格(CSV/Excel) + 全局文件内容搜索
        tools.register(Box::new(clipboard::ClipboardGetTool));
        tools.register(Box::new(clipboard::ClipboardSetTool));
        tools.register(Box::new(clipboard::ClipboardHistoryTool));
        tools.register(Box::new(text_tools::TextTransformTool));
        tools.register(Box::new(spreadsheet::CsvReadTool));
        tools.register(Box::new(spreadsheet::CsvWriteTool));
        tools.register(Box::new(spreadsheet::XlsxReadTool));
        tools.register(Box::new(spreadsheet::XlsxWriteTool));
        tools.register(Box::new(grep::GrepTool));
        tools.register(Box::new(browser::WebSearchTool));

        // 自主看护 Agent：任务注册表 + 触发器轮询 + 行动引擎 + 失败自愈重试（watchdog_run 需 AppHandle，在 setup 注册）
        let watchdog_state = watchdog::WatchdogState::new(store.clone(), tools.clone());
        tools.register(Box::new(watchdog::WatchdogRegisterTool::new(watchdog_state.clone())));
        tools.register(Box::new(watchdog::WatchdogListTool::new(watchdog_state.clone())));
        tools.register(Box::new(watchdog::WatchdogToggleTool::new(
            watchdog_state.clone(),
            watchdog::WatchdogToggle::Pause,
        )));
        tools.register(Box::new(watchdog::WatchdogToggleTool::new(
            watchdog_state.clone(),
            watchdog::WatchdogToggle::Resume,
        )));
        tools.register(Box::new(watchdog::WatchdogDeleteTool::new(watchdog_state.clone())));
        tools.register(Box::new(watchdog::WatchdogLogsTool::new(watchdog_state.clone())));

        // 模型配置：环境变量默认 → 持久化配置覆盖
        let model_config = load_model_config(&store);
        let model = Arc::new(ModelRouter::new(model_config.build_providers()));
        // 视觉/嵌入运行时同步：精确匹配激活项，缺省时回退到第一个可用云端
        if let Some((base_url, api_key, vision_model)) = model_config.vision_conn() {
            visual_grounding::set_vision_cloud(&base_url, &api_key);
            if !vision_model.is_empty() {
                visual_grounding::set_vision_model(vision_model);
            }
        }
        visual_grounding::sync_multimodal_main(&model_config);
        if let Some(em) = model_config.embedding_model() {
            embedding::set_embed_model(em);
        }

        // 任务广场注册表（聚合工具 / 工作流 / 技能 / 自研条目），启动时加载持久化自研工具
        let plaza = Arc::new(plaza::PlazaRegistry::new(
            tools.clone(),
            workflows.clone(),
            store.clone(),
        ));
        plaza.reload_diy_tools();

        // 恢复上次未完成的任务检查点（断点续跑：重启后承接中断的多步骤任务）
        let restored_todos = task::load_task_checkpoint(&store);
        if !restored_todos.is_empty() {
            println!("[检查点] 恢复 {} 个未完成任务步骤", restored_todos.len());
        }

        let im_bus = Arc::new(im::ImBus::new(store.clone()));

        let state = Self {
            tools,
            model,
            security: SecurityManager::new(store.clone()),
            wechat: im_bus.wechat.clone(),
            feishu: im_bus.feishu.clone(),
            im_bus,
            store,
            capability,
            focus: Mutex::new(None),
            browser: Arc::new(Mutex::new(browser::BrowserState::default())),
            markdown: Arc::new(Mutex::new(markdown::MarkdownState::default())),
            terminal: Arc::new(terminal::TerminalState::new()),
            rag: rag_index,
            todos: Arc::new(Mutex::new(restored_todos)),
            thought_log: Arc::new(Mutex::new(Vec::new())),
            cancel: Arc::new(AtomicBool::new(false)),
            escalation: notify::EscalationManager::new(),
            scheduler: scheduler_state,
            workmodes,
            workflows,
            plaza,
            watchdog: watchdog_state,
        };

        // MCP 集成（持久化配置，可运行时重建）
        let mcp_config = load_mcp_config(&state.store);
        let _ = state.apply_mcp_config(&mcp_config);

        println!(
            "[模型] 提供方链路: {}（失败自动切换）",
            model_config.chain_label()
        );
        println!(
            "[工具] 共 {} 个: {}",
            state.tools.names().len(),
            state.tools.names().join(", ")
        );

        state
    }

    /// 记录一条思考流到本轮执行流日志（供事后固化为 trace 回看）。
    /// 注意：这里只记录，不负责发事件；调用方仍各自 emit "thought" 事件。
    pub fn log_thought(&self, kind: &str, label: &str, detail: &str) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let mut log = self.thought_log.lock().unwrap();
        log.push(serde_json::json!({
            "ts": ts,
            "kind": kind,
            "label": label,
            "detail": detail,
        }));
    }

    /// 记录一条完整结构的执行流事件到日志（供固化为 trace 回看）。
    /// 与 log_thought 的区别：不裁剪字段，可携带 progress/phase/vendor/version/homepage 等扩展字段，
    /// 用于安装进度这类需在「安装完成后仍可回看」的事件。
    pub fn log_thought_full(&self, value: serde_json::Value) {
        self.thought_log.lock().unwrap().push(value);
    }

    /// 清空本轮思考日志（chat 开始时调用）
    pub fn clear_thought_log(&self) {
        self.thought_log.lock().unwrap().clear();
    }

    /// 应用 MCP 配置：移除旧 MCP 工具 → 按需连接新服务器并注册
    pub fn apply_mcp_config(&self, config: &mcp::McpConfig) -> Result<usize, String> {
        self.tools.remove_ns("mcp");
        if !config.enabled {
            println!("[MCP] 已禁用");
            return Ok(0);
        }
        let client = mcp::McpClient::connect(&config.command, &config.args)?;
        let count = client.tools().len();
        for adapter in client.into_adapters() {
            self.tools.register_ns("mcp", Box::new(adapter));
        }
        println!("[MCP] 已连接 {}，注册 {} 个工具", config.command, count);
        Ok(count)
    }
}

/// 加载模型配置：环境变量默认 → 持久化配置覆盖
pub(crate) fn load_model_config(store: &MemoryStore) -> ModelConfig {
    let mut config = ModelConfig::from_env();
    if let Ok(Some(json)) = store.get_setting("model_config") {
        if let Ok(saved) = serde_json::from_str::<ModelConfig>(&json) {
            config = saved;
        }
    }
    hydrate_model_keys(store, &mut config);
    config
}

/// 模型 API Key 的 vault key（按 profile.id 隔离）
fn model_key_vault_key(id: &str) -> String {
    format!("model_api_key:{id}")
}

/// 从 vault 还原各 profile 的 API Key（明文仅驻留内存，供 provider / cloud_conn 使用）
pub(crate) fn hydrate_model_keys(store: &MemoryStore, config: &mut ModelConfig) {
    for p in config.profiles.iter_mut() {
        if !p.api_key.trim().is_empty() {
            p.has_key = true;
            continue;
        }
        let key = model_key_vault_key(&p.id);
        if let Ok(Some(cipher)) = store.vault_get(&key) {
            if let Ok(plain) = crate::vault::open(&key, &cipher) {
                if !plain.trim().is_empty() {
                    p.api_key = plain;
                    p.has_key = true;
                }
            }
        }
    }
}

/// 将各 profile 的 API Key 抽离到 vault 加密存储，并清空内存/序列化副本中的明文。
/// 规则：api_key 非空 → 加密写入并覆盖；api_key 空且有 has_key → 保留原有 key；
///        api_key 空且无 has_key → 删除（用户显式清除）。
pub(crate) fn seal_model_keys(store: &MemoryStore, config: &mut ModelConfig) -> Result<(), String> {
    for p in config.profiles.iter_mut() {
        let key = model_key_vault_key(&p.id);
        if !p.api_key.trim().is_empty() {
            let cipher = crate::vault::seal(&key, p.api_key.trim())?;
            store.vault_set(&key, &cipher)?;
            p.api_key = String::new();
            p.has_key = true;
        } else if p.has_key {
            // 保留已有 key（前端留空表示不修改）
        } else {
            let _ = store.vault_delete(&key);
            p.has_key = false;
        }
    }
    // 旧的单云端明文字段若仍存在，一并清空，避免脱敏失效
    if !config.cloud_api_key.is_empty() {
        config.cloud_api_key = String::new();
    }
    Ok(())
}

/// 持久化模型配置（先脱敏加密 API Key，再序列化落库）
pub(crate) fn persist_model_config(
    store: &MemoryStore,
    config: &mut ModelConfig,
) -> Result<(), String> {
    seal_model_keys(store, config)?;
    let json = serde_json::to_string(&*config).map_err(|e| e.to_string())?;
    store.set_setting("model_config", &json)
}

/// 返回给前端的脱敏副本：清空明文 api_key，仅保留 has_key 标记
pub(crate) fn mask_model_keys(config: &mut ModelConfig) {
    for p in config.profiles.iter_mut() {
        if !p.api_key.is_empty() {
            p.has_key = true;
        }
        p.api_key = String::new();
    }
    if !config.cloud_api_key.is_empty() {
        config.cloud_api_key = String::new();
    }
}

/// 加载 MCP 配置：环境变量默认 → 持久化配置覆盖
pub(crate) fn load_mcp_config(store: &MemoryStore) -> mcp::McpConfig {
    let mut config = mcp::McpConfig::from_env();
    if let Ok(Some(json)) = store.get_setting("mcp_config") {
        if let Ok(saved) = serde_json::from_str::<mcp::McpConfig>(&json) {
            config = saved;
        }
    }
    config
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 全局 panic 钩子：任何线程 panic 都落盘 exe 同目录 baize-crash.log（附时间与线程名），
    // GUI 场景控制台不可见，此前崩溃无现场可查——这是「横幅出现就崩」类问题的取证通道
    std::panic::set_hook(Box::new(|info| {
        let thread = std::thread::current();
        let line = format!(
            "[崩溃] {} | 线程: {}",
            info,
            thread.name().unwrap_or("<unnamed>")
        );
        eprintln!("{line}");
        crate::windows::diag_log(&line);
    }));

    tauri::Builder::default()
        // 单实例保护（必须最先注册）：双开白泽时旧实例的孤儿清理会误杀新实例的
        // 受控 Chrome 进程（profile 目录互踩），第二个实例启动时聚焦已有窗口并退出
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            use tauri::Manager;
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .manage(AppState::new())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    use tauri_plugin_global_shortcut::ShortcutState;
                    if event.state == ShortcutState::Pressed {
                        if let Some(w) = app.get_webview_window("main") {
                            if w.is_visible().unwrap_or(false) {
                                let _ = w.hide();
                            } else {
                                let _ = w.show();
                                let _ = w.set_focus();
                            }
                        }
                    }
                })
                .build(),
        )
        .setup(|app| {
            // 注册全局快捷键（Alt+Space 呼出/隐藏主窗口）
            {
                use tauri_plugin_global_shortcut::GlobalShortcutExt;
                if let Err(e) = app.global_shortcut().register("alt+space") {
                    eprintln!("[快捷键] 注册 Alt+Space 失败: {e}");
                } else {
                    println!("[快捷键] 已注册 Alt+Space（呼出/隐藏主窗口）");
                }
            }

            // 初始化屏幕接管：常驻低层键鼠钩子（GUI 任务阻断外界输入，Ctrl+Shift+F12 紧急解除）
            takeover::init(app.handle().clone());
            panel::init_handle(app.handle().clone());

            // 兜底：主窗口以 visible=false 创建，正常时由前端首帧上屏后显示；
            // 若前端 10 秒内未就绪（JS 异常/资源损坏），强制显示窗口避免永远黑屏
            {
                let handle = app.handle().clone();
        capability::init_capability_app(handle.clone());
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(10));
                    if let Some(w) = handle.get_webview_window("main") {
                        if !w.is_visible().unwrap_or(true) {
                            let _ = w.show();
                            let _ = w.set_focus();
                            println!("[启动] 前端超时未就绪，已强制显示主窗口");
                        }
                    }
                });
            }

            // 系统托盘：常驻后台，可随时显示/隐藏/退出
            {
                let show_i = MenuItem::with_id(app, "show", "显示白泽", true, None::<&str>)?;
                let hide_i = MenuItem::with_id(app, "hide", "隐藏白泽", true, None::<&str>)?;
                let quit_i = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
                let menu = Menu::with_items(app, &[&show_i, &hide_i, &quit_i])?;

                let mut tray = TrayIconBuilder::new().menu(&menu);
                if let Some(icon) = app.default_window_icon() {
                    tray = tray.icon(icon.clone());
                }
                tray.on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    "hide" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.hide();
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;
                println!("[托盘] 已创建系统托盘");
            }

            // 主窗口关闭时隐藏到托盘（常驻后台），而非退出应用
            if let Some(main) = app.get_webview_window("main") {
                // 初始尺寸自适应屏幕：配置的 1480×900 超过屏幕时按 92% 工作区裁剪，避免小屏溢出；并居中
                if let Ok(Some(monitor)) = main.current_monitor() {
                    let scale = monitor.scale_factor();
                    let msize = monitor.size().to_logical::<f64>(scale);
                    let mw = msize.width * 0.92;
                    let mh = msize.height * 0.92;
                    let cur = main.inner_size().unwrap_or_default().to_logical::<f64>(scale);
                    if cur.width > mw || cur.height > mh {
                        let _ = main.set_size(tauri::LogicalSize::new(cur.width.min(mw), cur.height.min(mh)));
                    }
                    let _ = main.center();
                }
                let w = main.clone();
                main.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = w.hide();
                    }
                });
            }

            // 注册浏览器 / 文档工具（需要 AppHandle 向对应窗口推送事件）
            {
                let handle = app.handle().clone();
                let state = app.state::<AppState>();
                state.tools.register(Box::new(browser::BrowserNavigateTool::new(
                    handle.clone(),
                    state.browser.clone(),
                )));
                state.tools.register(Box::new(browser::BrowserSearchTool::new(
                    handle.clone(),
                    state.browser.clone(),
                )));
                state.tools.register(Box::new(browser::BrowserRenderHtmlTool::new(
                    handle.clone(),
                    state.browser.clone(),
                )));
                state
                    .tools
                    .register(Box::new(browser::BrowserGetTool::new(state.browser.clone())));
                state.tools.register(Box::new(browser::BrowserReadTool::new(
                    handle.clone(),
                    state.browser.clone(),
                )));
                // 浏览器标签页工具
                state.tools.register(Box::new(browser::BrowserOpenTool::new(
                    handle.clone(),
                    state.browser.clone(),
                )));
                state.tools.register(Box::new(browser::BrowserCloseTool::new(
                    handle.clone(),
                    state.browser.clone(),
                )));
                state.tools.register(Box::new(browser::BrowserCloseAllTool::new(
                    handle.clone(),
                    state.browser.clone(),
                )));
                state
                    .tools
                    .register(Box::new(browser::BrowserTabsTool::new(state.browser.clone())));
                state.tools.register(Box::new(browser::BrowserSwitchTool::new(
                    handle.clone(),
                    state.browser.clone(),
                )));
                // 浏览器交互自动化（headless Chrome，保持登录态）
                state.tools.register(Box::new(browser::BrowserActTool));
                state.tools.register(Box::new(markdown::MarkdownSetTool::new(
                    handle.clone(),
                    state.markdown.clone(),
                )));
                state.tools.register(Box::new(markdown::MarkdownAppendTool::new(
                    handle.clone(),
                    state.markdown.clone(),
                )));
                state
                    .tools
                    .register(Box::new(markdown::MarkdownGetTool::new(state.markdown.clone())));
                state
                    .tools
                    .register(Box::new(task::TodoUpdateTool::new(
                        handle.clone(),
                        state.todos.clone(),
                        state.store.clone(),
                    )));
                state
                    .tools
                    .register(Box::new(windows::ResizeWindowTool::new(handle.clone())));
                // 桌面步骤弹幕浮窗：GUI 自动化时最小化主窗口 + 桌面滚动展示进度
                state
                    .tools
                    .register(Box::new(windows::ShowStepTool::new(handle.clone())));
                state
                    .tools
                    .register(Box::new(windows::HideStepTool::new(handle.clone())));
                // 屏幕接管 / 解除：GUI 任务阻断外部输入 + 紧急解除
                state
                    .tools
                    .register(Box::new(takeover::ScreenTakeoverTool::new(handle.clone())));
                state
                    .tools
                    .register(Box::new(takeover::ScreenReleaseTool::new(handle.clone())));
                // 内置终端：打开窗口 / 发送命令执行
                state
                    .tools
                    .register(Box::new(terminal::OpenTerminalTool::new(
                        handle.clone(),
                        state.terminal.clone(),
                    )));
                state
                    .tools
                    .register(Box::new(terminal::TerminalSendTool::new(
                        handle.clone(),
                        state.terminal.clone(),
                    )));
                state
                    .tools
                    .register(Box::new(plugin::PluginLoadTool::new(state.tools.clone())));
                state
                    .tools
                    .register(Box::new(skill::SkillRunTool::new(
                        handle.clone(),
                        Arc::new(skill::SkillLibrary::new(state.store.clone())),
                        state.tools.clone(),
                        state.todos.clone(),
                    )));
                // 跨应用数据管道：可视化（内置浏览器渲染 ECharts）+ 报告（文档窗口）
                state
                    .tools
                    .register(Box::new(datapipeline::DataVizTool::new(
                        handle.clone(),
                        state.browser.clone(),
                    )));
                state
                    .tools
                    .register(Box::new(datapipeline::DataReportTool::new(
                        handle.clone(),
                        state.markdown.clone(),
                    )));
                // 自主看护 Agent：按 id 手动触发一次任务
                state
                    .tools
                    .register(Box::new(watchdog::WatchdogRunTool::new(
                        handle.clone(),
                        state.watchdog.clone(),
                    )));
                // 通知升级工具
                state
                    .tools
                    .register(Box::new(notify::NotifyUserTool::new(
                        handle.clone(),
                        state.escalation.clone(),
                    )));
                // TTS 语音播报（桌面助手开口说话）
                state
                    .tools
                    .register(Box::new(notify::SpeakTool::new(handle.clone())));
                // 微信图片发送：任务完成自动截图 / 显式指令发送，支持当前屏幕截图与本地图片回图
                state
                    .tools
                    .register(Box::new(wechat::WeChatSendImageTool::new(handle.clone())));
                // 子代理工具
                state
                    .tools
                    .register(Box::new(subagent::SpawnSubAgentTool::new(
                        state.model.clone(),
                        state.tools.clone(),
                        subagent::SubAgentTrace::enabled(handle.clone()),
                    )));
                // ACUI 受控卡片
                state
                    .tools
                    .register(Box::new(acui::RenderCardTool::new(handle.clone())));
                // 定时提醒
                state
                    .tools
                    .register(Box::new(scheduler::ReminderTool::new(handle.clone())));
                // 知识库检索（需要 AppHandle 发检索结果事件）
                state
                    .tools
                    .register(Box::new(rag::RagSearchTool::new(
                        handle.clone(),
                        state.rag.clone(),
                    )));
                // 软件安装（需要 AppHandle 向前端实时推送安装进度 + Capability 轮询点安装弹窗）
                state
                    .tools
                    .register(Box::new(software::SoftwareInstallTool::new(
                        handle.clone(),
                        state.capability.clone(),
                    )));
                // 测试工程师：需求 → 测试用例 管线工具
                state
                    .tools
                    .register(Box::new(test_engineer::GenerateTestCasesTool::new(handle.clone())));
                // 测试工程师：UI/接口 自动化测试执行器 + 断言
                state
                    .tools
                    .register(Box::new(test_engineer::AssertUITool::new(state.capability.clone())));
                state.tools.register(Box::new(test_engineer::RunUiTestTool::new(
                    state.capability.clone(),
                    handle.clone(),
                )));
                state
                    .tools
                    .register(Box::new(test_engineer::RunApiTestTool::new(handle.clone())));
                // 可编排工作流：按 id 执行多阶段流水线
                state
                    .tools
                    .register(Box::new(workflow::RunWorkflowTool::new(handle.clone())));
                // 文本 AI 增强：摘要/翻译/润色/纠错（本地模型优先）
                state
                    .tools
                    .register(Box::new(text_tools::TextAiTool::new(handle.clone())));
                // 工具自研：author_tool（模型按工种自行注册命令工具）
                state.tools.register(Box::new(workmode::AuthorTool::new(
                    state.tools.clone(),
                    state.workmodes.clone(),
                )));
            }

            // 注意：浏览器 / 文档窗口改为「按需调出」——
            // 仅在白泽调用 browser_* / markdown_* 工具时由工具内部 ensure 创建，
            // 启动时不再预创建，避免挤压主窗口。

            // 后台启动主动唤醒（文件监听 → 主动推卡片）
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                if let Err(e) = scheduler::run(handle) {
                    eprintln!("[主动唤醒] 停止: {e}");
                }
            });

            // 后台启动定时任务调度循环（cron）
            let schedule_state = app.state::<AppState>().scheduler.clone();
            scheduler::run_schedule(schedule_state, app.handle().clone());

            // 后台启动剪贴板监听（记录外部复制，实时推送历史变更）
            clipboard::start_monitor(app.handle().clone());

            // 后台启动自主看护 Agent 轮询循环（cron/interval/fs/process/threshold 触发器 → 行动引擎）
            let watchdog_arc = app.state::<AppState>().watchdog.clone();
            watchdog::run_watchdog(watchdog_arc, app.handle().clone());

            // 后台启动主动心跳（长期未互动 → 主动问候，每日限流）
            proactive::run_heartbeat(app.handle().clone());

            // 统一心跳中心：聚合子系统打点 → baize:vital 广播（银河背景星光随心跳明灭）
            heartbeat::init(app.handle().clone());

            // 后台记忆治理守护：每日一次 去重合并 + 衰减清理（星图更干净）
            {
                let store = app.state::<AppState>().store.clone();
                std::thread::spawn(move || loop {
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0);
                    let stale = store
                        .get_setting("memory_gov_last")
                        .ok()
                        .flatten()
                        .and_then(|v| v.parse::<i64>().ok())
                        .map(|t| now_ms - t > 24 * 3600 * 1000)
                        .unwrap_or(true);
                    if stale {
                        match store.consolidate_memories() {
                            Ok((merged, decayed)) => {
                                if merged + decayed > 0 {
                                    println!("[记忆治理] 合并重复 {merged} 条，衰减清理 {decayed} 条");
                                }
                                let _ = store.set_setting("memory_gov_last", &now_ms.to_string());
                            }
                            Err(e) => eprintln!("[记忆治理] 失败: {e}"),
                        }
                    }
                    // 自维护：每 6 小时自动检测并清理（审计日志裁剪、任务截屏残留、WAL 压缩）
                    if let Some(summary) = crate::maintenance::tick(&store, 6 * 3600 * 1000) {
                        println!("[自维护] {summary}");
                    }
                    std::thread::sleep(std::time::Duration::from_secs(3600));
                });
            }

            // 后台自动连接 IM 通道（微信 / 飞书，若已登录则恢复长连接接收指令）
            app.state::<AppState>().im_bus.start_all(app.handle());
        // 微信回图工具（需 AppHandle，setup 时补注册）：此前从未注册，导致模型
        // 只能把图片保存路径写进回复文本，微信端收不到真实图片
        {
            let state = app.state::<AppState>();
            state
                .tools
                .register(Box::new(wechat::WeChatSendImageTool::new(app.handle().clone())));
        }

            // 自测：设置 BAIZE_TEST_TASK 环境变量后，启动时自动跑一次任务并截图（用于联调验证）
            if let Ok(test_msg) = std::env::var("BAIZE_TEST_TASK") {
                if !test_msg.is_empty() {
                    let handle = app.handle().clone();
                    tauri::async_runtime::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_secs(8)).await;
                        let state = handle.state::<AppState>();
                        let capability = state.inner().capability.clone();
                        let result = crate::agent::Supervisor::new(&handle, state.inner())
                            .run(&test_msg, vec![])
                            .await;
                        match &result {
                            Ok(s) => println!(
                                "[测试任务] 成功: {}",
                                s.chars().take(300).collect::<String>()
                            ),
                            Err(e) => println!("[测试任务] 失败: {e}"),
                        }
                        if let Ok(info) = capability.capture_screen() {
                            println!("[测试任务] 截图: {}", info.path);
                        }
                    });
                }
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::chat,
            commands::stop_chat,
            commands::list_conversations,
            commands::create_conversation,
            commands::delete_conversation,
            commands::list_projects,
            commands::add_project,
            commands::delete_project,
            commands::set_conversation_project,
            commands::get_messages,
            commands::save_compare_result,
            commands::export_conversation,
            commands::visual_diff,
            commands::list_files,
            commands::read_file,
            commands::read_document,
            commands::check_document_deps,
            commands::get_pending_permissions,
            commands::resolve_permission,
            commands::get_model_config,
            commands::set_model_config,
            commands::set_active_model,
            commands::get_vendor_presets,
            commands::test_model_profile,
            commands::compare_models,
            commands::run_meeting,
            commands::meeting_interrupt,
            commands::meeting_stop,
            commands::summarize_meeting,
            commands::run_teamwork,
            commands::get_token_saver_config,
            commands::set_token_saver_config,
            commands::detect_image_model,
            commands::generate_image,
            commands::get_mcp_config,
            commands::set_mcp_config,
            commands::get_memories,
            commands::get_memory_graph,
            commands::memory_governance,
            commands::get_memory_overview,
            commands::forget_memory,
            commands::list_memories_panel,
            commands::delete_memory_by_id,
            commands::pin_memory,
            commands::get_rag_state,
            commands::index_rag_dir,
            commands::clear_rag,
            commands::search_rag,
            commands::get_db_connections,
            commands::save_db_connections,
            commands::get_browser_state,
            commands::switch_browser_tab,
            commands::close_browser_tab,
            commands::browser_act,
            commands::browser_get_path,
            commands::browser_set_path,
            commands::preview_html,
            commands::get_markdown_state,
            commands::switch_markdown_tab,
            commands::close_markdown_tab,
            commands::save_markdown,
            commands::get_voice,
            commands::set_voice,
            tts::get_tts_config,
            tts::set_tts_config,
            tts::tts_synthesize,
            tts::get_kokoro_voices,
            commands::get_runtime_config,
            commands::set_runtime_config,
            commands::get_notify_config,
            commands::set_notify_config,
            commands::get_work_modes,
            commands::get_work_mode,
            commands::set_work_mode,
            commands::test_generate_cases,
            commands::test_export_cases,
            commands::test_run_ui,
            commands::test_run_api,
            commands::test_run_selected,
            commands::test_load_projects,
            commands::test_save_project,
            commands::test_delete_project,
            commands::test_auto_detect_project,
            commands::test_prepare_env,
            commands::test_import_openapi,
            commands::test_list_records,
            commands::test_trend_get,
            commands::pick_files,
            commands::pick_folder,
            commands::open_path,
            commands::toggle_float_orb,
            commands::set_workspace,
            commands::env_check,
            commands::software_search,
            commands::software_list,
            commands::system_get,
            commands::disk_info,
            commands::schedule_list_jobs,
            commands::schedule_add_job,
            commands::schedule_update_job,
            commands::schedule_delete_job,
            commands::schedule_set_enabled,
            commands::schedule_job_logs,
            commands::schedule_clear_logs,
            workflow::list_workflows,
            workflow::add_workflow,
            workflow::run_workflow,
            workflow::workflow_delete,
            workflow::workflow_runs,
            workflow::workflow_clear_runs,
            clipboard::clipboard_get_text,
            clipboard::clipboard_set_text,
            clipboard::clipboard_history,
            clipboard::clipboard_history_clear,
            text_tools::text_transform,
            text_tools::text_ai,
            background::submit_task,
            background::list_tasks,
            background::get_task,
            background::cancel_task,
            commands::open_terminal_window,
            commands::open_terminal_with_command,
            commands::term_spawn,
            commands::term_write,
            commands::term_resize,
            commands::term_close,
            plaza::plaza_list,
            plaza::plaza_save_item,
            plaza::plaza_delete_item,
            plaza::plaza_run,
            plaza::plaza_market_catalog,
            plaza::plaza_market_install,
            windows::get_step_log,
            windows::halo_get_last,
            windows::frontend_log,
            updater::update_check,
            updater::update_install,
            wechat::wechat_get_status,
            wechat::wechat_login,
            wechat::wechat_start,
            wechat::wechat_stop,
            wechat::wechat_logout,
            feishu::feishu_get_status,
            feishu::feishu_save_credentials,
            feishu::feishu_start,
            feishu::feishu_stop,
            im::im_list,
            im::im_log,
        ])
        .run(tauri::generate_context!())
        .expect("error while running BaiZe");

    // 应用退出：显式关闭受控 Chrome（static 持有的实例不会随进程退出被 drop，
    // 不处理的话每次关闭应用都会留下一组孤儿 Chrome）
    crate::browser::shutdown_browser();
}
