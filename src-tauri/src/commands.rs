use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::agent::Supervisor;
use crate::mcp::McpConfig;
use crate::memory::RememberOutcome;
use crate::model::{ChatMessage, ModelConfig, ModelProvider, ModelTier};
use crate::security::{PermissionDecision, PermissionRequest};
use crate::tools::Tool;
use crate::AppState;

/// 主对话命令：持久化消息（工作记忆）+ 委托给 Agent 循环
#[tauri::command]
pub async fn chat(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    conv_id: String,
    message: String,
    history: Vec<ChatMessage>,
    attachments: Vec<String>,
) -> Result<String, String> {
    state.cancel.store(false, Ordering::SeqCst);
    crate::tools::clear_global_cancel();
    // 清空本轮思考日志，开始累积执行流
    state.clear_thought_log();

    // 首条用户消息即会话话题：折叠空白取前 20 字；会话是前端预建的「新会话」默认标题时改名，
    // 已被命名过的会话保持不变（即始终以「第一次对话用户发起的话题」命名）
    let topic: String = message
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(20)
        .collect();
    state.store.ensure_conversation(&conv_id, &topic, None)?;
    if !topic.is_empty() {
        state.store.rename_conversation_if_default(&conv_id, &topic)?;
    }
    let attachments_json = serde_json::to_string(&attachments).ok();
    state.store.add_message(&conv_id, "user", &message, attachments_json.as_deref())?;

    // 会话归属的项目：注入模型上下文（白泽由此「知道」当前项目），并在思考流中展示
    let project = state.store.project_of_conversation(&conv_id).ok().flatten();
    if let Some(p) = &project {
        let label = format!("项目上下文 · {}", p.name);
        let detail = if p.path.is_empty() {
            "未绑定工作目录".to_string()
        } else {
            p.path.clone()
        };
        let _ = app.emit("thought", json!({ "kind": "project", "label": label, "detail": detail }));
        state.log_thought("project", &label, &detail);
    }

    // 面板正则触发：消息命中面板关键词时直接打开对应界面（即时生效、零 token；LLM 自主决策仍兜底复杂意图）
    if let Some(pid) = crate::panel::detect_intent(&app, &message) {
        let label = "正则触发 · 打开面板";
        let detail = format!("消息命中面板「{pid}」关键词，已自动打开");
        let _ = app.emit("thought", json!({ "kind": "phase", "label": label, "detail": detail }));
        state.log_thought("phase", label, &detail);
    }

    // 附件文档：抽取文本 + 脱敏，注入为本轮上下文（不写入消息/记忆）
    let agent_input = enrich_with_attachments(&message, &attachments);

    // Token 节约：长对话超阈值时压缩早期消息为摘要（本地免费模型压缩）
    let (history, compress_stats) = crate::token_saver::compress_history(&state, history).await;
    if let Some(s) = compress_stats {
        let _ = app.emit(
            "thought",
            json!({
                "kind": "thinking",
                "label": "上下文压缩 · 节约 Token",
                "detail": format!(
                    "{} 字 → {} 字，本次少发送约 {} 字（本地压缩，不产生云端费用）",
                    s.before_chars, s.after_chars, s.saved()
                ),
            }),
        );
        state.log_thought(
            "thinking",
            "上下文压缩 · 节约 Token",
            &format!("{} 字 → {} 字，少发送约 {} 字", s.before_chars, s.after_chars, s.saved()),
        );
    }

    let answer = Supervisor::new(&app, &state)
        .with_project(project)
        .run(&agent_input, history)
        .await?;

    // 停止后跳过记忆/固化等后续处理（避免再触发模型请求）
    if !state.cancel.load(Ordering::SeqCst) {
        // 智能记忆（过滤噪音 + 同话题合并 + 自动衰减）
        let outcome = state
            .store
            .smart_remember(&message, "episodic")
            .unwrap_or(RememberOutcome::Filtered);
        let tag = match outcome {
            RememberOutcome::Created => "新建",
            RememberOutcome::Reinforced => "强化",
            RememberOutcome::Filtered => "过滤",
        };
        println!("[记忆] {tag}: {message}");

        // 焦点栈：话题切换检测（n-gram 相似度低于阈值视为切换）
        {
            let mut focus = state.focus.lock().unwrap();
            let switched = match focus.as_deref() {
                None => true,
                Some(prev) => crate::memory::ngram_overlap(prev, &message) < 2,
            };
            if switched && outcome != RememberOutcome::Filtered {
                let _ = app.emit(
                    "thought",
                    json!({ "kind": "focus", "label": "话题切换", "detail": message.clone() }),
                );
                state.log_thought("focus", "话题切换", &message);
                *focus = Some(message.clone());
            }
        }

        // 语义固化：模型抽取事实 → 语义记忆（噪音消息跳过）
        // 改为后台异步执行，不阻塞本轮返回
        if outcome != RememberOutcome::Filtered {
            let handle = app.clone();
            let msg = message.clone();
            tauri::async_runtime::spawn(async move {
                let state = handle.state::<AppState>();
                consolidate(state.inner(), &msg).await;
            });
        }

        // 记忆↔执行闭环：探测「X 分钟后/小时后」并自动登记一次性提醒 + 沉淀情景事件
        if let Some((delay_secs, label)) = detect_reminder_delay(&message) {
            let handle = app.clone();
            let body = message.clone();
            let action_label = label.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
                let _ = handle.emit(
                    "proactive",
                    json!({
                        "id": uuid::Uuid::new_v4().to_string(),
                        "title": "⏰ 记忆提醒",
                        "body": body,
                        "files": [],
                        "action": action_label,
                    }),
                );
            });
            let _ = state.store.record_event("reminder", &label, &message, 0.7);
            state.log_thought("remember", "自动登记提醒", &label);
        }

        // 告知前端本轮实际使用的模型（本地/云端）
        let provider = state.model.last_used();
        let _ = app.emit(
            "thought",
            json!({ "kind": "model", "label": format!("模型 · {provider}"), "detail": "" }),
        );
        state.log_thought("model", &format!("模型 · {provider}"), "");

        // 降级透出：激活模型调用失败时路由器会静默切到链上后续提供方，
        // 这里把「谁失败、为什么」写进执行流，避免用户疑惑「配了 A 却是 B 在跑」
        if let Some(note) = state.model.take_fallback_note() {
            let _ = app.emit(
                "thought",
                json!({ "kind": "model_fallback", "label": "⚠ 模型降级", "detail": note }),
            );
            state.log_thought("model_fallback", "⚠ 模型降级", &note);
        }
    }

    state.store.add_message(&conv_id, "assistant", &answer, None)?;

    // 固化执行流（thoughts + todos）到本条 assistant 消息，任务结束后仍可展开回看
    {
        let thoughts = state.thought_log.lock().unwrap().clone();
        let todos = state.todos.lock().unwrap().clone();
        let trace = json!({ "thoughts": thoughts, "todos": todos });
        if let Ok(t) = serde_json::to_string(&trace) {
            let _ = state.store.attach_trace(&conv_id, &t);
        }
    }

    // 主动意识：对话结束后异步整理（记忆衰减 + 检查未完成任务续跑提醒）
    crate::proactive::on_chat_idle(app.clone(), state.todos.clone(), state.store.clone());

    Ok(answer)
}

/// 停止当前正在进行的对话（置位取消标志，Agent 循环在下一个检查点返回）
#[tauri::command]
pub fn stop_chat(state: State<'_, AppState>) -> bool {
    state.cancel.store(true, Ordering::SeqCst);
    // 同步置位全局工具取消标志：ps_exec/run_shell/docker 子进程轮询感知后自行 kill
    crate::tools::request_global_cancel();
    true
}

// ---------------- 多会话管理 ----------------

#[tauri::command]
pub fn list_conversations(state: State<'_, AppState>) -> Vec<crate::memory::ConversationRow> {
    state.store.list_conversations().unwrap_or_default()
}

#[tauri::command]
pub fn create_conversation(
    state: State<'_, AppState>,
    title: String,
    project_id: Option<String>,
) -> Result<crate::memory::ConversationRow, String> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    state
        .store
        .ensure_conversation(&id, &title, project_id.as_deref())?;
    Ok(crate::memory::ConversationRow {
        id,
        title,
        project_id,
        created_at: now,
    })
}

#[tauri::command]
pub fn delete_conversation(state: State<'_, AppState>, id: String) -> bool {
    state.store.delete_conversation(&id).is_ok()
}

// ---------------- 项目（侧边栏「项目」导航） ----------------

#[tauri::command]
pub fn list_projects(state: State<'_, AppState>) -> Vec<crate::memory::ProjectRow> {
    state.store.list_projects().unwrap_or_default()
}

/// 新建项目：名称默认取工作目录文件夹名；返回创建后的完整项目列表
#[tauri::command]
pub fn add_project(
    state: State<'_, AppState>,
    name: String,
    path: String,
) -> Result<Vec<crate::memory::ProjectRow>, String> {
    let id = uuid::Uuid::new_v4().to_string();
    state.store.ensure_project(&id, &name, &path)?;
    state.store.list_projects()
}

/// 删除项目：其会话自动回到「未分组」，消息保留
#[tauri::command]
pub fn delete_project(state: State<'_, AppState>, id: String) -> bool {
    state.store.delete_project(&id).is_ok()
}

/// 把会话归入项目 / 移出项目（project_id 传 null）
#[tauri::command]
pub fn set_conversation_project(
    state: State<'_, AppState>,
    conv_id: String,
    project_id: Option<String>,
) -> bool {
    state
        .store
        .set_conversation_project(&conv_id, project_id.as_deref())
        .is_ok()
}

#[tauri::command]
pub fn get_messages(state: State<'_, AppState>, conv_id: String) -> Vec<crate::memory::MessageRow> {
    state.store.messages(&conv_id, 200).unwrap_or_default()
}

/// 把「多模型对比」结果写入会话（assistant 消息，分支存 trace.branches），重启后仍可回看
#[tauri::command]
pub fn save_compare_result(
    state: State<'_, AppState>,
    conv_id: String,
    branches: Value,
) -> bool {
    match serde_json::to_string(&json!({ "branches": branches })) {
        Ok(trace) => state.store.add_compare_message(&conv_id, &trace).is_ok(),
        Err(_) => false,
    }
}

// ---------------- 对话导出（Markdown / JSON） ----------------

/// 导出会话：弹出「另存为」对话框，按所选扩展名（.md / .json）生成对应格式并写入文件。
/// 返回保存路径；用户取消返回 None。
#[tauri::command]
pub async fn export_conversation(state: State<'_, AppState>, conv_id: String) -> Result<Option<String>, String> {
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || -> Result<Option<String>, String> {
        let title = store
            .list_conversations()
            .ok()
            .and_then(|l| l.into_iter().find(|c| c.id == conv_id.clone()).map(|c| c.title))
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| "白泽对话".to_string());

        let path = rfd::FileDialog::new()
            .set_file_name(&format!("{title}.md"))
            .add_filter("Markdown", &["md", "markdown"])
            .add_filter("JSON", &["json"])
            .save_file();

        let Some(p) = path else { return Ok(None) };

        // 依据保存时选择的扩展名决定导出格式（.json → JSON，否则 Markdown）
        let ext = p
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let is_json = ext == "json";

        let msgs = store.messages(&conv_id, 200).unwrap_or_default();
        let content = if is_json {
            build_export_json(&title, &msgs)?
        } else {
            build_export_markdown(&title, &msgs)
        };

        std::fs::write(&p, &content).map_err(|e| format!("导出失败：{e}"))?;
        Ok(Some(p.to_string_lossy().to_string()))
    })
    .await
    .map_err(|e| format!("导出失败: {e}"))?
}

/// 生成 Markdown 导出文本
fn build_export_markdown(title: &str, msgs: &[crate::memory::MessageRow]) -> String {
    let mut out = format!("# {title}\n\n");
    for m in msgs {
        let who = if m.role == "user" { "用户" } else { "白泽" };
        out.push_str(&format!("## {who}\n\n{}\n\n", m.content));
    }
    out
}

/// 生成 JSON 导出文本（含标题 + 消息数组）
fn build_export_json(title: &str, msgs: &[crate::memory::MessageRow]) -> Result<String, String> {
    let arr: Vec<Value> = msgs
        .iter()
        .map(|m| {
            json!({
                "role": m.role,
                "content": m.content,
                "created_at": m.created_at,
            })
        })
        .collect();
    serde_json::to_string_pretty(&json!({ "title": title, "messages": arr }))
        .map_err(|e| e.to_string())
}

// ---------------- 供前端直接调用的调试/演示命令 ----------------

#[tauri::command]
pub fn list_files(path: String) -> Result<Value, String> {
    crate::tools::FileListTool.run(json!({ "path": path }))
}

#[tauri::command]
pub fn read_file(path: String) -> Result<Value, String> {
    crate::tools::FileReadTool.run(json!({ "path": path }))
}

/// 办公文档解析命令（PDF/Word/Excel/PPT/CSV/TXT/MD）：富解析正文/表格/图片，可导出 CSV。
/// 底层会启动 Python（pdfplumber/python-docx 等）做富文本/表格/图片抽取，属于阻塞子进程；
/// 改为 async + spawn_blocking 避免阻塞 Tauri 主线程（与软件管家、浏览器操控修复同源）。
#[tauri::command]
pub async fn read_document(
    path: String,
    extract_text: Option<bool>,
    extract_tables: Option<bool>,
    extract_images: Option<bool>,
    export_csv: Option<bool>,
    csv_dir: Option<String>,
    max_chars: Option<u64>,
    recursive: Option<bool>,
) -> Result<Value, String> {
    let mut args = json!({ "path": path });
    if let Some(v) = extract_text {
        args["extract_text"] = json!(v);
    }
    if let Some(v) = extract_tables {
        args["extract_tables"] = json!(v);
    }
    if let Some(v) = extract_images {
        args["extract_images"] = json!(v);
    }
    if let Some(v) = export_csv {
        args["export_csv"] = json!(v);
    }
    if let Some(v) = csv_dir {
        args["csv_dir"] = json!(v);
    }
    if let Some(v) = max_chars {
        args["max_chars"] = json!(v);
    }
    if let Some(v) = recursive {
        args["recursive"] = json!(v);
    }
    tokio::task::spawn_blocking(move || crate::read_document::run(args))
        .await
        .map_err(|e| format!("文档解析失败: {e}"))?
}

/// 检查办公文档解析依赖（Python 运行时 + pdfplumber/python-docx/openpyxl/python-pptx/pypdf）
/// 是否就绪；缺失时返回安装命令，供环境探测面板做安装引导。
/// 子进程探测放到 blocking 线程池，避免启动 Python 阻塞主线程导致界面无响应。
#[tauri::command]
pub async fn check_document_deps() -> Result<Value, String> {
    tokio::task::spawn_blocking(|| crate::read_document::deps_report())
        .await
        .map_err(|e| format!("文档依赖检测失败: {e}"))
}

#[tauri::command]
pub fn get_pending_permissions(state: State<'_, AppState>) -> Vec<PermissionRequest> {
    state.security.pending()
}

#[tauri::command]
pub fn resolve_permission(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
    approved: bool,
    remember: bool,
) -> bool {
    state.escalation.cancel_escalation_with_event(&app, &id);
    // 「记住」：按「工具 + 具体情况」记录决定并持久化，下次相同情况直接放行/拒绝
    if remember {
        if let Some(req) = state.security.pending_by_id(&id) {
            state.security.remember(&req.tool, &req.args, approved);
        }
    }
    state.security.resolve(&id, approved)
}

// ---------------- 模型配置（前端可视化配置/切换） ----------------

/// 同步视觉/嵌入运行时：由当前模型配置派生（精确匹配激活项，缺省时回退）
fn sync_model_runtime(config: &crate::model::ModelConfig) {
    if let Some((base_url, api_key, vision_model)) = config.vision_conn() {
        crate::visual_grounding::set_vision_cloud(&base_url, &api_key);
        if !vision_model.is_empty() {
            crate::visual_grounding::set_vision_model(vision_model.clone());
        }
        // 视觉链路留痕：视觉/OCR 走的是独立于主 LLM 的连接（优先激活模型，
        // 激活模型无视觉能力时回退到第一个配了 vision_model 的云端配置）
        let active = config.active_id();
        let active_name = config
            .effective_profiles()
            .iter()
            .find(|p| p.id == active)
            .map(|p| p.name.clone())
            .unwrap_or_default();
        println!(
            "[视觉链路] 视觉/OCR 连接 → {base_url}（激活模型: {active_name}，视觉模型: {}）",
            if vision_model.is_empty() { "运行时默认" } else { &vision_model }
        );
    }
    crate::visual_grounding::sync_multimodal_main(config);
    if let Some(em) = config.embedding_model() {
        crate::embedding::set_embed_model(em);
    }
}

#[tauri::command]
pub fn get_model_config(state: State<'_, AppState>) -> ModelConfig {
    let mut config = crate::load_model_config(&state.store);
    // 规范化：把旧字段平滑迁移为多模型列表 + 有效激活项，前端始终拿到可编辑的 profiles/active
    config.profiles = config.effective_profiles();
    config.active = config.active_id();
    // 脱敏：明文的 API Key 仅驻留后端内存，返回前端只留 has_key 标记
    crate::mask_model_keys(&mut config);
    config
}

#[tauri::command]
pub async fn set_model_config(
    state: State<'_, AppState>,
    mut config: ModelConfig,
) -> Result<ModelConfig, String> {
    // 还原前端「留空表示保留」的 vault 密钥，供校验与运行时 provider 构建使用
    crate::hydrate_model_keys(&state.store, &mut config);
    // 校验：至少一个启用且可用的模型
    let any_ok = config
        .effective_profiles()
        .iter()
        .any(|p| p.enabled && (p.tier == ModelTier::Local || !p.api_key.trim().is_empty()));
    if !any_ok {
        return Err("请至少启用一个可用的模型（本地，或填了 API Key 的云端）".to_string());
    }
    // 若激活项无效/为空，回退到第一个启用项
    if config.active.is_empty()
        || !config
            .profiles
            .iter()
            .any(|p| p.id == config.active && p.enabled)
    {
        config.active = config
            .profiles
            .iter()
            .find(|p| p.enabled)
            .map(|p| p.id.clone())
            .unwrap_or_default();
    }

    // 运行时重建路由 + 视觉/嵌入同步（需在脱敏前用明文 key 构建）
    state.model.rebuild(&config);
    sync_model_runtime(&config);
    let label = config.chain_label();
    // 脱敏加密 API Key 后持久化；返回给前端的 config 同步脱敏
    crate::persist_model_config(&state.store, &mut config)?;

    println!("[模型] 配置已更新，链路: {}", label);
    Ok(config)
}

/// 全局切换当前激活模型（输入框下拉切换即调用此命令，立即生效并持久化）
#[tauri::command]
pub fn set_active_model(state: State<'_, AppState>, id: String) -> Result<ModelConfig, String> {
    let mut config = crate::load_model_config(&state.store);
    config.profiles = config.effective_profiles();
    let target = config
        .profiles
        .iter()
        .find(|p| p.id == id)
        .ok_or_else(|| format!("模型 {id} 不存在"))?;
    if !target.enabled {
        return Err(format!("模型 {id} 未启用"));
    }
    config.active = id.clone();
    state.model.rebuild(&config);
    sync_model_runtime(&config);
    crate::persist_model_config(&state.store, &mut config)?;
    println!("[模型] 已切换激活模型: {}", id);
    Ok(config)
}

/// 返回内置厂商预设清单（前端「添加模型」下拉选择时读取）
#[tauri::command]
pub fn get_vendor_presets() -> Vec<crate::model::VendorPreset> {
    crate::model::vendor_presets()
}

/// 测试某个已保存 profile 的连接：从当前配置还原密钥 → 构建 provider → 发一条最小消息
#[tauri::command]
pub async fn test_model_profile(state: State<'_, AppState>, id: String) -> Result<String, String> {
    let mut config = crate::load_model_config(&state.store);
    config.profiles = config.effective_profiles();
    crate::hydrate_model_keys(&state.store, &mut config);
    let p = config
        .profiles
        .iter()
        .find(|p| p.id == id)
        .cloned()
        .ok_or_else(|| format!("模型 {id} 不存在"))?;
    let prov = p
        .to_provider()
        .ok_or_else(|| "模型不可用（云端需配置 API Key）".to_string())?;
    let msgs = vec![ChatMessage {
        role: "user".to_string(),
        content: "请只回复两个字：正常".to_string(),
        tool_calls: None,
        tool_call_id: None,
    }];
    let resp = prov.chat(&msgs, &[]).await.map_err(|e| e.to_string())?;
    let content = resp
        .content
        .map(|c| c.trim().chars().take(80).collect::<String>())
        .unwrap_or_default();
    if content.is_empty() {
        Ok("连接成功（模型返回空内容）".to_string())
    } else {
        Ok(content)
    }
}

// ---------------- 对话分支（同一问题并行对比多个模型） ----------------

#[tauri::command]
pub async fn compare_models(
    state: State<'_, AppState>,
    message: String,
    history: Vec<ChatMessage>,
) -> Result<Vec<crate::model::ModelAnswer>, String> {
    // 组装消息：固定系统人设 + 历史上下文 + 本轮问题；不进入 Agent 工具循环
    let mut full = vec![ChatMessage {
        role: "system".to_string(),
        content: "你是白泽，一个本地优先的桌面助手。请直接、简洁、准确地回答用户问题。".to_string(),
        tool_calls: None,
        tool_call_id: None,
    }];
    full.extend(history);
    full.push(ChatMessage {
        role: "user".to_string(),
        content: message,
        tool_calls: None,
        tool_call_id: None,
    });

    let answers = state.model.compare(&full, &[]).await;
    Ok(answers)
}

// ---------------- 多 Agent 会议室（圆桌讨论） ----------------

/// 一个参会成员：绑定到某个已保存的模型配置（profile）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MeetingParticipant {
    pub id: String,
    pub name: String,
    pub role: String,
    pub profile_id: String,
}

/// 会议中某成员调用的一次共享工具（用于记录与前端展示）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MeetingToolUse {
    pub tool: String,
    pub args: Value,
    /// 结果摘要（已截断，避免记录过长）
    pub result: String,
}

/// 单条发言（某成员一次模型输出）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MeetingUtterance {
    pub speaker_id: String,
    pub speaker_name: String,
    pub round: usize,
    pub profile_id: String,
    pub content: String,
    /// 是否因「打断/停止」被截断
    #[serde(default)]
    pub interrupted: bool,
    /// 本次发言过程中调用过的共享工具（读/查类自动放行；高危/系统写自动跳过）
    #[serde(default)]
    pub tools_used: Vec<MeetingToolUse>,
}

/// 会议全局控制（打断 / 停止），进程级单例
struct MeetingControl {
    stop: AtomicBool,
    interrupt: AtomicBool,
}

static MEETING: OnceLock<Arc<MeetingControl>> = OnceLock::new();

fn meeting_ctl() -> Arc<MeetingControl> {
    MEETING
        .get_or_init(|| {
            Arc::new(MeetingControl {
                stop: AtomicBool::new(false),
                interrupt: AtomicBool::new(false),
            })
        })
        .clone()
}

/// 打断当前正在发言的成员：中断其流式输出，保留已生成的部分内容并跳到下一位
#[tauri::command]
pub fn meeting_interrupt() {
    meeting_ctl().interrupt.store(true, Ordering::SeqCst);
}

/// 停止整场会议：中断当前发言并结束后续所有成员
#[tauri::command]
pub fn meeting_stop() {
    let ctl = meeting_ctl();
    ctl.stop.store(true, Ordering::SeqCst);
    ctl.interrupt.store(true, Ordering::SeqCst);
}

/// 圆桌讨论：成员按顺序轮流发言，每轮都能看到此前的完整发言记录。
/// 期间通过 meeting-speaker / meeting-token / meeting-utterance / meeting-error 事件实时推送。
/// 支持打断（meeting_interrupt）与停止（meeting_stop）。
#[tauri::command]
pub async fn run_meeting(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    topic: String,
    participants: Vec<MeetingParticipant>,
    rounds: usize,
) -> Result<Vec<MeetingUtterance>, String> {
    if participants.is_empty() {
        return Err("请至少选择一位参会成员".to_string());
    }
    if topic.trim().is_empty() {
        return Err("请填写会议主题".to_string());
    }
    let rounds = rounds.clamp(1, 20);

    // 重置会议控制状态
    let ctl = meeting_ctl();
    ctl.stop.store(false, Ordering::SeqCst);
    ctl.interrupt.store(false, Ordering::SeqCst);

    // 解析每个成员绑定的 provider（按 profile_id 精确定位，无自动降级）
    let config = crate::load_model_config(&state.store);
    let profiles = config.effective_profiles();
    let mut members: Vec<(MeetingParticipant, Arc<dyn ModelProvider>)> = Vec::new();
    for p in &participants {
        let profile = profiles
            .iter()
            .find(|f| f.id == p.profile_id)
            .ok_or_else(|| format!("成员「{}」绑定的模型 {} 不存在", p.name, p.profile_id))?;
        if !profile.enabled {
            return Err(format!("成员「{}」绑定的模型 {} 未启用", p.name, p.profile_id));
        }
        let provider = profile.to_provider().ok_or_else(|| {
            format!("成员「{}」绑定的模型 {} 不可用（云端需填写 API Key）", p.name, p.profile_id)
        })?;
        members.push((p.clone(), provider));
    }

    let mut transcript: Vec<String> = Vec::new(); // 已完成的发言，供后续成员参考
    let mut utterances: Vec<MeetingUtterance> = Vec::new();

    'outer: for r in 0..rounds {
        for (member, provider) in &members {
            // 每成员发言前检查是否被停止
            if ctl.stop.load(Ordering::SeqCst) {
                break 'outer;
            }

            // 通知前端「谁即将发言」
            let _ = app.emit(
                "meeting-speaker",
                json!({
                    "speaker_id": member.id,
                    "speaker_name": member.name,
                    "round": r + 1,
                }),
            );

            // 组装上下文：人设 + 会议规则 + 此前发言记录 + 请发言
            let mut context = String::new();
            if transcript.is_empty() {
                context.push_str("会议刚刚开始，请先就主题发表你的开场观点。");
            } else {
                context.push_str("以下是此前的会议发言记录：\n");
                for (i, t) in transcript.iter().enumerate() {
                    context.push_str(&format!("{}. {}\n", i + 1, t));
                }
                context.push_str(&format!("\n现在是第 {} 轮，请「{}」发言。", r + 1, member.name));
            }

            let mut messages = vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: format!(
                        "你叫「{}」，在一个多智能体圆桌会议中担任「{}」。\n会议主题：{}\n会议规则：请用第一人称、简明扼要地发表观点，可补充、赞同或礼貌反驳他人的发言；不要重复前面已经说过的原话；不要使用表情符号；只输出你的发言正文，不要加任何前缀称呼。\n如需查证事实、读取文件、查询数据库、检索资料或访问接口，可先调用共享工具获取真实信息，再据此给出有依据的发言。",
                        member.name, member.role, topic
                    ),
                    tool_calls: None,
                    tool_call_id: None,
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: context,
                    tool_calls: None,
                    tool_call_id: None,
                },
            ];

            // 共享工具集：所有成员共用同一份工具注册表（读/查类自动放行，高危/系统写自动跳过）
            let tool_schemas = state.tools.schemas();

            // 每成员最多 6 轮「工具调用 → 观察结果」，直到给出纯文本发言
            let mut tools_used: Vec<MeetingToolUse> = Vec::new();
            let mut final_content = String::new();
            let mut interrupted = false;
            let mut stream_err: Option<String> = None;

            for _round in 0..6 {
                if ctl.stop.load(Ordering::SeqCst) {
                    interrupted = true;
                    break;
                }

                // 流式 token 实时推送 + 本轮取消标志（stop/interrupt 任一触发即中断）
                let app2 = app.clone();
                let sid = member.id.clone();
                let cb = move |tok: &str| {
                    let _ = app2.emit(
                        "meeting-token",
                        json!({ "speaker_id": sid.clone(), "token": tok }),
                    );
                };

                let cancel = Arc::new(AtomicBool::new(false));
                let cancel_w = cancel.clone();
                let ctl_w = ctl.clone();
                let watcher = tokio::spawn(async move {
                    loop {
                        if ctl_w.stop.load(Ordering::SeqCst) || ctl_w.interrupt.load(Ordering::SeqCst) {
                            cancel_w.store(true, Ordering::SeqCst);
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                    }
                });

                let result = provider
                    .stream_chat_ctl(&messages, &tool_schemas, &cb, cancel.as_ref())
                    .await;
                watcher.abort();
                interrupted = cancel.load(Ordering::SeqCst);

                match result {
                    Ok(resp) => {
                        // 被打断：保留已生成的部分内容作为本条发言
                        if interrupted {
                            final_content = resp.content.unwrap_or_default();
                            break;
                        }
                        match resp.tool_calls {
                            Some(tc) if !tc.is_empty() => {
                                // 本轮是工具调用：推 assistant 消息，再逐个执行并回填 tool 结果
                                messages.push(ChatMessage {
                                    role: "assistant".to_string(),
                                    content: resp.content.unwrap_or_default(),
                                    tool_calls: Some(tc.clone()),
                                    tool_call_id: None,
                                });
                                for call in &tc {
                                    let name = call["function"]["name"].as_str().unwrap_or("").to_string();
                                    let args: Value = match call["function"]["arguments"].clone() {
                                        Value::String(s) => {
                                            serde_json::from_str(&s).unwrap_or(Value::String(s))
                                        }
                                        other => other,
                                    };
                                    let call_id = call["id"].as_str().unwrap_or("").to_string();

                                    // 通知前端「谁在调用哪个工具」，用于重置流式文本与展示工具活动
                                    let _ = app.emit(
                                        "meeting-tool",
                                        json!({
                                            "speaker_id": member.id,
                                            "speaker_name": member.name,
                                            "tool": name,
                                            "args": args,
                                        }),
                                    );

                                    let result_val = execute_meeting_tool(&state, &name, &args).await;
                                    let result_str = result_val.to_string();
                                    tools_used.push(MeetingToolUse {
                                        tool: name,
                                        args,
                                        result: result_str.chars().take(300).collect(),
                                    });

                                    // 喂给模型的工具结果做「首尾保留」截断；记录用完整摘要
                                    let model_output =
                                        crate::token_saver::cap_tool_result(&result_val.to_string());
                                    messages.push(ChatMessage {
                                        role: "tool".to_string(),
                                        content: model_output,
                                        tool_calls: None,
                                        tool_call_id: Some(call_id),
                                    });
                                }
                                // 继续下一轮：让模型基于工具结果给出最终发言
                            }
                            _ => {
                                // 纯文本 → 最终发言
                                final_content = resp.content.unwrap_or_default();
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        stream_err = Some(e);
                        break;
                    }
                }
            }

            if let Some(e) = stream_err {
                // 单个成员失败不中断整场会议，仅向前端报错
                let _ = app.emit(
                    "meeting-error",
                    json!({
                        "speaker_id": member.id,
                        "speaker_name": member.name,
                        "error": e,
                    }),
                );
            } else {
                let content = final_content.trim().to_string();
                if !content.is_empty() {
                    transcript.push(format!("【{}】{}", member.name, content));
                    let u = MeetingUtterance {
                        speaker_id: member.id.clone(),
                        speaker_name: member.name.clone(),
                        round: r + 1,
                        profile_id: member.profile_id.clone(),
                        content,
                        interrupted,
                        tools_used,
                    };
                    let _ = app.emit("meeting-utterance", &u);
                    utterances.push(u);
                }
            }

            if interrupted {
                if ctl.stop.load(Ordering::SeqCst) {
                    break 'outer;
                }
                // 打断：清除标志，跳到下一位成员继续
                ctl.interrupt.store(false, Ordering::SeqCst);
            }
        }
    }

    Ok(utterances)
}

/// 会议模式下的共享工具执行：读/查类自动放行；高危操作与「系统目录写入」自动跳过（会议非阻塞，不弹审批）。
/// 工具真正执行放到 blocking 线程池，避免阻塞异步运行时。
async fn execute_meeting_tool(state: &AppState, name: &str, args: &Value) -> Value {
    let Some(tool) = state.tools.get(name) else {
        return json!({ "error": format!("未知工具: {name}") });
    };
    let class = tool.permission();
    match state.security.classify(name, args, class) {
        PermissionDecision::AutoAllow => {
            let tool = tool.clone();
            let args = args.clone();
            match tokio::task::spawn_blocking(move || tool.run(args)).await {
                Ok(res) => res.unwrap_or_else(|e| json!({ "error": e })),
                Err(e) => json!({ "error": format!("工具执行失败: {e}") }),
            }
        }
        PermissionDecision::AutoDeny => json!({ "error": "已被记住的规则拒绝此操作" }),
        PermissionDecision::Prompt(_) => json!({ "error": "会议模式下该操作需人工确认，已跳过（仅自动放行只读/普通读写）" }),
    }
}

/// 会议总结：用激活模型对整场发言生成结构化总结，流式推向 meeting-summary-token
#[tauri::command]
pub async fn summarize_meeting(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    topic: String,
    utterances: Vec<MeetingUtterance>,
) -> Result<String, String> {
    if utterances.is_empty() {
        return Err("暂无发言记录，无法总结".to_string());
    }
    let mut transcript = String::new();
    for u in utterances.iter() {
        transcript.push_str(&format!("【{}】{}\n", u.speaker_name, u.content));
    }

    let messages = vec![
        ChatMessage {
            role: "system".to_string(),
            content: "你是一位会议记录员。请对下面这场多智能体会议的发言做一份结构清晰的中文总结，包含：核心结论、各方主要观点与分歧、以及一条可执行的下一步建议。用简洁的要点式输出。".to_string(),
            tool_calls: None,
            tool_call_id: None,
        },
        ChatMessage {
            role: "user".to_string(),
            content: format!("会议主题：{}\n\n完整发言记录：\n{}", topic, transcript),
            tool_calls: None,
            tool_call_id: None,
        },
    ];

    let app2 = app.clone();
    let cb = move |tok: &str| {
        let _ = app2.emit("meeting-summary-token", json!({ "token": tok }));
    };
    let resp = state.model.stream_chat(&messages, &[], &cb).await?;
    Ok(resp.content.unwrap_or_default())
}

// ---------------- 协作执行（负责人拆解 → 成员分工执行 → 汇总交付） ----------------

/// 协作执行：负责人拆解出的一个子任务
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TeamTask {
    pub title: String,
    pub assignee: String,
    pub detail: String,
}

/// 协作执行：一个阶段/子任务的产出条目（规划 / 任务 / 汇总）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TeamEntry {
    pub kind: String,       // "plan" | "task" | "summary"
    pub speaker_name: String,
    pub title: String,
    pub content: String,
    pub tools_used: Vec<MeetingToolUse>,
    pub interrupted: bool,
}

/// 单成员「工具调用循环」的执行结果（协作执行复用）
struct AgentRun {
    content: String,
    tools_used: Vec<MeetingToolUse>,
    interrupted: bool,
    error: Option<String>,
}

/// 从规划文本中提取 JSON 数组（容忍 ```json 包裹或前后说明文字）
fn extract_task_array(text: &str) -> Option<Vec<TeamTask>> {
    let start = text.find('[')?;
    let end = text.rfind(']')?;
    if end <= start {
        return None;
    }
    serde_json::from_str::<Vec<TeamTask>>(&text[start..=end]).ok()
}

/// 让某成员「带共享工具」完成一段任务：多轮「工具调用 → 观察结果」，直到给出纯文本产出。
/// 事件前缀固定为 teamwork-*，与讨论模式的 meeting-* 隔离，避免前端两种模式串扰。
async fn run_agent_task(
    state: &AppState,
    app: &tauri::AppHandle,
    ctl: &Arc<MeetingControl>,
    member: &MeetingParticipant,
    provider: &Arc<dyn ModelProvider>,
    system_prompt: &str,
    user_content: &str,
    tool_schemas: &[Value],
) -> AgentRun {
    let mut tools_used: Vec<MeetingToolUse> = Vec::new();
    let mut final_content = String::new();
    let mut interrupted = false;
    let mut stream_err: Option<String> = None;

    let mut messages = vec![
        ChatMessage {
            role: "system".to_string(),
            content: system_prompt.to_string(),
            tool_calls: None,
            tool_call_id: None,
        },
        ChatMessage {
            role: "user".to_string(),
            content: user_content.to_string(),
            tool_calls: None,
            tool_call_id: None,
        },
    ];

    for _round in 0..6 {
        if ctl.stop.load(Ordering::SeqCst) {
            interrupted = true;
            break;
        }

        let app2 = app.clone();
        let sid = member.id.clone();
        let cb = move |tok: &str| {
            let _ = app2.emit(
                "teamwork-token",
                json!({ "speaker_id": sid.clone(), "token": tok }),
            );
        };

        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_w = cancel.clone();
        let ctl_w = ctl.clone();
        let watcher = tokio::spawn(async move {
            loop {
                if ctl_w.stop.load(Ordering::SeqCst) || ctl_w.interrupt.load(Ordering::SeqCst) {
                    cancel_w.store(true, Ordering::SeqCst);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            }
        });

        let result = provider
            .stream_chat_ctl(&messages, tool_schemas, &cb, cancel.as_ref())
            .await;
        watcher.abort();
        interrupted = cancel.load(Ordering::SeqCst);

        match result {
            Ok(resp) => {
                if interrupted {
                    final_content = resp.content.unwrap_or_default();
                    break;
                }
                match resp.tool_calls {
                    Some(tc) if !tc.is_empty() => {
                        messages.push(ChatMessage {
                            role: "assistant".to_string(),
                            content: resp.content.unwrap_or_default(),
                            tool_calls: Some(tc.clone()),
                            tool_call_id: None,
                        });
                        for call in &tc {
                            let name = call["function"]["name"].as_str().unwrap_or("").to_string();
                            let args: Value = match call["function"]["arguments"].clone() {
                                Value::String(s) => {
                                    serde_json::from_str(&s).unwrap_or(Value::String(s))
                                }
                                other => other,
                            };
                            let call_id = call["id"].as_str().unwrap_or("").to_string();

                            let _ = app.emit(
                                "teamwork-tool",
                                json!({
                                    "speaker_id": member.id,
                                    "speaker_name": member.name,
                                    "tool": name,
                                    "args": args,
                                }),
                            );

                            let result_val = execute_meeting_tool(state, &name, &args).await;
                            let result_str = result_val.to_string();
                            tools_used.push(MeetingToolUse {
                                tool: name,
                                args,
                                result: result_str.chars().take(300).collect(),
                            });

                            let model_output =
                                crate::token_saver::cap_tool_result(&result_val.to_string());
                            messages.push(ChatMessage {
                                role: "tool".to_string(),
                                content: model_output,
                                tool_calls: None,
                                tool_call_id: Some(call_id),
                            });
                        }
                    }
                    _ => {
                        final_content = resp.content.unwrap_or_default();
                        break;
                    }
                }
            }
            Err(e) => {
                stream_err = Some(e);
                break;
            }
        }
    }

    AgentRun {
        content: final_content,
        tools_used,
        interrupted,
        error: stream_err,
    }
}

/// 协作执行主流程：负责人先拆解任务 → 各成员分工用共享工具执行 → 负责人汇总成交付物。
/// 通过 teamwork-stage / teamwork-token / teamwork-tool / teamwork-entry 事件实时推送。
#[tauri::command]
pub async fn run_teamwork(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    topic: String,
    participants: Vec<MeetingParticipant>,
) -> Result<Vec<TeamEntry>, String> {
    if participants.is_empty() {
        return Err("请至少选择一位成员".to_string());
    }
    if topic.trim().is_empty() {
        return Err("请填写任务主题".to_string());
    }

    let ctl = meeting_ctl();
    ctl.stop.store(false, Ordering::SeqCst);
    ctl.interrupt.store(false, Ordering::SeqCst);

    let config = crate::load_model_config(&state.store);
    let profiles = config.effective_profiles();
    let mut members: Vec<(MeetingParticipant, Arc<dyn ModelProvider>)> = Vec::new();
    for p in &participants {
        let profile = profiles
            .iter()
            .find(|f| f.id == p.profile_id)
            .ok_or_else(|| format!("成员「{}」绑定的模型 {} 不存在", p.name, p.profile_id))?;
        if !profile.enabled {
            return Err(format!("成员「{}」绑定的模型 {} 未启用", p.name, p.profile_id));
        }
        let provider = profile.to_provider().ok_or_else(|| {
            format!("成员「{}」绑定的模型 {} 不可用（云端需填写 API Key）", p.name, p.profile_id)
        })?;
        members.push((p.clone(), provider));
    }

    let coordinator_name = members[0].0.name.clone();
    let tool_schemas = state.tools.schemas();
    let no_tools: Vec<Value> = Vec::new();
    let mut entries: Vec<TeamEntry> = Vec::new();

    // ---- 阶段 1：规划（负责人拆解任务） ----
    let names_joined = members
        .iter()
        .map(|(p, _)| p.name.clone())
        .collect::<Vec<_>>()
        .join("、");
    let _ = app.emit(
        "teamwork-stage",
        json!({ "stage": "plan", "label": format!("{coordinator_name} 正在拆解任务…") }),
    );

    let n_tasks = members.len().max(2).min(5);
    let plan_prompt = format!(
        "你是团队负责人「{coordinator_name}」。请把下面这个目标拆解成 {n_tasks} 个可独立完成的子任务，并分配给团队成员。\n\n目标：{topic}\n\n团队成员名单：{names_joined}\n\n要求：\n1. 只输出一个 JSON 数组，不要输出任何其他文字或 markdown 代码块标记；\n2. 数组每个元素形如 {{\"title\":\"子任务标题\",\"assignee\":\"负责成员姓名\",\"detail\":\"子任务具体要求、产出物与验收标准\"}}；\n3. assignee 必须从团队成员名单中选取；\n4. 子任务尽量覆盖目标的不同侧面，分配给不同成员。"
    );

    let noop = |_: &str| {};
    let planner = &members[0];
    let plan_resp = planner
        .1
        .stream_chat(
            &[ChatMessage {
                role: "system".to_string(),
                content: plan_prompt,
                tool_calls: None,
                tool_call_id: None,
            }],
            &no_tools,
            &noop,
        )
        .await;
    let plan_text = match plan_resp {
        Ok(r) => r.content.unwrap_or_default(),
        Err(_) => String::new(),
    };
    let mut tasks = extract_task_array(&plan_text).unwrap_or_default();

    // 降级：解析失败或为空时，把整体目标当作单一任务，交给第一个非负责人成员
    if tasks.is_empty() {
        let fallback = members
            .iter()
            .skip(1)
            .next()
            .map(|(p, _)| p.name.clone())
            .unwrap_or_else(|| coordinator_name.clone());
        tasks.push(TeamTask {
            title: topic.clone(),
            assignee: fallback,
            detail: "完成上述目标，产出结果、结论或可交付成果".to_string(),
        });
    }

    let plan_summary = tasks
        .iter()
        .enumerate()
        .map(|(i, t)| format!("{}. [{}] {} —— {}", i + 1, t.assignee, t.title, t.detail))
        .collect::<Vec<_>>()
        .join("\n");
    let plan_entry = TeamEntry {
        kind: "plan".to_string(),
        speaker_name: coordinator_name.clone(),
        title: "任务拆解".to_string(),
        content: format!("已将目标拆解为 {} 个子任务：\n{}", tasks.len(), plan_summary),
        tools_used: Vec::new(),
        interrupted: false,
    };
    let _ = app.emit("teamwork-entry", &plan_entry);
    entries.push(plan_entry);

    // ---- 阶段 2：执行（各成员分工，带共享工具） ----
    for task in &tasks {
        if ctl.stop.load(Ordering::SeqCst) {
            break;
        }
        let member_idx = members
            .iter()
            .position(|(p, _)| p.name.as_str() == task.assignee.as_str())
            .or_else(|| {
                members
                    .iter()
                    .position(|(p, _)| p.name.as_str() != coordinator_name.as_str())
            })
            .unwrap_or(0);
        let (member, provider) = &members[member_idx];

        let _ = app.emit(
            "teamwork-stage",
            json!({ "stage": "task", "label": format!("{} 正在执行：{}", member.name, task.title) }),
        );

        let sys = format!(
            "你是「{}」，在一个多智能体协作团队中负责执行分配给你的子任务。请专注完成你的子任务，可调用共享工具（读文件、查库、检索、访问接口、生成内容等）获取真实信息，最终输出你的成果。用正文直接呈现成果，不要加无关解释。",
            member.name
        );
        let run = run_agent_task(
            &state,
            &app,
            &ctl,
            member,
            provider,
            &sys,
            &format!("子任务：{}\n具体要求：{}", task.title, task.detail),
            &tool_schemas,
        )
        .await;

        let stopped = run.interrupted && ctl.stop.load(Ordering::SeqCst);
        let entry_content = match run.error {
            Some(e) => format!("执行失败：{e}"),
            None => run.content,
        };
        let entry = TeamEntry {
            kind: "task".to_string(),
            speaker_name: member.name.clone(),
            title: task.title.clone(),
            content: entry_content,
            tools_used: run.tools_used,
            interrupted: run.interrupted,
        };
        if !entry.content.trim().is_empty() {
            let _ = app.emit("teamwork-entry", &entry);
            entries.push(entry);
        }
        if stopped {
            break;
        }
    }

    // ---- 阶段 3：汇总（负责人整合交付物） ----
    if !ctl.stop.load(Ordering::SeqCst) && !entries.is_empty() {
        let _ = app.emit(
            "teamwork-stage",
            json!({ "stage": "summary", "label": format!("{coordinator_name} 正在汇总交付物…") }),
        );
        let deliverables = entries
            .iter()
            .filter(|e| e.kind == "task")
            .map(|e| format!("【子任务：{}】\n{}", e.title, e.content))
            .collect::<Vec<_>>()
            .join("\n\n");

        let (member, provider) = &members[0];
        let sys = format!(
            "你是团队负责人「{coordinator_name}」。请把下面各成员完成的子任务成果整合成一份完整、结构清晰的最终交付物，包含：目标回顾、各部分成果、整体结论与后续建议。用要点式中文输出。"
        );
        let run = run_agent_task(
            &state,
            &app,
            &ctl,
            member,
            provider,
            &sys,
            &format!("目标：{topic}\n\n各成员成果：\n{deliverables}"),
            &no_tools,
        )
        .await;

        let entry_content = match run.error {
            Some(e) => format!("汇总失败：{e}"),
            None => run.content,
        };
        let entry = TeamEntry {
            kind: "summary".to_string(),
            speaker_name: coordinator_name.clone(),
            title: "最终交付物".to_string(),
            content: entry_content,
            tools_used: run.tools_used,
            interrupted: run.interrupted,
        };
        let _ = app.emit("teamwork-entry", &entry);
        entries.push(entry);
    }

    if ctl.interrupt.load(Ordering::SeqCst) {
        ctl.interrupt.store(false, Ordering::SeqCst);
    }

    Ok(entries)
}

// ---------------- Token 节约配置（长上下文压缩 / 工具结果截断） ----------------

#[tauri::command]
pub fn get_token_saver_config(state: State<'_, AppState>) -> crate::token_saver::TokenSaverConfig {
    if let Ok(Some(json)) = state.store.get_setting("token_saver_config") {
        if let Ok(cfg) = serde_json::from_str::<crate::token_saver::TokenSaverConfig>(&json) {
            return cfg;
        }
    }
    crate::token_saver::TokenSaverConfig::default()
}

#[tauri::command]
pub fn set_token_saver_config(
    state: State<'_, AppState>,
    config: crate::token_saver::TokenSaverConfig,
) -> Result<(), String> {
    crate::token_saver::set_config(config.clone());
    let json = serde_json::to_string(&config).map_err(|e| e.to_string())?;
    state.store.set_setting("token_saver_config", &json)?;
    println!(
        "[Token 节约] 配置已更新：压缩={} 阈值={} 保留={} 工具截断={} 本地压缩={}",
        if config.auto_compress { "开" } else { "关" },
        config.compress_threshold_chars,
        config.keep_recent_chars,
        config.max_tool_result_chars,
        if config.local_only_compress { "是" } else { "否" },
    );
    Ok(())
}

// ---------------- 文生图（能力检测 + 图片生成） ----------------

#[tauri::command]
pub async fn detect_image_model(
    state: State<'_, AppState>,
) -> Result<crate::text_to_image::ImageCapability, String> {
    Ok(crate::text_to_image::detect(&state).await)
}

#[tauri::command]
pub async fn generate_image(
    state: State<'_, AppState>,
    prompt: String,
    size: Option<String>,
) -> Result<String, String> {
    crate::text_to_image::generate(&state, &prompt, size.as_deref()).await
}

// ---------------- MCP 配置（前端可视化配置/切换） ----------------

#[tauri::command]
pub fn get_mcp_config(state: State<'_, AppState>) -> McpConfig {
    crate::load_mcp_config(&state.store)
}

#[tauri::command]
pub async fn set_mcp_config(state: State<'_, AppState>, config: McpConfig) -> Result<McpConfig, String> {
    // 运行时重建 MCP 工具（旧连接关闭、新连接建立）
    let count = state.apply_mcp_config(&config)?;

    // 持久化
    let json = serde_json::to_string(&config).map_err(|e| e.to_string())?;
    state.store.set_setting("mcp_config", &json)?;

    println!("[MCP] 配置已更新，注册 {count} 个工具");
    Ok(config)
}

// ---------------- 记忆（M3：意识网络） ----------------

#[tauri::command]
pub fn get_memories(state: State<'_, AppState>) -> Vec<crate::memory::MemoryRow> {
    state.store.recent_memories(20).unwrap_or_default()
}

#[tauri::command]
pub fn get_memory_graph(state: State<'_, AppState>) -> crate::memory::MemoryGraph {
    state.store.memory_graph().unwrap_or(crate::memory::MemoryGraph {
        nodes: vec![],
        edges: vec![],
    })
}

/// 记忆治理：去重合并 + 衰减清理（星图更干净，召回更精准）
#[tauri::command]
pub fn memory_governance(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let (merged, decayed) = state.store.consolidate_memories()?;
    Ok(serde_json::json!({ "merged": merged, "decayed": decayed }))
}

#[tauri::command]
pub fn get_memory_overview(state: State<'_, AppState>) -> crate::memory::MemoryOverview {
    state
        .store
        .memory_overview()
        .unwrap_or(crate::memory::MemoryOverview {
            memories: 0,
            events: 0,
            semantic: 0,
            scheduled: 0,
            watchdog: 0,
        })
}

/// 按关键词遗忘相关记忆（工作记忆 / 语义记忆 / 情景事件），返回删除条数。
#[tauri::command]
pub fn forget_memory(state: State<'_, AppState>, keyword: String) -> Result<usize, String> {
    state.store.forget_matching(&keyword)
}

// ---------------- 知识库管理（RAG） ----------------

#[tauri::command]
pub fn get_rag_state(state: State<'_, AppState>) -> Vec<Value> {
    state.rag.list_paths()
}

#[tauri::command]
pub async fn index_rag_dir(state: State<'_, AppState>, path: String) -> Result<Value, String> {
    // 目录索引涉及文件读取 + 文本切分 + 向量化，属于阻塞 I/O/计算，移到 blocking 线程池执行
    let rag = state.rag.clone();
    let path_for_index = path.clone();
    let count = tokio::task::spawn_blocking(move || rag.index_dir(&path_for_index, 200))
        .await
        .map_err(|e| format!("索引任务异常: {e}"))??;
    Ok(json!({ "ok": true, "path": path, "chunks": count }))
}

#[tauri::command]
pub fn clear_rag(state: State<'_, AppState>) -> Result<bool, String> {
    state.rag.clear()?;
    Ok(true)
}

#[tauri::command]
pub fn search_rag(state: State<'_, AppState>, query: String) -> Value {
    let hits = state.rag.search(&query, 5);
    json!({ "count": hits.len(), "hits": hits })
}

// ---------------- 数据库连接配置管理 ----------------

#[tauri::command]
pub fn get_db_connections(state: State<'_, AppState>) -> Vec<crate::tools::DbConnection> {
    state
        .store
        .get_setting("db_connections")
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

#[tauri::command]
pub fn save_db_connections(
    state: State<'_, AppState>,
    connections: Vec<crate::tools::DbConnection>,
) -> Result<bool, String> {
    let json = serde_json::to_string(&connections).map_err(|e| e.to_string())?;
    state.store.set_setting("db_connections", &json)?;
    crate::tools::refresh_db_connections(&connections);
    Ok(true)
}

// ---------------- 内置浏览器 / Markdown 文档（独立窗口状态） ----------------

#[tauri::command]
pub fn get_browser_state(state: State<'_, AppState>) -> Value {
    state.browser.lock().unwrap().snapshot()
}

/// 前端切换浏览器标签页时同步后端状态
#[tauri::command]
pub fn switch_browser_tab(state: State<'_, AppState>, id: String) -> bool {
    state.browser.lock().unwrap().set_active_tab(&id);
    true
}

/// 前端关闭浏览器标签页时同步后端状态
#[tauri::command]
pub fn close_browser_tab(state: State<'_, AppState>, id: String) -> bool {
    state.browser.lock().unwrap().close_tab(&id);
    true
}

/// 读取桌面浏览器路径配置：手动指定值 + 当前自动探测结果
#[tauri::command]
pub fn browser_get_path(state: State<'_, AppState>) -> Value {
    let custom = state.store.get_setting("browser_chrome_path").ok().flatten();
    let resolved = crate::browser::resolved_browser_path()
        .map(|p| p.to_string_lossy().to_string());
    json!({
        "custom": custom,
        "resolved": resolved,
        "found": resolved.is_some(),
    })
}

/// 保存/清除（空串清除）手动指定的桌面浏览器路径，并持久化。
/// 手动路径不存在时拒绝保存（避免静默回退造成困惑），前端展示具体原因；
/// 清空保存 = 恢复自动探测。
#[tauri::command]
pub fn browser_set_path(state: State<'_, AppState>, path: String) -> Value {
    let trimmed = path.trim().to_string();
    if trimmed.is_empty() {
        crate::browser::set_custom_browser_path(None);
        let _ = state.store.set_setting("browser_chrome_path", "");
        let resolved = crate::browser::resolved_browser_path()
            .map(|p| p.to_string_lossy().to_string());
        return json!({ "ok": true, "resolved": resolved, "note": "已恢复自动探测" });
    }
    // 手动路径必须真实存在，否则拒绝保存并说明原因（当前仍使用自动探测结果）
    if !std::path::Path::new(&trimmed).exists() {
        let resolved = crate::browser::resolved_browser_path()
            .map(|p| p.to_string_lossy().to_string());
        return json!({
            "ok": false,
            "error": format!("路径不存在：{trimmed}（未保存，仍使用自动探测）"),
            "resolved": resolved,
        });
    }
    crate::browser::set_custom_browser_path(Some(&trimmed));
    let _ = state.store.set_setting("browser_chrome_path", &trimmed);
    json!({ "ok": true, "resolved": Some(trimmed), "note": "已保存并使用手动指定的路径" })
}

/// 前端「Chrome 操控面板」统一入口：透明转发到 browser::act（与 BrowserActTool 同一动作集）。
/// 供用户手动驱动桌面谷歌浏览器：tabs / state / goto / click_text / screenshot / look 等。
/// browser::act 内部是阻塞的 Chrome CDP / 截图 / OCR / 视觉模型 HTTP / 点击操作，原先作为同步
/// #[tauri::command] 跑在 Tauri 主线程，点击控件时会把主线程占满导致界面无响应甚至崩溃，
/// 现改为 async + spawn_blocking，把阻塞执行挪到 blocking 线程池（与软件管家修复同源）。
#[tauri::command]
pub async fn browser_act(args: Value) -> Result<Value, String> {
    tokio::task::spawn_blocking(move || crate::browser::act(args))
        .await
        .map_err(|e| format!("浏览器操控失败: {e}"))?
}

/// 前端「预览」按钮：把完整 HTML 页面代码作为新标签页打开到内置浏览器窗口
#[tauri::command]
pub fn preview_html(
    app: AppHandle,
    state: State<'_, AppState>,
    html: String,
    title: Option<String>,
) -> Result<String, String> {
    let title = title
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| "HTML 预览".to_string());
    let tab_id = {
        let mut b = state.browser.lock().unwrap();
        b.open_tab("html", &title, &html)
    };
    crate::windows::ensure_browser_window(&app);
    let snap = state.browser.lock().unwrap().snapshot();
    let _ = app.emit_to("browser", "browser-update", &snap);
    Ok(tab_id)
}

#[tauri::command]
pub fn get_markdown_state(state: State<'_, AppState>) -> Value {
    state.markdown.lock().unwrap().snapshot()
}

/// 前端切换文档标签页时同步后端状态
#[tauri::command]
pub fn switch_markdown_tab(state: State<'_, AppState>, id: String) -> bool {
    state.markdown.lock().unwrap().set_active(&id);
    true
}

/// 前端关闭文档标签页时同步后端状态
#[tauri::command]
pub fn close_markdown_tab(state: State<'_, AppState>, id: String) -> bool {
    state.markdown.lock().unwrap().close_doc(&id);
    true
}

/// 保存文档：弹出原生「另存为」对话框，用户选择路径后写入文件。
/// 返回保存路径；用户取消返回 None。
#[tauri::command]
pub async fn save_markdown(title: String, content: String) -> Result<Option<String>, String> {
    tokio::task::spawn_blocking(move || -> Result<Option<String>, String> {
        let path = rfd::FileDialog::new()
            .set_file_name(&format!("{}.md", title))
            .add_filter("Markdown", &["md"])
            .add_filter("文本文件", &["txt"])
            .save_file();
        match path {
            Some(p) => {
                std::fs::write(&p, &content).map_err(|e| format!("保存失败: {e}"))?;
                Ok(Some(p.to_string_lossy().to_string()))
            }
            None => Ok(None),
        }
    })
    .await
    .map_err(|e| format!("保存失败: {e}"))?
}

/// 弹出文件选择对话框（支持多选），返回选中文件的绝对路径列表；取消返回 None。
/// 必须是 async + spawn_blocking：Tauri 同步命令跑在主线程上，rfd 的原生模态
/// 对话框需要独立消息循环与 STA COM 环境，主线程被阻塞会与 WebView2 冲突直接崩溃。
#[tauri::command]
pub async fn pick_files() -> Result<Option<Vec<String>>, String> {
    tokio::task::spawn_blocking(move || -> Option<Vec<String>> {
        let paths = rfd::FileDialog::new().pick_files()?;
        Some(
            paths
                .into_iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect(),
        )
    })
    .await
    .map_err(|e| format!("文件选择对话框异常: {e}"))
}

/// 弹出文件夹选择对话框，返回选中文件夹的绝对路径；取消返回 None。（同上必须异步）
#[tauri::command]
pub async fn pick_folder() -> Result<Option<String>, String> {
    tokio::task::spawn_blocking(move || -> Option<String> {
        rfd::FileDialog::new()
            .pick_folder()
            .map(|p| p.to_string_lossy().to_string())
    })
    .await
    .map_err(|e| format!("文件夹选择对话框异常: {e}"))
}

/// 桌面悬浮球：创建/关闭独立的 always-on-top 透明小窗（index.html#/orb）。
/// 返回 true=已创建悬浮球，false=已关闭。悬浮球状态经 Tauri 事件与主窗互通。
#[tauri::command]
pub async fn toggle_float_orb(app: tauri::AppHandle) -> Result<bool, String> {
    use tauri::Manager;
    if let Some(w) = app.get_webview_window("orb-float") {
        let _ = w.close();
        return Ok(false);
    }
    // 默认落在主屏右上角（留出任务栏高度）
    let (x, y) = match app.primary_monitor().ok().flatten() {
        Some(m) => {
            let sz = m.size();
            let scale = m.scale_factor();
            (sz.width as f64 / scale - 96.0, 72.0)
        }
        None => (1200.0, 72.0),
    };
    tauri::WebviewWindowBuilder::new(
        &app,
        "orb-float",
        tauri::WebviewUrl::App("index.html#/orb".into()),
    )
    .title("白泽")
    .inner_size(64.0, 64.0)
    .position(x, y)
    .decorations(false)
    .transparent(true)
    .always_on_top(true)
    .skip_taskbar(true)
    .resizable(false)
    .shadow(false)
    .build()
    .map_err(|e| format!("创建悬浮球窗口失败: {e}"))?;
    Ok(true)
}

/// 用系统默认程序打开本地文件/目录（供消息附件 chip 点击打开）。
/// 路径由前端文件选择器产生，仅做存在性校验；跨平台走 explorer/open/xdg-open。
#[tauri::command]
pub fn open_path(path: String) -> Result<(), String> {
    if !std::path::Path::new(&path).exists() {
        return Err(format!("路径不存在：{path}"));
    }
    let opened = {
        #[cfg(target_os = "windows")]
        {
            std::process::Command::new("explorer").arg(&path).spawn()
        }
        #[cfg(target_os = "macos")]
        {
            std::process::Command::new("open").arg(&path).spawn()
        }
        #[cfg(target_os = "linux")]
        {
            std::process::Command::new("xdg-open").arg(&path).spawn()
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        {
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "unsupported platform",
            ))
        }
    };
    opened.map(|_| ()).map_err(|e| format!("打开失败：{e}"))
}

/// 设置当前工作空间（后端强绑定）：文件工具的相对路径以它为根目录；空串清除。
#[tauri::command]
pub fn set_workspace(path: String) {
    crate::tools::set_workspace(&path);
}

// ---------------- 软件管家（只读命令，供前端直接调用） ----------------
// 这些命令底层都会启动 winget/powershell/python 等阻塞子进程；原先作为同步 #[tauri::command]
// 运行在 Tauri 主线程，装机面板一打开（env 标签自动探测 + 磁盘检测 + 文档依赖检测并发触发）
// 就会把主线程占满，导致白泽界面无响应甚至崩溃。现改为 async + spawn_blocking，把阻塞执行
// 挪到 blocking 线程池，主线程只负责等待结果。

/// 环境探测：包管理器 / 运行时 / 管理员权限
#[tauri::command]
pub async fn env_check(state: State<'_, AppState>) -> Result<Value, String> {
    let tool = state.tools.get("env_check").ok_or("环境探测工具未注册")?;
    tokio::task::spawn_blocking(move || tool.run(json!({})))
        .await
        .map_err(|e| format!("环境探测失败: {e}"))?
}

/// 搜索软件（走包管理器）
#[tauri::command]
pub async fn software_search(state: State<'_, AppState>, query: String) -> Result<Value, String> {
    let tool = state.tools.get("software_search").ok_or("软件搜索工具未注册")?;
    tokio::task::spawn_blocking(move || tool.run(json!({ "query": query })))
        .await
        .map_err(|e| format!("软件搜索失败: {e}"))?
}

/// 已安装软件列表
#[tauri::command]
pub async fn software_list(state: State<'_, AppState>) -> Result<Value, String> {
    let tool = state.tools.get("software_list").ok_or("软件列表工具未注册")?;
    tokio::task::spawn_blocking(move || tool.run(json!({})))
        .await
        .map_err(|e| format!("读取已装软件失败: {e}"))?
}

/// 读系统配置（环境变量 / PATH / 启动项）
#[tauri::command]
pub async fn system_get(state: State<'_, AppState>) -> Result<Value, String> {
    let tool = state.tools.get("system_get").ok_or("系统配置工具未注册")?;
    tokio::task::spawn_blocking(move || tool.run(json!({})))
        .await
        .map_err(|e| format!("读取系统配置失败: {e}"))?
}

/// 磁盘空间与装机习惯 + 推荐安装位置
#[tauri::command]
pub async fn disk_info(state: State<'_, AppState>) -> Result<Value, String> {
    let tool = state.tools.get("disk_info").ok_or("磁盘检测工具未注册")?;
    tokio::task::spawn_blocking(move || tool.run(json!({})))
        .await
        .map_err(|e| format!("磁盘检测失败: {e}"))?
}

// ---------------- 语音音色偏好（持久化） ----------------

#[tauri::command]
pub fn get_voice(state: State<'_, AppState>) -> String {
    state
        .store
        .get_setting("voice")
        .ok()
        .flatten()
        .unwrap_or_default()
}

#[tauri::command]
pub fn set_voice(state: State<'_, AppState>, voice: String) -> Result<(), String> {
    state.store.set_setting("voice", &voice)
}

// ---------------- 运行时配置（embedding/vision 模型等） ----------------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RuntimeConfig {
    pub embed_model: String,
    pub vision_model: String,
    /// "ollama" | "deepseek"
    pub vision_provider: String,
    /// 视觉模型总开关；关闭后所有视觉调用短路（GUI 走 OCR、图片描述降级）
    pub vision_enabled: bool,
}

#[tauri::command]
pub fn get_runtime_config(state: State<'_, AppState>) -> RuntimeConfig {
    let embed = state
        .store
        .get_setting("embed_model")
        .ok()
        .flatten()
        .unwrap_or_else(|| crate::embedding::embed_model());
    let vision = state
        .store
        .get_setting("vision_model")
        .ok()
        .flatten()
        .unwrap_or_else(|| crate::visual_grounding::vision_model());
    let provider = state
        .store
        .get_setting("vision_provider")
        .ok()
        .flatten()
        .unwrap_or_else(|| "ollama".to_string());
    let enabled = state
        .store
        .get_setting("vision_enabled")
        .ok()
        .flatten()
        .and_then(|s: String| s.parse::<bool>().ok())
        .unwrap_or_else(crate::visual_grounding::vision_enabled);
    RuntimeConfig {
        embed_model: embed,
        vision_model: vision,
        vision_provider: provider,
        vision_enabled: enabled,
    }
}

#[tauri::command]
pub fn set_runtime_config(
    state: State<'_, AppState>,
    config: RuntimeConfig,
) -> Result<(), String> {
    state
        .store
        .set_setting("vision_provider", &config.vision_provider)?;
    state.store.set_setting("embed_model", &config.embed_model)?;
    state
        .store
        .set_setting("vision_model", &config.vision_model)?;
    state
        .store
        .set_setting("vision_enabled", &config.vision_enabled.to_string())?;
    crate::visual_grounding::set_vision_provider(&config.vision_provider);
    crate::embedding::set_embed_model(config.embed_model);
    crate::visual_grounding::set_vision_model(config.vision_model);
    crate::visual_grounding::set_vision_enabled(config.vision_enabled);
    Ok(())
}

// ---------------- 通知升级配置 ----------------

#[tauri::command]
pub fn get_notify_config(state: State<'_, AppState>) -> crate::notify::NotifyConfig {
    // 从持久化配置恢复
    if let Ok(Some(json)) = state.store.get_setting("notify_config") {
        if let Ok(config) = serde_json::from_str::<crate::notify::NotifyConfig>(&json) {
            return config;
        }
    }
    state.escalation.get_config()
}

#[tauri::command]
pub async fn set_notify_config(
    state: State<'_, AppState>,
    config: crate::notify::NotifyConfig,
) -> Result<(), String> {
    state.escalation.set_config(config.clone());
    let json = serde_json::to_string(&config).map_err(|e| e.to_string())?;
    state.store.set_setting("notify_config", &json)?;
    println!("[通知升级] 配置已更新");
    Ok(())
}

// ---------------- 工作模式（软件测试工程师 / 开发工程师） ----------------

#[tauri::command]
pub fn get_work_modes(state: State<'_, AppState>) -> Vec<crate::workmode::WorkMode> {
    state.workmodes.list()
}

#[tauri::command]
pub fn get_work_mode(state: State<'_, AppState>) -> Value {
    let cur = state.workmodes.current();
    json!({
        "current": cur.as_ref().map(|m| m.id.clone()),
        "label": cur.as_ref().map(|m| m.label.clone()),
        "authored": state.workmodes.authored(),
    })
}

#[tauri::command]
pub fn set_work_mode(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
) -> Result<Value, String> {
    // 切换前回收旧模式在 workmode 命名空间下的自研工具
    state.tools.remove_ns("workmode");
    let mode = state.workmodes.activate(&id)?;
    // 持久化当前模式，重启后恢复
    let _ = state.store.set_setting("work_mode_current", &id);
    let _ = app.emit("workmode-change", json!({ "id": mode.id, "label": mode.label }));
    Ok(json!({ "id": mode.id, "label": mode.label }))
}

// ---------------- 测试工程师：UI/接口 自动化测试（前端面板直连） ----------------

/// 生成结构化测试用例（面板「用例入口」直连；在阻塞线程跑异步管线，避免卡 UI）
#[tauri::command]
pub async fn test_generate_cases(
    app: AppHandle,
    requirement: Option<String>,
    path: Option<String>,
    case_types: Option<Vec<String>>,
    per_type_count: Option<u64>,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        // 阶段① 读取需求文档：解析本地文件前先广播进度，避免长时间无反馈
        let _ = app.emit(
            "thought",
            json!({ "kind": "test_pipeline", "label": "读取需求文档", "detail": "正在解析文档…" }),
        );
        let req = crate::test_engineer::resolve_requirement_from(requirement.as_deref(), path.as_deref())?;
        let _ = app.emit(
            "thought",
            json!({ "kind": "test_pipeline", "label": "读取需求文档", "detail": "文档解析完成" }),
        );
        let tool = crate::test_engineer::GenerateTestCasesTool::new(app);
        tool.run(json!({
            "requirement": req,
            "case_types": case_types.unwrap_or_default(),
            "per_type_count": per_type_count.unwrap_or(0),
        }))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 导出测试用例到本地文件（json / csv / xlsx）：弹出另存对话框，用户取消返回 None
#[tauri::command]
pub async fn test_export_cases(
    cases: Value,
    format: String,
    title: String,
) -> Result<Option<String>, String> {
    tokio::task::spawn_blocking(move || -> Result<Option<String>, String> {
        let arr = cases.as_array().cloned().unwrap_or_default();
        if arr.is_empty() {
            return Err("没有可导出的用例".into());
        }
        let safe_title = if title.trim().is_empty() {
            "测试用例".to_string()
        } else {
            title.trim().to_string()
        };
        let (ext, desc): (&str, &str) = match format.as_str() {
            "csv" => ("csv", "CSV 文件（逗号分隔）"),
            "xlsx" => ("xlsx", "Excel 工作簿"),
            _ => ("json", "JSON 文件"),
        };
        let path = rfd::FileDialog::new()
            .set_file_name(&format!("{}.{ext}", safe_title))
            .add_filter(desc, &[ext])
            .save_file();
        match path {
            Some(p) => {
                crate::test_engineer::write_cases_file(&arr, &format, &p)?;
                Ok(Some(p.to_string_lossy().to_string()))
            }
            None => Ok(None),
        }
    })
    .await
    .map_err(|e| format!("导出失败: {e}"))?
}

/// 执行 UI 自动化测试（面板直连；含点击/输入等写操作，放阻塞线程跑）
#[tauri::command]
pub async fn test_run_ui(
    app: AppHandle,
    state: State<'_, AppState>,
    name: String,
    steps: Value,
) -> Result<Value, String> {
    let cap = state.capability.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let tool = crate::test_engineer::RunUiTestTool::new(cap, app);
        tool.run(json!({ "name": name, "steps": steps }))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 执行接口自动化测试（面板直连；HTTP 请求 + 断言）
#[tauri::command]
pub async fn test_run_api(app: AppHandle, name: String, requests: Value) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let tool = crate::test_engineer::RunApiTestTool::new(app);
        tool.run(json!({ "name": name, "requests": requests }))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 勾选用例批量执行：脚本化 → 自动执行（UI/接口）→ 综合报告（面板直连）
/// `project` 为被测对象台账（可选），传入后触发：环境隔离硬门 + 执行记录落盘。
#[tauri::command]
pub async fn test_run_selected(
    app: AppHandle,
    state: State<'_, AppState>,
    name: String,
    cases: Value,
    project: Option<Value>,
) -> Result<Value, String> {
    let cap = state.capability.clone();
    let project_profile = project.and_then(|p| serde_json::from_value(p).ok());
    tauri::async_runtime::spawn_blocking(move || {
        let arr = cases.as_array().cloned().ok_or("缺少参数 cases")?;
        crate::test_engineer::run_selected_cases(
            &app,
            cap.as_ref(),
            &name,
            &arr,
            project_profile.as_ref(),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------------- 被测对象台账（项目基线） ----------------

/// 加载被测对象台账（项目档案库），空则返回空数组
#[tauri::command]
pub fn test_load_projects(state: State<'_, AppState>) -> Result<Value, String> {
    let list = crate::test_engineer::load_projects(&state.store);
    Ok(json!(list))
}

/// 保存（新增或按 id 更新）一个被测项目档案，返回保存后的档案列表
#[tauri::command]
pub fn test_save_project(state: State<'_, AppState>, profile: Value) -> Result<Value, String> {
    let p: crate::test_engineer::ProjectProfile =
        serde_json::from_value(profile).map_err(|e| format!("项目档案参数解析失败: {e}"))?;
    let list = crate::test_engineer::save_project(&state.store, p)?;
    Ok(json!(list))
}

/// 按 id 删除一个被测项目档案，返回删除后的档案列表
#[tauri::command]
pub fn test_delete_project(state: State<'_, AppState>, id: String) -> Result<Value, String> {
    let list = crate::test_engineer::delete_project(&state.store, &id)?;
    Ok(json!(list))
}

/// 自动识别被测项目：从需求文档 +（可选）代码目录推断项目形态与地址（面板直连）
#[tauri::command]
pub async fn test_auto_detect_project(
    app: AppHandle,
    requirement: String,
    repo_or_path: Option<String>,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let profile = crate::test_engineer::auto_detect_project(
            &app,
            &requirement,
            repo_or_path.as_deref(),
        )?;
        Ok(json!(profile))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 环境准备：就绪方式=boot 时后台启动被测应用（面板直连）
#[tauri::command]
pub fn test_prepare_env(run_command: String) -> Result<Value, String> {
    let detail = crate::test_engineer::run_prepare_env(&run_command)?;
    Ok(json!({ "ok": true, "detail": detail }))
}

/// 从 openapi/swagger 文档（URL 或本地路径）直出接口用例（面板直连）
/// `api_base` 为空时优先使用文档内的 servers[0].url / host+basePath。
#[tauri::command]
pub async fn test_import_openapi(
    doc: String,
    api_base: Option<String>,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let cases = crate::test_engineer::import_openapi(&doc, api_base.as_deref().unwrap_or(""))?;
        Ok(json!({ "count": cases.len(), "cases": cases }))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 列出某被测项目的执行记录（md/html 成对），按时间倒序（面板直连）
#[tauri::command]
pub fn test_list_records(
    app: AppHandle,
    project_name: String,
    project_id: String,
    report_dir: Option<String>,
) -> Result<Value, String> {
    let list = crate::test_engineer::list_execution_records(
        &app,
        &project_name,
        &project_id,
        report_dir.as_deref(),
    )?;
    Ok(json!(list))
}

/// 读取某被测项目的执行趋势（每次自动执行入库一条：ts/total/passed/failed/rate），按时间正序
#[tauri::command]
pub fn test_trend_get(state: State<'_, AppState>, project_id: String) -> Result<Value, String> {
    if project_id.is_empty() {
        return Ok(json!([]));
    }
    let key = format!("test_trend:{}", project_id);
    let list: Value = state
        .store
        .get_setting(&key)
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!([]));
    Ok(list)
}

/// 视觉回归对比：基线截图 vs 当前截图 像素级 diff（面板直连）
/// 图片解码与逐像素比对是 CPU 密集操作，spawn_blocking 避免卡主线程。
#[tauri::command]
pub async fn visual_diff(
    baseline: String,
    current: String,
    tolerance: Option<u8>,
    threshold: Option<f64>,
    save_dir: Option<String>,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::visual_diff::run_diff(
            &baseline,
            &current,
            tolerance.unwrap_or(24),
            threshold.unwrap_or(0.02),
            save_dir.as_deref(),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------------- 内置终端 ----------------

/// 打开内置终端窗口（若未启动则启动 PTY 会话）
#[tauri::command]
pub async fn open_terminal_window(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let terminal = state.terminal.clone();
    crate::windows::ensure_terminal_window(&app, terminal.clone());
    if !terminal.is_running() {
        let t = terminal.clone();
        let a = app.clone();
        // PTY 创建 / 启动 shell 可能阻塞，放到阻塞线程池，避免卡住界面
        let res = tauri::async_runtime::spawn_blocking(move || t.spawn(a))
            .await
            .map_err(|e| e.to_string())?;
        res?;
    }
    Ok(())
}

/// 打开内置终端并自动执行一条命令（执行流「终端查看」入口）：
/// 确保窗口与会话就绪后，等待前端 xterm 挂载订阅输出，再把命令敲进 PTY，
/// 用户可实时看到命令的回显与执行过程（区别于 ps_exec 的「跑完给结果」）
#[tauri::command]
pub async fn open_terminal_with_command(
    app: AppHandle,
    state: State<'_, AppState>,
    command: String,
) -> Result<(), String> {
    let command = command.trim().to_string();
    if command.is_empty() {
        return Err("命令为空".into());
    }
    let terminal = state.terminal.clone();
    crate::windows::ensure_terminal_window(&app, terminal.clone());
    if !terminal.is_running() {
        let t = terminal.clone();
        let a = app.clone();
        let res = tauri::async_runtime::spawn_blocking(move || t.spawn(a))
            .await
            .map_err(|e| e.to_string())?;
        res?;
        // 新拉起的窗口需要加载前端并订阅 onTermData，留出挂载时间
        tokio::time::sleep(std::time::Duration::from_millis(900)).await;
    }
    // 把命令敲进 PTY（\r 触发执行，回显与输出经 onTermData 流回终端窗口）
    terminal
        .write(&format!("{command}\r"))
        .map_err(|e| format!("写入终端失败: {e}"))?;
    Ok(())
}

/// 启动/复用终端会话（前端终端窗口挂载时调用）
#[tauri::command]
pub async fn term_spawn(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let terminal = state.terminal.clone();
    tauri::async_runtime::spawn_blocking(move || terminal.spawn(app))
        .await
        .map_err(|e| e.to_string())?
}

/// 向前端终端写入输入（用户按键回送）
#[tauri::command]
pub fn term_write(state: State<'_, AppState>, data: String) -> Result<(), String> {
    state.terminal.write(&data)
}

/// 调整终端行列尺寸
#[tauri::command]
pub fn term_resize(state: State<'_, AppState>, rows: u16, cols: u16) -> Result<(), String> {
    state.terminal.resize(rows, cols)
}

/// 结束终端会话（关闭窗口 / 清理子进程）
#[tauri::command]
pub fn term_close(state: State<'_, AppState>) -> Result<(), String> {
    state.terminal.close();
    Ok(())
}

/// 附件（文档/图片）抽取 + 脱敏，附加到用户消息之后作为本轮上下文（只影响 Agent 输入，不落库不背记）
fn enrich_with_attachments(message: &str, attachments: &[String]) -> String {
    let mut out = message.to_string();
    for path in attachments {
        let name = std::path::Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.clone());

        // 图片：先本地 OCR 提取文字；无文字时退化为视觉模型描述。
        // 无论识别成功与否都注入完整路径，供白泽直接用 image_describe 读取，避免按文件名到文件系统里盲搜。
        if is_image(path) {
            let mut recognized: Option<String> = None;
            let mut fail_reason = String::new();
            match crate::ocr::ocr_text(path, "chi_sim+eng") {
                Ok(text) if !text.trim().is_empty() => {
                    recognized = Some(format!("识别文字：\n{text}"));
                }
                _ => match crate::visual_grounding::describe_image(path, message) {
                    Ok(desc) if !desc.trim().is_empty() => {
                        recognized = Some(format!("图片内容：\n{desc}"));
                    }
                    Ok(_) => fail_reason = "OCR 无文字、视觉模型返回空描述".to_string(),
                    Err(e) => fail_reason = e,
                },
            }

            match recognized {
                Some(body) => out.push_str(&format!(
                    "\n\n【附件图片：{name}】\n完整路径：{path}\n{body}"
                )),
                None => out.push_str(&format!(
                    "\n\n【附件图片：{name}】\n完整路径：{path}\n\
                     （自动识别未成功：{fail_reason}。若需要识别这张图，请调用 image_describe 工具并传入上面的完整路径，\
                     不要再按文件名到文件系统里搜索）"
                )),
            }
            continue;
        }

        // 富解析：pdf/docx/xlsx/pptx 走 read_document（含表格抽取），纯文本走内置抽取
        let text = if rich_parse_kind(path).is_some() {
            match crate::read_document::run(json!({
                "path": path,
                "extract_images": false,
                "export_csv": false,
            })) {
                Ok(resp) => render_read_document(resp),
                Err(_) => crate::document::extract_text(path).unwrap_or_default(),
            }
        } else {
            crate::document::extract_text(path).unwrap_or_default()
        };
        let (redacted, sensitive) = crate::document::redact_secrets(&text);
        let mut header = format!("\n\n【附件文档：{name}】");
        if !sensitive.is_empty() {
            header.push_str(&format!("（已脱敏 {} 处）", sensitive.len()));
        }
        header.push('\n');
        out.push_str(&header);
        out.push_str(&redacted);
    }
    out
}

/// 需要走富解析（Python sidecar）的办公文档扩展名
fn rich_parse_kind(path: &str) -> Option<&'static str> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "pdf" => Some("pdf"),
        "docx" => Some("docx"),
        "xlsx" | "xls" => Some("xlsx"),
        "pptx" => Some("pptx"),
        _ => None,
    }
}

/// 把 read_document 的响应渲染成「正文 + 表格」的纯文本串，供注入对话上下文
fn render_read_document(resp: Value) -> String {
    let mut text = String::new();
    let Some(files) = resp.get("files").and_then(|v| v.as_array()) else {
        return text;
    };
    for f in files {
        if let Some(t) = f.get("text").and_then(|v| v.as_str()) {
            if !t.trim().is_empty() {
                text.push_str(t);
                text.push('\n');
            }
        }
        if let Some(tables) = f.get("tables").and_then(|v| v.as_array()) {
            for (ti, tbl) in tables.iter().enumerate() {
                text.push_str(&format!("\n【表格 {}】\n", ti + 1));
                if let Some(cols) = tbl.get("columns").and_then(|v| v.as_array()) {
                    let header = cols
                        .iter()
                        .flat_map(|c| c.as_str())
                        .collect::<Vec<_>>()
                        .join(" | ");
                    if !header.is_empty() {
                        text.push_str(&header);
                        text.push('\n');
                    }
                }
                if let Some(rows) = tbl.get("rows").and_then(|v| v.as_array()) {
                    for r in rows {
                        if let Some(arr) = r.as_array() {
                            let line = arr
                                .iter()
                                .flat_map(|c| c.as_str())
                                .collect::<Vec<_>>()
                                .join(" | ");
                            text.push_str(&line);
                            text.push('\n');
                        }
                    }
                }
            }
        }
    }
    text
}

/// 判断路径是否为常见图片格式（png/jpg/jpeg/bmp/webp/gif）
fn is_image(path: &str) -> bool {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "bmp" | "webp" | "gif")
}

// ---------------- 语义固化（模型抽取事实） ----------------

struct Fact {
    kind: String,
    content: String,
}

/// 语义固化：用模型把用户消息抽取为结构化事实，存入语义记忆
async fn consolidate(state: &AppState, message: &str) {
    let prompt = format!(
        "从下面这句话提取值得长期记住的信息（偏好、事实、项目、习惯、关系等）。\
         输出 JSON 数组，每项格式 {{\"type\":\"偏好|事实|项目|习惯|关系\",\"content\":\"简洁描述\"}}。\
         没有值得记的输出 []。只输出 JSON，不要任何解释：\n\n{message}"
    );
    let msgs = vec![ChatMessage {
        role: "user".into(),
        content: prompt,
        tool_calls: None,
        tool_call_id: None,
    }];
    let Ok(resp) = state.model.chat(&msgs, &[]).await else {
        return;
    };
    let Some(text) = resp.content else { return };
    let Ok(facts) = parse_facts(&text) else { return };
    for f in facts {
        if f.content.chars().count() >= 2 {
            let _ = state.store.smart_remember(&f.content, &f.kind);
        }
    }
}

/// 从模型输出中解析 JSON 数组（容忍模型加的前后缀文字）
fn parse_facts(text: &str) -> Result<Vec<Fact>, String> {
    let start = text.find('[').ok_or("无 JSON 数组")?;
    let end = text.rfind(']').ok_or("无 JSON 数组")?;
    let json = &text[start..=end];
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let arr = v.as_array().ok_or("非数组")?;
    let mut out = Vec::new();
    for item in arr {
        let kind = item.get("type").and_then(|t| t.as_str()).unwrap_or("fact").to_string();
        let content = item
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();
        if !content.is_empty() {
            out.push(Fact { kind, content });
        }
    }
    Ok(out)
}

/// 记忆↔执行闭环：从用户消息里探测「X 分钟后 / X 小时后」这类明确的未来时间，
/// 命中则返回 (延后秒数, 提示文案)，供 chat 流程自动登记一次性提醒。
fn detect_reminder_delay(text: &str) -> Option<(u64, String)> {
    if let Some(pos) = text.find("分钟后") {
        if let Some(n) = parse_num_before(text, pos) {
            if n > 0 {
                return Some((n.saturating_mul(60), format!("{} 分钟后提醒", n)));
            }
        }
    }
    if let Some(pos) = text.find("小时后") {
        if let Some(n) = parse_num_before(text, pos) {
            if n > 0 {
                return Some((n.saturating_mul(3600), format!("{} 小时后提醒", n)));
            }
        }
    }
    None
}

/// 取 `pos` 之前紧邻的整数（容忍空格），用于解析「30 分钟后」里的 30。
fn parse_num_before(text: &str, pos: usize) -> Option<u64> {
    let bytes = text.as_bytes();
    let mut i = pos;
    while i > 0 && (bytes[i - 1].is_ascii_digit() || bytes[i - 1] == b' ') {
        i -= 1;
    }
    let s: String = text[i..pos].chars().filter(|c| c.is_ascii_digit()).collect();
    s.parse::<u64>().ok()
}

// ---------------- 定时任务编排（前端管理面板） ----------------

/// 列出全部定时任务
#[tauri::command]
pub fn schedule_list_jobs(state: State<'_, AppState>) -> Vec<crate::scheduler::ScheduledJob> {
    state.scheduler.list_jobs()
}

/// 新增定时任务
#[tauri::command]
pub fn schedule_add_job(
    state: State<'_, AppState>,
    cron_expr: String,
    title: String,
    task_type: String,
    task: String,
) -> Result<crate::scheduler::ScheduledJob, String> {
    state
        .scheduler
        .add_job_full(&cron_expr, &title, &task_type, &task)
}

/// 编辑定时任务
#[tauri::command]
pub fn schedule_update_job(
    state: State<'_, AppState>,
    id: String,
    cron_expr: String,
    title: String,
    task_type: String,
    task: String,
) -> Result<crate::scheduler::ScheduledJob, String> {
    state
        .scheduler
        .update_job(&id, &cron_expr, &title, &task_type, &task)
}

/// 删除定时任务（含执行日志）
#[tauri::command]
pub fn schedule_delete_job(state: State<'_, AppState>, id: String) -> Result<bool, String> {
    let removed = state.scheduler.cancel_job(&id)?;
    if removed {
        let _ = state.scheduler.clear_runs(&id);
    }
    Ok(removed)
}

/// 暂停 / 恢复定时任务
#[tauri::command]
pub fn schedule_set_enabled(
    state: State<'_, AppState>,
    id: String,
    enabled: bool,
) -> Result<bool, String> {
    state.scheduler.set_enabled(&id, enabled)
}

/// 查询执行日志（job_id 空查全部）
#[tauri::command]
pub fn schedule_job_logs(
    state: State<'_, AppState>,
    job_id: String,
    limit: Option<usize>,
) -> Vec<crate::scheduler::JobRun> {
    state.scheduler.list_runs(&job_id, limit.unwrap_or(20))
}

/// 清空某任务的执行日志
#[tauri::command]
pub fn schedule_clear_logs(state: State<'_, AppState>, job_id: String) -> Result<usize, String> {
    state.scheduler.clear_runs(&job_id)
}
