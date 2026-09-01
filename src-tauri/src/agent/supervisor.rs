//! 多 agent 监督者：任务拆解（todo 规划）→ Executor（AgentLoop 按 todo 执行）→ Critic（可选审查）
//!
//! 任务拆解结果通过「todo-list / todo-update」事件推给前端流程面板；
//! 执行过程中模型可用 todo_update 工具自主维护步骤状态。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

use serde_json::json;
use tauri::{AppHandle, Emitter};

use crate::model::ChatMessage;
use crate::AppState;

use super::runtime::cancellable_chat_with_tier;
use super::AgentLoop;
use crate::model::ModelTier;

/// 是否启用「审查」额外模型调用（默认关闭）
const REFLECT: bool = false;

// ─────────────── 任务单飞门闩（任务队列） ───────────────
// 所有入口（聊天/微信/飞书/定时/后台/看门狗/自测）都经 Supervisor::run 进入，
// 在这里统一排队串行执行，防止并发 Agent 实例互踩 cancel/todos/关键帧/卡片等共享槽位。
// tokio::sync::Mutex 是 FIFO 公平锁：先到先执行，后到自动排队。
static AGENT_GATE: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
/// 正在排队等待的任务数（不含正在执行的）
static QUEUE_WAITING: AtomicUsize = AtomicUsize::new(0);

pub struct Supervisor<'a> {
    app: &'a AppHandle,
    state: &'a AppState,
    /// 会话所属项目（侧边栏「项目」导航）：透传给 AgentLoop 注入系统提示词
    project: Option<crate::memory::ProjectRow>,
}

impl<'a> Supervisor<'a> {
    pub fn new(app: &'a AppHandle, state: &'a AppState) -> Self {
        Self {
            app,
            state,
            project: None,
        }
    }

    /// 附带会话所属项目（chat 命令查库后传入）
    pub fn with_project(mut self, project: Option<crate::memory::ProjectRow>) -> Self {
        self.project = project;
        self
    }

    /// 监督执行：回答 → 标记完成 → （可选）审查。
    /// 预拆解已移除：多步任务的计划改由模型在回答过程中用 plan_confirm 提交，
    /// 用户在聊天卡片/消息中心/IM 上确认后再执行——首响应不再等任何拆解调用
    pub async fn run(&self, message: &str, history: Vec<ChatMessage>) -> Result<String, String> {
        if self.state.cancel.load(Ordering::SeqCst) {
            return Ok("已停止。".to_string());
        }

        // 任务排队：忙时自动入队（推「任务排队」thought 让用户可见），拿到门闩后串行执行
        let waiting = QUEUE_WAITING.fetch_add(1, Ordering::SeqCst);
        if waiting > 0 {
            let brief: String = message.trim().chars().take(24).collect();
            let detail = format!("「{brief}…」等待当前任务完成（前面还有 {waiting} 个任务）");
            let _ = self.app.emit(
                "thought",
                json!({ "kind": "queue", "label": "任务排队", "detail": detail }),
            );
            self.state.log_thought("queue", "任务排队", &format!("前面还有 {waiting} 个任务"));
        }
        let _gate = AGENT_GATE.get_or_init(|| tokio::sync::Mutex::new(())).lock().await;
        QUEUE_WAITING.fetch_sub(1, Ordering::SeqCst);
        // 复位上一任务可能残留的取消脏标志：用户停止的是上一个任务，排队中的本任务不应被误吞
        // （这也是「定时任务被残留标志吞掉」问题的根治：复位点统一收敛到拿锁之后）
        self.state.cancel.store(false, Ordering::SeqCst);
        crate::tools::clear_global_cancel();

        // Executor：AgentLoop 直接回答/执行（模型可用 todo_update 维护 plan_confirm 注册的步骤进度）
        let executor = AgentLoop::new(self.app, self.state).with_project(self.project.clone());
        let answer = executor.run(message, history).await?;

        // 完成：把所有步骤标为 completed（若 plan_confirm 注册过步骤）
        {
            let mut t = self.state.todos.lock().unwrap();
            if !t.is_empty() {
                for item in t.iter_mut() {
                    item.status = "completed".to_string();
                }
                let snapshot = t.clone();
                drop(t);
                crate::task::save_task_checkpoint(&self.state.store, &[]);
                crate::task::emit_todo_update(self.app, &snapshot);
            }
        }

        // Critic：自我审查（可选，默认关闭）
        if REFLECT {
            if self.state.cancel.load(Ordering::SeqCst) {
                return Ok("已停止。".to_string());
            }
            self.review(message, &answer).await;
        }

        Ok(answer)
    }

    async fn review(&self, message: &str, answer: &str) {

        let prompt = format!(
            "请审查下面的助手回复是否准确、完整、有帮助。\
             如果没有问题，只回复 OK；如果有问题，用一句话简要指出，不要重复原回复：\n\n\
             用户请求：{message}\n助手回复：{answer}"
        );
        let msgs = vec![ChatMessage {
            role: "user".into(),
            content: prompt,
            tool_calls: None,
            tool_call_id: None,
        }];
        // 审查用云端强模型（无云端时回退到默认路由）
        if let Ok(resp) = cancellable_chat_with_tier(self.state, ModelTier::Cloud, &msgs, &[]).await {
            if let Some(text) = resp.content {
                let verdict = text.trim();
                if !verdict.is_empty() && !verdict.eq_ignore_ascii_case("ok") {
                    let _ = self.app.emit(
                        "thought",
                        json!({ "kind": "critic", "label": "审查意见", "detail": verdict }),
                    );
                    self.state.log_thought("critic", "审查意见", &verdict);
                }
            }
        }
    }
}
