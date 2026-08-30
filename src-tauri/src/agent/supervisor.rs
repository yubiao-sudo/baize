//! 多 agent 监督者：任务拆解（todo 规划）→ Executor（AgentLoop 按 todo 执行）→ Critic（可选审查）
//!
//! 任务拆解结果通过「todo-list / todo-update」事件推给前端流程面板；
//! 执行过程中模型可用 todo_update 工具自主维护步骤状态。

use std::sync::atomic::Ordering;

use serde_json::json;
use tauri::{AppHandle, Emitter};

use crate::model::ChatMessage;
use crate::task::Todo;
use crate::AppState;

use super::runtime::cancellable_chat_with_tier;
use super::AgentLoop;
use crate::model::ModelTier;

/// 是否启用「审查」额外模型调用（默认关闭）
const REFLECT: bool = false;

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

    /// 监督执行：拆解 todo → 执行 → 标记完成 → （可选）审查
    pub async fn run(&self, message: &str, history: Vec<ChatMessage>) -> Result<String, String> {
        if self.state.cancel.load(Ordering::SeqCst) {
            return Ok("已停止。".to_string());
        }

        // 1) 任务拆解 → todo 列表（仅对疑似多步骤任务拆解；简单问答直接跳过）
        //    拆解与回答并行执行：拆解不再阻塞回答的首个 token
        let plan_enabled = needs_planning(message);
        let plan_fut = async {
            if !plan_enabled {
                return;
            }
            let _ = self.app.emit(
                "thought",
                json!({ "kind": "thinking", "label": "拆解任务", "detail": "正在分析任务步骤…" }),
            );
            self.state.log_thought("thinking", "拆解任务", "正在分析任务步骤…");
            let mut todos = self.plan_todos(message).await;
            if todos.len() >= 2 {
                let _ = self.app.emit(
                    "thought",
                    json!({
                        "kind": "plan",
                        "label": format!("拆解为 {} 步", todos.len()),
                        "detail": todos.iter().map(|t| t.title.as_str()).collect::<Vec<_>>().join(" → "),
                    }),
                );
                self.state.log_thought(
                    "plan",
                    &format!("拆解为 {} 步", todos.len()),
                    &todos.iter().map(|t| t.title.as_str()).collect::<Vec<_>>().join(" → "),
                );
                if let Some(first) = todos.first_mut() {
                    first.status = "in_progress".to_string();
                }
                *self.state.todos.lock().unwrap() = todos.clone();
                crate::task::save_task_checkpoint(&self.state.store, &todos);
                crate::task::emit_todo_list(self.app, &todos);
            }
        };

        // 2) Executor：AgentLoop 执行（与拆解并行；模型可用 todo_update 工具更新进度）
        let executor = AgentLoop::new(self.app, self.state).with_project(self.project.clone());
        let (_, answer) = tokio::join!(
            plan_fut,
            executor.run(message, history),
        );
        let answer = answer?;

        // 3) 完成：把所有步骤标为 completed（若已拆解出步骤）
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

        // 4) Critic：自我审查（可选，默认关闭）
        if REFLECT {
            if self.state.cancel.load(Ordering::SeqCst) {
                return Ok("已停止。".to_string());
            }
            self.review(message, &answer).await;
        }

        Ok(answer)
    }

    /// 让模型把任务拆解成步骤列表（简单问答返回空数组）
    async fn plan_todos(&self, message: &str) -> Vec<Todo> {
        let prompt = format!(
            "把下面的用户请求拆解成 2-6 个可执行的步骤。\
             注意：写文章、写报告、写文档、写总结、做调研、整理资料、分析问题等都属于多步骤任务，必须拆解成多个步骤，不要返回空数组。\
             只有真正的简单问题（一句话就能直接回答、无需任何工具操作）才返回空数组 []。\
             只输出 JSON 数组，每项格式 {{\"title\":\"步骤描述\"}}，不要任何解释：\n\n{message}"
        );
        let msgs = vec![ChatMessage {
            role: "user".into(),
            content: prompt,
            tool_calls: None,
            tool_call_id: None,
        }];
        // 规划用云端强模型（无云端时回退到默认路由）
        if let Ok(resp) = cancellable_chat_with_tier(self.state, ModelTier::Cloud, &msgs, &[]).await {
            if let Some(text) = resp.content {
                if let Ok(todos) = crate::task::parse_todos(&text) {
                    return todos;
                }
            }
        }
        Vec::new()
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

/// 判断是否需要「任务拆解」这一步。
/// 拆解会额外触发一次完整模型调用（非流式、优先云端/回退本地），是每轮对话延迟的主要来源。
/// 仅对「疑似多步骤」的请求拆解：含明确动作信号或文本较长；简单短问句直接跳过以避免空耗一次模型调用。
fn needs_planning(message: &str) -> bool {
    let m = message.trim();
    if m.is_empty() {
        return false;
    }
    const TRIGGERS: &[&str] = &[
        "写", "生成", "创建", "分析", "调研", "整理", "下载", "搜索", "列出", "对比",
        "总结", "报告", "文档", "测试", "安装", "运行", "修复", "实现", "设计", "转换",
        "构建", "部署", "配置", "开发", "优化", "重构", "批量", "步骤", "教程", "文章",
        "代码", "编译", "脚本", "执行", "规划", "安排", "爬取", "采集", "同步", "备份",
        "迁移", "清理", "摘要",
        "按住", "选中", "框选", "拖拽", "点击", "鼠标", "快捷键", "截屏", "屏幕",
    ];
    if TRIGGERS.iter().any(|k| m.contains(k)) {
        return true;
    }
    // 较长文本通常蕴含多步骤需求，保留拆解
    m.chars().count() > 24
}
