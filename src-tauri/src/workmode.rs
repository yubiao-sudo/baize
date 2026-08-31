//! 工作模式（WorkMode）：用户可显式选择的专业工作身份。
//!
//! 每种模式声明：系统提示词（专业方法论）、允许工具白名单、产出文档模板、可自研工具模板。
//! 与 `SubAgentType` 不同：WorkMode 是「会话级用户显式身份」，不再是匿名子任务分工。
//! 切换模式时回收 `workmode` 命名空间下的自研工具，避免跨工种工具串用。

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::plugin::run_command;
use crate::tools::{PermissionClass, Tool, ToolRegistry};

/// 工具自研模板（声明本模式可自研哪类工具）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolTemplate {
    pub name: String,
    pub description: String,
    pub hint: String,
}

/// 文档模板（本模式稳定产出的工作文档）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocTemplate {
    pub id: String,
    pub title: String,
    pub outline: Vec<String>,
}

/// 工作模式定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkMode {
    pub id: String,
    pub label: String,
    pub description: String,
    pub system_prompt: String,
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub tool_templates: Vec<ToolTemplate>,
    #[serde(default)]
    pub doc_templates: Vec<DocTemplate>,
    #[serde(default)]
    pub skills: Vec<String>,
}

/// 内置工作模式（无用户配置时的兜底默认）
pub fn builtin_modes() -> Vec<WorkMode> {
    vec![qa_engineer(), dev_engineer()]
}

fn qa_engineer() -> WorkMode {
    WorkMode {
        id: "qa-engineer".into(),
        label: "软件测试工程师".into(),
        description: "需求分析、测试用例设计、测试执行与测试报告".into(),
        system_prompt: r#"你是「软件测试工程师」工作模式下的专业 QA。

【测试思维】
- 先理解被测对象：需求、接口、代码实现、运行方式。
- 用例设计覆盖：等价类划分、边界值分析、异常路径、业务规则与状态转移。
- 区分「静态分析」（读代码发现潜在缺陷）与「动态执行」（运行命令获得实证）。
- 每个结论都要有依据：路径、用例编号、实际输出、期望输出。

【行为规范】
1. 用 todo_update 拆解测试流程进度。
2. 与「需求 → 测试用例」相关时，优先调用 generate_test_cases 工具生成结构化用例。
3. 开始测试任务（生成用例/执行用例/看报告）前，先用 panel_control 打开 test 测试面板，让用户能在界面上同步看到项目配置、用例与报告；完成后再视需要关闭。
4. UI 自动化测试用 run_ui_test 工具（结构化步骤 + 断言，产出报告）；对单个 UI 状态验证可用 assert_ui 工具（窗口标题 / 画面文字 / 视觉目标三重校验）。
5. 视觉回归用 visual_diff 工具：先 capture_screen 留基线截图，改版/回归后再 visual_diff 对比（差异占比 + 差异区域包围盒 + 红框高亮图），把高亮图路径写进报告，UI 改动一眼可查。
6. 接口测试用 run_api_test 工具（结构化请求 + 状态码/响应体/JSON 字段断言，产出报告）。
7. 产物写入右侧文档窗口（markdown_set/append），不在对话里贴长文。
8. 运行测试命令前先经用户授权；只读操作自动放行。
9. 缺陷描述遵循「标题 + 复现步骤 + 期望 + 实际 + 严重级别」。"#.into(),
        allowed_tools: vec![
            "panel_control".into(),
            "list_files".into(),
            "read_file".into(),
            "run_command".into(),
            "list_windows".into(),
            "read_screen".into(),
            "read_window".into(),
            "capture_screen".into(),
            "visual_diff".into(),
            "find_element".into(),
            "ground_element".into(),
            "click_element".into(),
            "click_at".into(),
            "mouse_click".into(),
            "mouse_drag".into(),
            "type_text".into(),
            "paste_text".into(),
            "key_press".into(),
            "key_down".into(),
            "key_up".into(),
            "close_popup".into(),
            "window_minimize_all".into(),
            "window_focus".into(),
            "window_set_topmost".into(),
            "window_prepare".into(),
            "browser_open".into(),
            "browser_navigate".into(),
            "browser_search".into(),
            "browser_read".into(),
            "rag_index".into(),
            "rag_search".into(),
            "todo_update".into(),
            "markdown_set".into(),
            "markdown_append".into(),
            "markdown_get".into(),
            "generate_test_cases".into(),
            "assert_ui".into(),
            "run_ui_test".into(),
            "run_api_test".into(),
            "author_tool".into(),
            "open_terminal".into(),
            "terminal_send".into(),
            "ps_exec".into(),
            "env_check".into(),
            "software_search".into(),
            "software_info".into(),
            "software_list".into(),
            "disk_info".into(),
            "software_install".into(),
            "software_uninstall".into(),
            "system_get".into(),
            "system_set".into(),
        ],
        tool_templates: vec![
            ToolTemplate {
                name: "run_test_suite".into(),
                description: "运行项目测试套件并汇总结果".into(),
                hint: "调用项目测试命令（pytest / npm test 等）".into(),
            },
            ToolTemplate {
                name: "api_smoke_test".into(),
                description: "对 HTTP 接口做冒烟/契约校验".into(),
                hint: "脚本循环断言状态码与字段".into(),
            },
            ToolTemplate {
                name: "gen_test_data".into(),
                description: "生成边界/异常测试数据".into(),
                hint: "脚本构造数据集".into(),
            },
            ToolTemplate {
                name: "diff_check".into(),
                description: "对比期望输出与实际输出".into(),
                hint: "shell 的 diff 或脚本比较".into(),
            },
        ],
        doc_templates: vec![
            DocTemplate {
                id: "test_plan".into(),
                title: "测试计划".into(),
                outline: vec!["测试范围".into(), "测试策略".into(), "环境".into(), "风险".into()],
            },
            DocTemplate {
                id: "test_case".into(),
                title: "测试用例设计".into(),
                outline: vec!["用例编号".into(), "前置条件".into(), "步骤".into(), "期望结果".into()],
            },
            DocTemplate {
                id: "test_report".into(),
                title: "测试执行报告".into(),
                outline: vec!["执行概览".into(), "通过失败统计".into(), "缺陷汇总".into(), "风险结论".into()],
            },
            DocTemplate {
                id: "bug_list".into(),
                title: "缺陷清单".into(),
                outline: vec!["缺陷 ID".into(), "标题".into(), "严重级别".into(), "复现步骤".into(), "状态".into()],
            },
        ],
        skills: vec![],
    }
}

fn dev_engineer() -> WorkMode {
    WorkMode {
        id: "dev-engineer".into(),
        label: "开发工程师".into(),
        description: "需求分析、技术设计、编码实现、测试验证与文档".into(),
        system_prompt: r#"你是「开发工程师」工作模式下的全栈工程师。

【工程规范】
- 动手前先理解现有代码：搜索、定位、理清调用链。
- 遵循最小改动原则，不做需求外重构。
- 修改后必须验证：构建/编译/运行测试，拿到证据再宣称完成。

【行为规范】
1. 用 todo_update 拆解开发流程进度。
2. 按 需求分析 -> 技术设计 -> 编码 -> 构建/测试 -> 文档 推进。
3. 需要批量构建/测试时，用 author_tool 自研自动化工具。
4. 产物（设计文档、变更说明、接口文档）写入右侧文档窗口。
5. 一切写操作（写文件、执行命令）先经用户授权。"#.into(),
        allowed_tools: vec![
            "panel_control".into(),
            "list_files".into(),
            "read_file".into(),
            "write_file".into(),
            "edit_file".into(),
            "create_directory".into(),
            "move_file".into(),
            "run_command".into(),
            "list_windows".into(),
            "read_screen".into(),
            "read_window".into(),
            "capture_screen".into(),
            "visual_diff".into(),
            "find_element".into(),
            "ground_element".into(),
            "click_element".into(),
            "click_at".into(),
            "mouse_click".into(),
            "mouse_drag".into(),
            "type_text".into(),
            "paste_text".into(),
            "key_press".into(),
            "key_down".into(),
            "key_up".into(),
            "close_popup".into(),
            "window_minimize_all".into(),
            "window_focus".into(),
            "window_set_topmost".into(),
            "window_prepare".into(),
            "browser_open".into(),
            "browser_navigate".into(),
            "browser_search".into(),
            "browser_read".into(),
            "rag_index".into(),
            "rag_search".into(),
            "plugin_load".into(),
            "todo_update".into(),
            "markdown_set".into(),
            "markdown_append".into(),
            "markdown_get".into(),
            "spawn_subagent".into(),
            "author_tool".into(),
            "open_terminal".into(),
            "terminal_send".into(),
            "ps_exec".into(),
            "env_check".into(),
            "software_search".into(),
            "software_info".into(),
            "software_list".into(),
            "disk_info".into(),
            "software_install".into(),
            "software_uninstall".into(),
            "system_get".into(),
            "system_set".into(),
        ],
        tool_templates: vec![
            ToolTemplate {
                name: "build_project".into(),
                description: "执行构建/编译".into(),
                hint: "调用项目构建命令（cargo build / npm run build）".into(),
            },
            ToolTemplate {
                name: "run_unit_tests".into(),
                description: "运行单元测试并汇总".into(),
                hint: "调用测试命令".into(),
            },
            ToolTemplate {
                name: "lint_check".into(),
                description: "代码风格/静态检查".into(),
                hint: "cargo clippy / eslint 等".into(),
            },
            ToolTemplate {
                name: "format_code".into(),
                description: "代码格式化".into(),
                hint: "cargo fmt / prettier 等".into(),
            },
            ToolTemplate {
                name: "gen_api_doc".into(),
                description: "从代码抽取接口生成文档".into(),
                hint: "解析注释/签名生成 markdown".into(),
            },
        ],
        doc_templates: vec![
            DocTemplate {
                id: "tech_design".into(),
                title: "技术设计文档".into(),
                outline: vec!["现状".into(), "方案".into(), "数据结构".into(), "接口".into()],
            },
            DocTemplate {
                id: "changelog".into(),
                title: "变更说明".into(),
                outline: vec!["变更清单".into(), "文件列表".into(), "影响面".into(), "测试验证".into()],
            },
            DocTemplate {
                id: "req_analysis".into(),
                title: "需求分析".into(),
                outline: vec!["背景".into(), "目标".into(), "功能点".into(), "非功能".into(), "影响面".into()],
            },
            DocTemplate {
                id: "api_doc".into(),
                title: "接口文档".into(),
                outline: vec!["接口列表".into(), "请求响应".into(), "参数说明".into(), "示例".into()],
            },
        ],
        skills: vec![],
    }
}

/// 工作模式注册表（挂到 `AppState`）
pub struct WorkModeRegistry {
    modes: RwLock<Vec<WorkMode>>,
    current: Mutex<Option<String>>,
    authored: Mutex<Vec<String>>,
}

impl WorkModeRegistry {
    pub fn new() -> Self {
        Self {
            modes: RwLock::new(Vec::new()),
            current: Mutex::new(None),
            authored: Mutex::new(Vec::new()),
        }
    }

    /// 注册内置模式
    pub fn with_builtins() -> Self {
        let reg = Self::new();
        for m in builtin_modes() {
            reg.register(m);
        }
        reg
    }

    /// 注册一个模式（按 id 去重）
    pub fn register(&self, mode: WorkMode) {
        let mut modes = self.modes.write().unwrap();
        if !modes.iter().any(|m| m.id == mode.id) {
            modes.push(mode);
        }
    }

    /// 列出全部已注册模式
    pub fn list(&self) -> Vec<WorkMode> {
        self.modes.read().unwrap().clone()
    }

    /// 当前激活模式（无则返回通用模式 None）
    pub fn current(&self) -> Option<WorkMode> {
        let id = self.current.lock().unwrap().clone();
        id.and_then(|id| {
            self.modes
                .read()
                .unwrap()
                .iter()
                .find(|m| m.id == id)
                .cloned()
        })
    }

    pub fn current_id(&self) -> Option<String> {
        self.current.lock().unwrap().clone()
    }

    /// 激活指定模式
    pub fn activate(&self, id: &str) -> Result<WorkMode, String> {
        let mode = self
            .modes
            .read()
            .unwrap()
            .iter()
            .find(|m| m.id == id)
            .cloned()
            .ok_or_else(|| format!("未知工作模式: {id}"))?;
        *self.current.lock().unwrap() = Some(id.to_string());
        // 切换模式清空上一模式的自研工具记录（实际工具由 set_work_mode 的 remove_ns 回收）
        self.authored.lock().unwrap().clear();
        Ok(mode)
    }

    /// 退出模式（回到通用模式），并清空自研工具记录
    pub fn deactivate(&self) {
        *self.current.lock().unwrap() = None;
        self.authored.lock().unwrap().clear();
    }

    /// 当前模式已自研的工具名（用于白名单合并与回收）
    pub fn authored(&self) -> Vec<String> {
        self.authored.lock().unwrap().clone()
    }

    pub fn add_authored(&self, name: &str) {
        let mut a = self.authored.lock().unwrap();
        if !a.iter().any(|n| n == name) {
            a.push(name.to_string());
        }
    }
}

impl Default for WorkModeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ───────────────────── 工具自研（author_tool） ─────────────────────

/// 自研工具的执行方式：shell 命令 或 脚本（python / nodejs）
pub enum ToolExec {
    Command(String),
    Script { lang: String, code: String },
}

/// 运行时自研的工具：命令或脚本，占位符替换 + 执行。
/// 权限为 `Write`，执行时走人工审批（与 ShellTool 同级别风控）。
pub struct DynamicTool {
    name: String,
    description: String,
    parameters: Value,
    exec: ToolExec,
    /// 来源信任级别：trusted / authored / untrusted（untrusted 走进程级沙箱执行）
    trust: String,
}

impl DynamicTool {
    /// 供任务广场（plaza）与自研（author_tool）运行时动态注册工具使用
    pub fn new(name: String, description: String, parameters: Value, exec: ToolExec, trust: String) -> Self {
        Self {
            name,
            description,
            parameters,
            exec,
            trust,
        }
    }
}

impl Tool for DynamicTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn schema(&self) -> Value {
        self.parameters.clone()
    }
    fn permission(&self) -> PermissionClass {
        // 信任分级闸门：未受信（市场来源）工具无论从哪条路径调用都按「高危」处理，
        // 强制走用户审批（agent 自动调用 / 广场直接运行一致）；可信/自研工具保留原 Write 语义。
        if self.trust == "untrusted" {
            PermissionClass::HighRisk
        } else {
            PermissionClass::Write
        }
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        // 收集参数值（用于 {key} 占位替换）
        let mut params: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        if let Some(obj) = args.as_object() {
            for (k, v) in obj {
                let val = match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                params.insert(k.clone(), val);
            }
        }
        // 未受信（市场来源）工具走沙箱；可信 / 自研工具直接执行
        let sandboxed = self.trust == "untrusted";
        let stdout = match &self.exec {
            ToolExec::Command(cmd) => {
                let substituted = substitute(cmd, &params);
                if sandboxed {
                    run_command_sandboxed(&substituted)
                } else {
                    run_command(&substituted)
                }
            }
            ToolExec::Script { lang, code } => {
                let substituted = substitute(code, &params);
                if sandboxed {
                    execute_script_sandboxed(&self.name, lang, &substituted)
                } else {
                    execute_script(&self.name, lang, &substituted)
                }
            }
        };
        Ok(json!({ "stdout": stdout }))
    }
}

/// 占位符替换：{key} → 参数值
fn substitute(template: &str, params: &std::collections::HashMap<String, String>) -> String {
    let mut out = template.to_string();
    for (k, v) in params {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

/// 把脚本代码落盘到临时目录并用解释器执行
fn execute_script(name: &str, lang: &str, code: &str) -> String {
    let (ext, cmd) = match lang.to_lowercase().as_str() {
        "python" | "py" => ("py", "python"),
        "node" | "nodejs" | "js" => ("js", "node"),
        _ => return format!("不支持的语言: {lang}（仅支持 python / nodejs）"),
    };
    let path = std::env::temp_dir().join(format!("workmode_{name}.{ext}"));
    if let Err(e) = std::fs::write(&path, code) {
        return format!("写脚本失败: {e}");
    }
    let output = std::process::Command::new(cmd).arg(&path).output();
    match output {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).to_string();
            let err = String::from_utf8_lossy(&o.stderr);
            if !err.is_empty() {
                s.push_str("\n[stderr]\n");
                s.push_str(&err);
            }
            if s.chars().count() > 4000 {
                s = s.chars().take(4000).collect();
            }
            s
        }
        Err(e) => format!("执行失败: {e}"),
    }
}

// ───────────────────── 未受信工具 · 进程级沙箱 ─────────────────────

/// 未受信工具沙箱执行超时（秒）
const SANDBOX_TIMEOUT: Duration = Duration::from_secs(30);
const SANDBOX_TIMEOUT_SECS: u64 = 30;

/// 未受信脚本的进程级沙箱执行：
/// 1. 隔离工作目录 —— 每次调用在临时目录下建独立子目录，脚本只能读写该目录；
/// 2. 剥离环境变量 —— 仅保留最小可用集，避免读取 token/密钥等敏感变量；
/// 3. 执行超时 —— 超过 `SANDBOX_TIMEOUT` 即终止进程。
///
/// 说明：这是「尽力而为」的进程级隔离（无 OS 级网络/写保护），
/// 更严格可采用 WASM 沙箱或 Windows 作业对象（Job Object）进一步增强。
fn execute_script_sandboxed(name: &str, lang: &str, code: &str) -> String {
    let (ext, prog) = match lang.to_lowercase().as_str() {
        "python" | "py" => ("py", "python"),
        "node" | "nodejs" | "js" => ("js", "node"),
        _ => return format!("不支持的语言: {lang}（仅支持 python / nodejs）"),
    };

    let sandbox = std::env::temp_dir().join(format!(
        "baize_sb_{}_{}",
        std::process::id(),
        now_nanos()
    ));
    if let Err(e) = std::fs::create_dir_all(&sandbox) {
        return format!("创建沙箱目录失败: {e}");
    }
    let script = sandbox.join(format!("{name}.{ext}"));
    if let Err(e) = std::fs::write(&script, code) {
        return format!("写脚本失败: {e}");
    }

    let mut cmd = crate::tools::silent_command(prog);
    cmd.arg(&script).current_dir(&sandbox);
    apply_sandbox_env(&mut cmd, &sandbox);
    run_child_sandboxed(cmd)
}

/// 未受信 shell 命令的进程级沙箱执行（隔离目录 + 剥离环境 + 超时）
fn run_command_sandboxed(cmd_str: &str) -> String {
    let sandbox = std::env::temp_dir().join(format!(
        "baize_sb_{}_{}",
        std::process::id(),
        now_nanos()
    ));
    let _ = std::fs::create_dir_all(&sandbox);

    let mut cmd = {
        #[cfg(windows)]
        let c = crate::tools::silent_command("cmd");
        #[cfg(not(windows))]
        let c = std::process::Command::new("sh");
        c
    };
    #[cfg(windows)]
    cmd.args(["/c", cmd_str]);
    #[cfg(not(windows))]
    cmd.args(["-c", cmd_str]);
    cmd.current_dir(&sandbox);
    apply_sandbox_env(&mut cmd, &sandbox);
    run_child_sandboxed(cmd)
}

/// 剥离环境变量，仅保留解释器/系统运行所需的最小集，并把临时目录指向沙箱目录
fn apply_sandbox_env(cmd: &mut std::process::Command, sandbox: &std::path::Path) {
    cmd.env_clear();
    #[cfg(windows)]
    cmd.env(
        "SystemRoot",
        std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into()),
    );
    #[cfg(not(windows))]
    cmd.env("PATH", "/usr/bin:/bin");
    cmd.env("TEMP", sandbox);
    cmd.env("TMP", sandbox);
}

/// 执行已配置好沙箱约束的命令，带超时；超时则杀进程并返回错误。
fn run_child_sandboxed(mut cmd: std::process::Command) -> String {
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return format!("启动失败: {e}"),
    };
    let deadline = Instant::now() + SANDBOX_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => return finish_child(child),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return format!("执行超时（>{SANDBOX_TIMEOUT_SECS}s），已终止进程");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return format!("等待进程失败: {e}"),
        }
    }
}

/// 收集已结束进程的 stdout / stderr（并做长度截断）
fn finish_child(child: std::process::Child) -> String {
    match child.wait_with_output() {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).to_string();
            let err = String::from_utf8_lossy(&o.stderr);
            if !err.is_empty() {
                s.push_str("\n[stderr]\n");
                s.push_str(&err);
            }
            if s.chars().count() > 4000 {
                s = s.chars().take(4000).collect();
            }
            s
        }
        Err(e) => format!("读取输出失败: {e}"),
    }
}

/// 单调纳秒时间戳（用于沙箱目录命名，避免跨调用碰撞）
fn now_nanos() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// `author_tool` 工具：模型按当前工种自行编写并注册一个工具（shell 命令或脚本）。
/// 工具名唯一；注册到 `workmode` 命名空间，本轮后续可见。
pub struct AuthorTool {
    tools: Arc<ToolRegistry>,
    workmodes: Arc<WorkModeRegistry>,
}

impl AuthorTool {
    pub fn new(tools: Arc<ToolRegistry>, workmodes: Arc<WorkModeRegistry>) -> Self {
        Self { tools, workmodes }
    }
}

impl Tool for AuthorTool {
    fn name(&self) -> &str {
        "author_tool"
    }
    fn description(&self) -> &str {
        "根据当前工作模式自行编写并注册一个新工具（shell 命令或 python/nodejs 脚本），供本轮任务后续调用。用 {参数名} 占位"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "唯一工具名，建议带模式前缀（如 qa_run_api_smoke）" },
                "description": { "type": "string", "description": "工具用途说明" },
                "parameters": { "type": "object", "description": "JSON Schema，描述工具入参（可选）" },
                "command": { "type": "string", "description": "shell 命令，用 {参数名} 占位（与 lang+code 二选一）" },
                "lang": { "type": "string", "description": "脚本语言：python 或 nodejs（提供 code 时必填）" },
                "code": { "type": "string", "description": "脚本代码，用 {参数名} 占位（与 command 二选一）" }
            },
            "required": ["name", "description"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let name = args["name"].as_str().ok_or("缺少参数 name")?.to_string();
        let description = args["description"].as_str().unwrap_or("").to_string();
        let parameters = args
            .get("parameters")
            .cloned()
            .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));

        // 执行方式：优先 command，否则 (code + lang)
        let exec = {
            let command = args["command"].as_str().unwrap_or("");
            if !command.is_empty() {
                ToolExec::Command(command.to_string())
            } else {
                let lang = args["lang"]
                    .as_str()
                    .ok_or("缺少参数：请提供 command 或 (code + lang)")?
                    .to_string();
                let code = args["code"].as_str().ok_or("缺少参数 code")?.to_string();
                if code.is_empty() {
                    return Err("code 不能为空".into());
                }
                ToolExec::Script { lang, code }
            }
        };

        if self.tools.get(&name).is_some() {
            return Err(format!("工具 {name} 已存在"));
        }
        let tool = DynamicTool {
            name: name.clone(),
            description,
            parameters,
            exec,
            trust: "authored".into(),
        };
        self.tools.register_ns("workmode", Box::new(tool));
        self.workmodes.add_authored(&name);
        Ok(json!({ "ok": true, "name": name }))
    }
}