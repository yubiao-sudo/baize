//! 可编排工作流：用户可配置的「阶段流水线」。
//!
//! 一个工作流由若干阶段组成，每个阶段是一段提示词模板（`{input}` 会被替换为上一阶段输出），
//! 逐阶段用强模型执行，前一阶段的输出作为下一阶段的输入，最终渲染到右侧文档窗口。
//!
//! 与「工作模式」的关系：工作流是可复用、可注册、可自省的任务流水线；内置若干样例，
//! 用户可通过 `add_workflow` 命令（或让白泽代劳）注册自定义流水线。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::memory::MemoryStore;
use crate::model::{ChatMessage, ModelTier};
use crate::tools::{PermissionClass, Tool};
use crate::AppState;

// ───────────────────── 数据结构 ─────────────────────

/// 工作流阶段（提示词模板，`{input}` 为上一阶段输出）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowStage {
    pub name: String,
    pub prompt: String,
}

/// 工作流定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workflow {
    pub id: String,
    pub name: String,
    pub description: String,
    pub stages: Vec<WorkflowStage>,
}

/// 工作流单次执行日志
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRun {
    pub id: String,
    pub workflow_id: String,
    pub workflow_name: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    /// "running" | "success" | "failed"
    pub status: String,
    pub result: String,
}

// ───────────────────── 内置工作流 ─────────────────────

fn builtin_workflows() -> Vec<Workflow> {
    vec![
        Workflow {
            id: "summary_report".into(),
            name: "总结报告".into(),
            description: "把输入内容整理成结构化 Markdown 报告".into(),
            stages: vec![WorkflowStage {
                name: "整理总结".into(),
                prompt: "你是文档整理助手。请把下面的内容整理成一份结构清晰的中文 Markdown 报告，包含：概述、要点、结论。原始内容：\n{input}".into(),
            }],
        },
        Workflow {
            id: "write_spec".into(),
            name: "写设计文档".into(),
            description: "两阶段：先做需求分析，再输出设计文档".into(),
            stages: vec![
                WorkflowStage {
                    name: "需求分析".into(),
                    prompt: "你是需求分析师。请从下面的内容中抽取结构化需求点（功能、输入、输出、业务规则、验收标准），只输出 JSON 数组。\n{input}".into(),
                },
                WorkflowStage {
                    name: "设计文档".into(),
                    prompt: "你是软件设计工程师。请基于下面的需求分析结果，输出一份 Markdown 设计文档（含架构、模块、接口、数据模型）。\n{input}".into(),
                },
            ],
        },
    ]
}

// ───────────────────── 注册表 ─────────────────────

/// 持久化工作流注册表：内置样例 + 用户自定义（存 SQLite），并提供执行日志。
pub struct WorkflowRegistry {
    store: Arc<MemoryStore>,
}

impl WorkflowRegistry {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self { store }
    }

    /// 用户自定义工作流（从持久化恢复）
    fn persisted(&self) -> Vec<Workflow> {
        let rows = match self.store.list_workflows() {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[工作流] 加载失败: {e}");
                return Vec::new();
            }
        };
        rows.into_iter()
            .filter_map(|(_, data)| serde_json::from_str::<Workflow>(&data).ok())
            .collect()
    }

    /// 内置 + 用户自定义
    pub fn all(&self) -> Vec<Workflow> {
        let mut v = builtin_workflows();
        v.extend(self.persisted());
        v
    }

    pub fn find(&self, id: &str) -> Option<Workflow> {
        self.all().into_iter().find(|w| w.id == id)
    }

    /// 创建或更新（同名 id 覆盖）
    pub fn save(&self, wf: Workflow) -> Result<(), String> {
        if wf.id.trim().is_empty() {
            return Err("工作流缺少 id".into());
        }
        if wf.stages.is_empty() {
            return Err("工作流至少需要一个阶段".into());
        }
        let data = serde_json::to_string(&wf).map_err(|e| e.to_string())?;
        self.store.upsert_workflow(&wf.id, &data)
    }

    pub fn delete(&self, id: &str) -> Result<bool, String> {
        self.store.delete_workflow(id)
    }

    /// 执行日志（workflow_id 传空串查全部）
    pub fn runs(&self, workflow_id: &str, limit: usize) -> Vec<WorkflowRun> {
        let limit = limit.clamp(1, 500);
        let rows = match self.store.list_workflow_runs(workflow_id, limit) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[工作流] 读取执行日志失败: {e}");
                return Vec::new();
            }
        };
        rows.into_iter()
            .filter_map(|(_, _, _, data)| serde_json::from_str::<WorkflowRun>(&data).ok())
            .collect()
    }

    pub fn clear_runs(&self, workflow_id: &str) -> Result<usize, String> {
        self.store.clear_workflow_runs(workflow_id)
    }

    fn persist_run(&self, run: &WorkflowRun) -> Result<(), String> {
        let data = serde_json::to_string(run).map_err(|e| e.to_string())?;
        self.store
            .upsert_workflow_run(&run.id, &run.workflow_id, run.started_at, &data)
    }
}

// ───────────────────── 执行器 ─────────────────────

/// 工作流执行器（与 `TestCasePipeline` 同构，持 AppHandle + AppState）
pub struct WorkflowRunner<'a> {
    app: &'a AppHandle,
    state: &'a AppState,
}

impl<'a> WorkflowRunner<'a> {
    pub fn new(app: &'a AppHandle, state: &'a AppState) -> Self {
        Self { app, state }
    }

    pub async fn run(&self, wf: &Workflow, input: &str) -> Result<String, String> {
        let total = wf.stages.len();
        let mut current = input.to_string();
        for (i, stage) in wf.stages.iter().enumerate() {
            let _ = self.app.emit(
                "thought",
                json!({
                    "kind": "workflow",
                    "label": format!("工作流「{}」· {}/{} {}", wf.name, i + 1, total, stage.name),
                    "detail": "执行中…"
                }),
            );
            let prompt = stage.prompt.replace("{input}", &current);
            current = self.call_model(&prompt).await?;
        }
        Ok(current)
    }

    /// 单轮无工具调用，走云端强模型（同 `TestCasePipeline` 链路）
    async fn call_model(&self, prompt: &str) -> Result<String, String> {
        let msgs = vec![ChatMessage {
            role: "user".into(),
            content: prompt.to_string(),
            tool_calls: None,
            tool_call_id: None,
        }];
        let resp = self
            .state
            .model
            .chat_with_tier(ModelTier::Cloud, &msgs, &[])
            .await?;
        Ok(resp.content.unwrap_or_default())
    }
}

/// 把工作流产出渲染到右侧文档窗口（新建标签页，不覆盖已有文档）
fn render_output(app: &AppHandle, state: &AppState, title: &str, content: &str) {
    crate::markdown::write_document(app, &state.markdown, title, content);
}

/// 执行工作流并写执行日志（running → success/failed）
async fn run_tracked(
    app: &AppHandle,
    state: &AppState,
    wf: &Workflow,
    input: &str,
) -> Result<String, String> {
    let runner = WorkflowRunner::new(app, state);
    let registry = state.workflows.clone();
    let run_id = uuid::Uuid::new_v4().to_string();
    let started = now_ms();
    let _ = registry.persist_run(&WorkflowRun {
        id: run_id.clone(),
        workflow_id: wf.id.clone(),
        workflow_name: wf.name.clone(),
        started_at: started,
        finished_at: None,
        status: "running".into(),
        result: String::new(),
    });

    match runner.run(wf, input).await {
        Ok(out) => {
            let finished = now_ms();
            let _ = registry.persist_run(&WorkflowRun {
                id: run_id,
                workflow_id: wf.id.clone(),
                workflow_name: wf.name.clone(),
                started_at: started,
                finished_at: Some(finished),
                status: "success".into(),
                result: truncate(out.clone(), 4000),
            });
            Ok(out)
        }
        Err(e) => {
            let finished = now_ms();
            let _ = registry.persist_run(&WorkflowRun {
                id: run_id,
                workflow_id: wf.id.clone(),
                workflow_name: wf.name.clone(),
                started_at: started,
                finished_at: Some(finished),
                status: "failed".into(),
                result: e.clone(),
            });
            Err(e)
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn truncate(s: String, max: usize) -> String {
    if s.chars().count() > max {
        let t: String = s.chars().take(max).collect();
        format!("{t}\n...(已截断)")
    } else {
        s
    }
}

// ───────────────────── 工具：run_workflow ─────────────────────

/// 让白泽按 id 执行某个工作流（多阶段提示词链），结果写入右侧文档窗口。
pub struct RunWorkflowTool {
    app: AppHandle,
}

impl RunWorkflowTool {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl Tool for RunWorkflowTool {
    fn name(&self) -> &str {
        "run_workflow"
    }
    fn description(&self) -> &str {
        "按 id 执行一个可编排工作流（多阶段流水线），结果写入右侧文档窗口。可用 list_workflows 查询可用工作流"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "工作流 id（如 summary_report / write_spec）" },
                "input": { "type": "string", "description": "输入内容：文本或文档路径" }
            },
            "required": ["id", "input"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let id = args["id"].as_str().ok_or("缺少参数 id")?;
        let input = args["input"].as_str().ok_or("缺少参数 input")?;
        let app = self.app.clone();

        tauri::async_runtime::block_on(async move {
            let state = app.state::<AppState>();
            let wf = state
                .inner()
                .workflows
                .find(id)
                .ok_or_else(|| format!("未找到工作流: {id}"))?;
            let output = run_tracked(&app, state.inner(), &wf, input).await?;
            render_output(&app, state.inner(), &format!("工作流 · {}", wf.name), &output);
            Ok(json!({
                "ok": true,
                "id": id,
                "name": wf.name,
                "stages": wf.stages.len(),
                "chars": output.chars().count(),
            }))
        })
    }
}

// ───────────────────── 命令 ─────────────────────

/// 列出全部工作流（内置 + 用户自定义）
#[tauri::command]
pub fn list_workflows(state: State<'_, AppState>) -> Vec<Workflow> {
    state.workflows.all()
}

/// 创建或更新一个自定义工作流（JSON 结构同 Workflow，同名 id 覆盖）
#[tauri::command]
pub fn add_workflow(state: State<'_, AppState>, wf: Workflow) -> Result<String, String> {
    state.workflows.save(wf.clone())?;
    Ok(format!("已保存工作流「{}」（{} 个阶段）", wf.name, wf.stages.len()))
}

/// 删除一个自定义工作流（含其执行日志）；内置工作流不可删除
#[tauri::command]
pub fn workflow_delete(state: State<'_, AppState>, id: String) -> Result<bool, String> {
    if builtin_workflows().iter().any(|w| w.id == id) {
        return Err("内置工作流不可删除".into());
    }
    let removed = state.workflows.delete(&id)?;
    if removed {
        let _ = state.workflows.clear_runs(&id);
    }
    Ok(removed)
}

/// 查询工作流执行日志（workflow_id 空查全部）
#[tauri::command]
pub fn workflow_runs(
    state: State<'_, AppState>,
    workflow_id: String,
    limit: Option<usize>,
) -> Vec<WorkflowRun> {
    state.workflows.runs(&workflow_id, limit.unwrap_or(20))
}

/// 清空某工作流的执行日志
#[tauri::command]
pub fn workflow_clear_runs(
    state: State<'_, AppState>,
    workflow_id: String,
) -> Result<usize, String> {
    state.workflows.clear_runs(&workflow_id)
}

/// 直接执行一个工作流并写入文档窗口（供前端/外部调用），记录执行日志
#[tauri::command]
pub async fn run_workflow(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    input: String,
) -> Result<String, String> {
    let wf = state
        .workflows
        .find(&id)
        .ok_or_else(|| format!("未找到工作流: {id}"))?;
    let output = run_tracked(&app, state.inner(), &wf, &input).await?;
    render_output(&app, state.inner(), &format!("工作流 · {}", wf.name), &output);
    Ok(output)
}