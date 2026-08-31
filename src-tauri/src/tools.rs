use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use serde_json::{json, Value};
use crate::memory::MemoryStore;

/// 工具权限分级：只读自动放行，写/高危需人工审批
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PermissionClass {
    ReadOnly,
    Write,
    HighRisk,
}

/// 当前工作空间（后端强绑定的默认工作目录，全局单例）
static WORKSPACE: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn workspace_lock() -> &'static Mutex<Option<String>> {
    WORKSPACE.get_or_init(|| Mutex::new(None))
}

/// 数据库连接配置（名称 → 连接串）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DbConnection {
    pub name: String,
    pub connection: String,
}

/// 全局数据库连接配置缓存（名称 → 连接串）
static DB_CONNECTIONS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn db_connections_lock() -> &'static Mutex<HashMap<String, String>> {
    DB_CONNECTIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 刷新连接配置缓存
pub fn refresh_db_connections(list: &[DbConnection]) {
    let mut map = db_connections_lock().lock().unwrap();
    map.clear();
    for c in list {
        map.insert(c.name.clone(), c.connection.clone());
    }
}

/// 解析连接串：输入若是已配置的名称则替换为连接串，否则原样返回
pub fn resolve_db_connection(input: &str) -> String {
    if let Ok(map) = db_connections_lock().lock() {
        if let Some(conn) = map.get(input) {
            return conn.clone();
        }
    }
    input.to_string()
}

/// 设置当前工作空间（空串表示清除）
pub fn set_workspace(path: &str) {
    let trimmed = path.trim();
    *workspace_lock().lock().unwrap() = if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    };
}

/// 获取当前工作空间
pub fn get_workspace() -> Option<String> {
    workspace_lock().lock().unwrap().clone()
}

// ---------------- 全局取消标志：用户点击停止时，长时间子进程工具能提前感知并终止 ----------------

static TOOL_CANCEL: AtomicBool = AtomicBool::new(false);

/// 置位全局取消标志（stop_chat 时调用）
pub fn request_global_cancel() {
    TOOL_CANCEL.store(true, Ordering::SeqCst);
}

/// 复位全局取消标志（新一轮任务开始 / 定时任务入口调用）
pub fn clear_global_cancel() {
    TOOL_CANCEL.store(false, Ordering::SeqCst);
}

/// 是否已请求取消
pub fn global_cancelled() -> bool {
    TOOL_CANCEL.load(Ordering::SeqCst)
}

/// 带取消感知的子进程执行：等待期间轮询全局取消标志，命中即 kill 子进程并返回错误。
/// 用于 run_on_host / run_in_docker 等此前用 .output() 一黑到底的阻塞调用
fn run_child_cancellable(
    mut command: std::process::Command,
) -> Result<std::process::Output, String> {
    use std::io::Read;
    let mut child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("启动进程失败: {e}"))?;
    let stdout_reader = child.stdout.take().map(|mut s| {
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = s.read_to_string(&mut buf);
            buf
        })
    });
    let stderr_reader = child.stderr.take().map(|mut s| {
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = s.read_to_string(&mut buf);
            buf
        })
    });
    loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => {
                let stdout = stdout_reader
                    .and_then(|h| h.join().ok())
                    .unwrap_or_default();
                let stderr = stderr_reader
                    .and_then(|h| h.join().ok())
                    .unwrap_or_default();
                return Ok(std::process::Output {
                    status,
                    stdout: stdout.into_bytes(),
                    stderr: stderr.into_bytes(),
                });
            }
            None => {
                if global_cancelled() {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("任务已被用户停止，进程已终止".to_string());
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// 解析路径：绝对路径原样返回；相对路径拼到工作空间根目录（未设置工作空间则原样返回）
pub(crate) fn resolve_path(path: &str) -> String {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        return path.to_string();
    }
    if let Some(ws) = get_workspace() {
        if !ws.is_empty() {
            return std::path::Path::new(&ws)
                .join(path)
                .to_string_lossy()
                .to_string();
        }
    }
    path.to_string()
}

// ---------------- 危险操作撤销：写/改/移动前自动快照，undo 一键还原 ----------------

/// 一条「操作前」路径快照（文件存字节，目录记标记，不存在记不存在）
struct PathSnapshot {
    path: String,
    existed: bool,
    is_dir: bool,
    bytes: Vec<u8>,
}

/// 一次危险操作对应的撤销记录（可能涉及多个路径，如移动）
struct UndoRecord {
    desc: String,
    snapshots: Vec<PathSnapshot>,
}

/// 撤销栈（后进先出，undo 恢复最近一次）
static UNDO_STACK: OnceLock<Mutex<Vec<UndoRecord>>> = OnceLock::new();

fn undo_stack_lock() -> &'static Mutex<Vec<UndoRecord>> {
    UNDO_STACK.get_or_init(|| Mutex::new(Vec::new()))
}

/// 拍摄单个路径的「操作前」快照
fn snapshot_path(path: &str) -> PathSnapshot {
    let p = std::path::Path::new(path);
    match std::fs::metadata(p) {
        Ok(m) if m.is_dir() => PathSnapshot {
            path: path.to_string(),
            existed: true,
            is_dir: true,
            bytes: Vec::new(),
        },
        Ok(_) => PathSnapshot {
            path: path.to_string(),
            existed: true,
            is_dir: false,
            bytes: std::fs::read(path).unwrap_or_default(),
        },
        Err(_) => PathSnapshot {
            path: path.to_string(),
            existed: false,
            is_dir: false,
            bytes: Vec::new(),
        },
    }
}

/// 一次给多个路径拍照（操作前调用）
fn snapshot_paths(paths: &[&str]) -> Vec<PathSnapshot> {
    paths.iter().map(|p| snapshot_path(p)).collect()
}

/// 压入一条撤销记录
fn push_undo(desc: String, snapshots: Vec<PathSnapshot>) {
    undo_stack_lock().lock().unwrap().push(UndoRecord { desc, snapshots });
}

/// 还原一条快照（逆操作）
fn restore_snapshot(s: &PathSnapshot) -> Result<(), String> {
    let p = std::path::Path::new(&s.path);
    if s.existed {
        if s.is_dir {
            std::fs::create_dir_all(p).map_err(|e| format!("还原目录失败: {e}"))?;
        } else {
            if let Some(parent) = p.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent).map_err(|e| format!("还原父目录失败: {e}"))?;
                }
            }
            std::fs::write(p, &s.bytes).map_err(|e| format!("还原文件失败: {e}"))?;
        }
    } else {
        // 原不存在，则删除当前产物（文件或目录）
        match std::fs::metadata(p) {
            Ok(m) if m.is_dir() => std::fs::remove_dir_all(p).map_err(|e| format!("删除目录失败: {e}"))?,
            Ok(_) => std::fs::remove_file(p).map_err(|e| format!("删除文件失败: {e}"))?,
            Err(_) => {}
        }
    }
    Ok(())
}

/// 撤销最近一次危险操作，返回描述
pub(crate) fn undo_last_step() -> Result<String, String> {
    let record = undo_stack_lock()
        .lock()
        .unwrap()
        .pop()
        .ok_or("没有可撤销的操作（撤销栈为空）")?;
    let mut restored: Vec<String> = Vec::new();
    // 逆序还原
    for s in record.snapshots.iter().rev() {
        restore_snapshot(s)?;
        restored.push(s.path.clone());
    }
    Ok(format!("已撤销：{}（涉及 {}）", record.desc, restored.join("、")))
}

/// 工具统一抽象：所有能力（文件、Shell、浏览器、MCP、Computer Use 动作）都实现此 trait
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    /// JSON Schema（OpenAI function calling 的 parameters 字段）
    fn schema(&self) -> Value;
    fn permission(&self) -> PermissionClass;
    fn run(&self, args: Value) -> Result<Value, String>;
}

/// 工具注册表：内部 RwLock，支持按命名空间增删（用于 MCP 运行时重建）
pub struct ToolRegistry {
    tools: RwLock<Vec<Arc<dyn Tool>>>,
    namespaces: RwLock<HashMap<String, Vec<String>>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: RwLock::new(Vec::new()),
            namespaces: RwLock::new(HashMap::new()),
        }
    }

    /// 注册全局工具（无命名空间）
    pub fn register(&self, tool: Box<dyn Tool>) {
        self.register_ns("__global__", tool);
    }

    /// 注册到指定命名空间（如 "mcp"），可整体移除后重建
    pub fn register_ns(&self, ns: &str, tool: Box<dyn Tool>) {
        let arc: Arc<dyn Tool> = Arc::from(tool);
        let name = arc.name().to_string();
        // 去重：工具名必须唯一（OpenAI 兼容 API 严格校验），首个注册优先
        {
            let tools = self.tools.read().unwrap();
            if tools.iter().any(|t| t.name() == name) {
                eprintln!("[工具] 跳过重复工具名: {name}");
                return;
            }
        }
        self.namespaces
            .write()
            .unwrap()
            .entry(ns.to_string())
            .or_default()
            .push(name);
        self.tools.write().unwrap().push(arc);
    }

    /// 移除某命名空间下的全部工具（旧工具被 drop，MCP 子进程随之关闭）
    pub fn remove_ns(&self, ns: &str) {
        let removed = self.namespaces.write().unwrap().remove(ns).unwrap_or_default();
        if !removed.is_empty() {
            let mut tools = self.tools.write().unwrap();
            tools.retain(|t| !removed.contains(&t.name().to_string()));
        }
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools
            .read()
            .unwrap()
            .iter()
            .find(|t| t.name() == name)
            .cloned()
    }

    /// 生成 OpenAI 兼容的 tools 描述，供 function calling
    pub fn schemas(&self) -> Vec<Value> {
        self.tools
            .read()
            .unwrap()
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name(),
                        "description": t.description(),
                        "parameters": t.schema(),
                    }
                })
            })
            .collect()
    }

    pub fn names(&self) -> Vec<String> {
        self.tools
            .read()
            .unwrap()
            .iter()
            .map(|t| t.name().to_string())
            .collect()
    }

    /// 某命名空间下的工具名列表（用于区分 plaza 自研工具与内置工具）
    pub fn ns_names(&self, ns: &str) -> Vec<String> {
        self.namespaces
            .read()
            .unwrap()
            .get(ns)
            .cloned()
            .unwrap_or_default()
    }

    /// 生成过滤后的工具 schemas（只包含允许的工具名）
    pub fn schemas_filtered(&self, allowed: &[&str]) -> Vec<Value> {
        self.tools
            .read()
            .unwrap()
            .iter()
            .filter(|t| allowed.contains(&t.name()))
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name(),
                        "description": t.description(),
                        "parameters": t.schema(),
                    }
                })
            })
            .collect()
    }

    /// 运行指定工具
    pub fn run(&self, name: &str, args: Value) -> Result<Value, String> {
        self.get(name)
            .ok_or_else(|| format!("工具不存在: {name}"))?
            .run(args)
    }
}

// ---------------- 内置只读文件工具 ----------------

pub struct FileListTool;

impl Tool for FileListTool {
    fn name(&self) -> &str {
        "list_files"
    }
    fn description(&self) -> &str {
        "列出目录下的文件与子目录（只读；path 可为绝对路径或相对于工作空间的路径，缺省列出工作空间根目录）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "绝对路径，或相对于工作空间的路径（可省略）" }
            }
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let raw = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let path = resolve_path(raw);
        let entries = std::fs::read_dir(&path).map_err(|e| format!("读取目录失败: {e}"))?;
        let mut list = Vec::new();
        for entry in entries.flatten() {
            let is_dir = entry.metadata().map(|m| m.is_dir()).unwrap_or(false);
            list.push(json!({
                "name": entry.file_name().to_string_lossy(),
                "is_dir": is_dir,
            }));
        }
        Ok(json!(list))
    }
}

pub struct FileReadTool;

impl Tool for FileReadTool {
    fn name(&self) -> &str {
        "read_file"
    }
    fn description(&self) -> &str {
        "读取一个文本文件的内容（只读，可访问任意本地路径）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "文件的绝对路径" }
            },
            "required": ["path"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let path = resolve_path(args["path"].as_str().ok_or("缺少参数 path")?);
        std::fs::read_to_string(&path)
            .map(|s| json!(s))
            .map_err(|e| format!("读取文件失败: {e}"))
    }
}

// ---------------- 写文件工具（开发工程师核心，均需审批） ----------------

pub struct WriteFileTool;

impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }
    fn description(&self) -> &str {
        "写入或覆盖一个文本文件（自动创建父目录；写操作需授权）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "文件绝对路径" },
                "content": { "type": "string", "description": "要写入的完整内容" }
            },
            "required": ["path", "content"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let path = resolve_path(args["path"].as_str().ok_or("缺少参数 path")?);
        let content = args["content"].as_str().unwrap_or("");
        let snapshots = snapshot_paths(&[path.as_str()]);
        if let Some(parent) = std::path::Path::new(&path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| format!("创建父目录失败: {e}"))?;
            }
        }
        std::fs::write(&path, content).map_err(|e| format!("写入文件失败: {e}"))?;
        push_undo(format!("write_file {path}"), snapshots);
        Ok(json!({ "ok": true, "path": path, "bytes": content.len() }))
    }
}

pub struct EditFileTool;

impl Tool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }
    fn description(&self) -> &str {
        "对文本文件做精确替换：old_string 必须唯一（除非 replace_all=true），写回文件（需授权）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "文件绝对路径" },
                "old_string": { "type": "string", "description": "要被替换的原文（需精确匹配）" },
                "new_string": { "type": "string", "description": "替换后的文本" },
                "replace_all": { "type": "boolean", "description": "是否替换所有匹配（默认 false）" }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let path = resolve_path(args["path"].as_str().ok_or("缺少参数 path")?);
        let old = args["old_string"].as_str().ok_or("缺少参数 old_string")?;
        let new = args["new_string"].as_str().unwrap_or("");
        if old.is_empty() {
            return Err("old_string 不能为空".into());
        }
        let replace_all = args["replace_all"].as_bool().unwrap_or(false);
        let original = std::fs::read_to_string(&path).map_err(|e| format!("读取文件失败: {e}"))?;
        let count = original.matches(old).count();
        if count == 0 {
            return Err("未在文件中找到匹配的 old_string".into());
        }
        if count > 1 && !replace_all {
            return Err(format!(
                "old_string 出现 {count} 次，请提供更精确的上下文，或设置 replace_all=true"
            ));
        }
        let result = if replace_all {
            original.replace(old, new)
        } else {
            original.replacen(old, new, 1)
        };
        // 用已知原文构造快照，避免重复读盘
        let snapshot = PathSnapshot {
            path: path.clone(),
            existed: true,
            is_dir: false,
            bytes: original.into_bytes(),
        };
        std::fs::write(&path, &result).map_err(|e| format!("写入文件失败: {e}"))?;
        push_undo(format!("edit_file {path}"), vec![snapshot]);
        Ok(json!({ "ok": true, "path": path, "replaced": count }))
    }
}

pub struct CreateDirectoryTool;

impl Tool for CreateDirectoryTool {
    fn name(&self) -> &str {
        "create_directory"
    }
    fn description(&self) -> &str {
        "递归创建目录（目录已存在时忽略；需授权）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "目录绝对路径" }
            },
            "required": ["path"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let path = resolve_path(args["path"].as_str().ok_or("缺少参数 path")?);
        std::fs::create_dir_all(&path).map_err(|e| format!("创建目录失败: {e}"))?;
        Ok(json!({ "ok": true, "path": path }))
    }
}

pub struct MoveFileTool;

impl Tool for MoveFileTool {
    fn name(&self) -> &str {
        "move_file"
    }
    fn description(&self) -> &str {
        "移动或重命名文件/目录（自动创建目标父目录；需授权）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "from": { "type": "string", "description": "源路径（文件或目录）" },
                "to": { "type": "string", "description": "目标路径" }
            },
            "required": ["from", "to"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let from = resolve_path(args["from"].as_str().ok_or("缺少参数 from")?);
        let to = resolve_path(args["to"].as_str().ok_or("缺少参数 to")?);
        let snapshots = snapshot_paths(&[from.as_str(), to.as_str()]);
        if let Some(parent) = std::path::Path::new(&to).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| format!("创建目标父目录失败: {e}"))?;
            }
        }
        std::fs::rename(&from, &to).map_err(|e| format!("移动失败: {e}"))?;
        push_undo(format!("move_file {from} → {to}"), snapshots);
        Ok(json!({ "ok": true, "from": from, "to": to }))
    }
}

/// 撤销最近一次危险操作（write_file / edit_file / move_file 均自动留存快照）
pub struct UndoTool;

impl Tool for UndoTool {
    fn name(&self) -> &str {
        "undo"
    }
    fn description(&self) -> &str {
        "撤销最近一次危险文件操作（write_file/edit_file/move_file），把相关文件还原到操作前状态。可在误操作后调用；可连续撤销多次，每次回退一步"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "steps": { "type": "integer", "description": "连续撤销的步数，默认 1" }
            }
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let steps = args["steps"].as_u64().unwrap_or(1).clamp(1, 20) as usize;
        let mut reports = Vec::new();
        for _ in 0..steps {
            match undo_last_step() {
                Ok(r) => reports.push(r),
                Err(e) => {
                    if reports.is_empty() {
                        return Err(e);
                    }
                    break;
                }
            }
        }
        Ok(json!({ "ok": true, "undone": reports }))
    }
}

// ---------------- Shell 工具（高危，需审批） ----------------

pub struct ShellTool;

impl Tool for ShellTool {
    fn name(&self) -> &str {
        "run_command"
    }
    fn description(&self) -> &str {
        "执行一条 Shell 命令并返回输出（高危操作，会请求授权）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "要执行的命令" }
            },
            "required": ["command"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::HighRisk
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let cmd = args["command"].as_str().ok_or("缺少参数 command")?;
        let output = run_shell(cmd)?;
        Ok(json!({ "stdout": output }))
    }
}

/// 在 Windows 上以隐藏窗口方式启动子进程（防控制台黑窗闪烁）。
/// 全项目所有 powershell/cmd/python 等子进程启动统一走此入口。
pub fn silent_command(program: &str) -> std::process::Command {
    let mut c = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

// ───────────────── launch_app 快捷启动应用 ─────────────────
// 开始菜单索引（用户 + 常用，含子目录 .lnk）一次性缓存；未命中再查 UWP（Get-StartApps）。
// 启动后轮询等待目标窗口出现并返回窗口信息——把「找图标→点开始菜单→搜索→点开→等窗口」
// 的多轮 GUI 慢流程折叠成一次工具调用。

static APP_INDEX: OnceLock<Vec<(String, std::path::PathBuf)>> = OnceLock::new();

fn start_menu_apps() -> &'static Vec<(String, std::path::PathBuf)> {
    APP_INDEX.get_or_init(|| {
        let mut out: Vec<(String, std::path::PathBuf)> = Vec::new();
        let mut roots: Vec<std::path::PathBuf> = Vec::new();
        if let Ok(u) = std::env::var("USERPROFILE") {
            roots.push(std::path::PathBuf::from(format!(
                "{u}\\AppData\\Roaming\\Microsoft\\Windows\\Start Menu\\Programs"
            )));
        }
        if let Ok(pd) = std::env::var("ProgramData") {
            roots.push(std::path::PathBuf::from(format!(
                "{pd}\\Microsoft\\Windows\\Start Menu\\Programs"
            )));
        }
        fn walk(dir: &std::path::Path, out: &mut Vec<(String, std::path::PathBuf)>, depth: usize) {
            if depth > 6 {
                return;
            }
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        walk(&p, out, depth + 1);
                    } else if p
                        .extension()
                        .map(|x| x.eq_ignore_ascii_case("lnk"))
                        .unwrap_or(false)
                    {
                        if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                            out.push((stem.to_lowercase(), p.clone()));
                        }
                    }
                }
            }
        }
        for r in roots {
            walk(&r, &mut out, 0);
        }
        out
    })
}

/// UWP / 商店应用：Get-StartApps 枚举（名称, AppID），explorer shell:AppsFolder 启动
fn find_uwp_app(name: &str) -> Option<(String, String)> {
    let out = crate::tools::silent_command("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-StartApps | ConvertTo-Json -Compress",
        ])
        .output()
        .ok()?;
    let v: Value = serde_json::from_slice(&out.stdout).ok()?;
    let arr = v.as_array()?;
    let ql = name.to_lowercase();
    for it in arr {
        let app_name = it["Name"].as_str().unwrap_or("").to_lowercase();
        let app_id = it["AppID"].as_str().unwrap_or("");
        if !app_id.is_empty()
            && (app_name.contains(&ql) || ql.contains(app_name.as_str()))
        {
            return Some((app_name, app_id.to_string()));
        }
    }
    None
}

pub struct LaunchAppTool;

impl Tool for LaunchAppTool {
    fn name(&self) -> &str {
        "launch_app"
    }
    fn description(&self) -> &str {
        "启动桌面应用（首选方式，一次调用完成）：从开始菜单索引（含 UWP 商店应用）按名称匹配并启动，\
         然后轮询等待应用窗口出现（最长 wait_secs 秒），返回窗口标题与位置——拿到窗口信息即可直接接 \
         window_prepare/screen_elements 规划操作，无需再 GUI 点开始菜单或 ps_exec Start-Process"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "应用名称关键词，如 \"汽水音乐\"、\"记事本\"、\"Calculator\"" },
                "wait_secs": { "type": "number", "description": "等待窗口出现的秒数，默认 12" }
            },
            "required": ["name"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let name = args["name"]
            .as_str()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .ok_or("缺少 name 参数")?;
        let wait_secs = args["wait_secs"].as_u64().unwrap_or(12).clamp(2, 30);
        let ql = name.to_lowercase();

        // 1) 开始菜单 .lnk 匹配（最短名最贴近）
        let apps = start_menu_apps();
        let matched = apps
            .iter()
            .filter(|(n, _)| n.contains(&ql) || ql.contains(n.as_str()))
            .min_by_key(|(n, _)| n.len())
            .cloned();

        let (via, launched) = if let Some((n, p)) = matched {
            let r = silent_command("cmd")
                .args(["/c", "start", "", &p.to_string_lossy()])
                .spawn();
            match r {
                Ok(_) => ("start_menu".to_string(), true),
                Err(e) => return Err(format!("启动失败: {e}")),
            }
        } else if let Some((app_name, app_id)) = find_uwp_app(name) {
            // 2) UWP 回退
            let arg = format!("shell:AppsFolder\\{app_id}");
            let mut cmd = silent_command("explorer.exe");
            cmd.args([&arg]);
            let r = cmd.spawn();
            match r {
                Ok(_) => (format!("uwp:{app_name}"), true),
                Err(e) => return Err(format!("UWP 启动失败: {e}")),
            }
        } else {
            return Err(format!(
                "开始菜单与 UWP 应用列表中未找到「{name}」，可让用户手动安装，或改用 ps_exec 启动已知路径"
            ));
        };

        // 3) 等待窗口出现（标题包含应用名关键词即视为就绪）
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(wait_secs);
        let mut window: Option<[i32; 4]> = None;
        while std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(600));
            if let Some(rect) = crate::capability::windows::find_window_rect(name) {
                window = Some(rect);
                break;
            }
        }

        Ok(json!({
            "ok": true,
            "via": via,
            "window_ready": window.is_some(),
            "window_rect": window,
            "note": if window.is_some() {
                "窗口已出现，可直接 window_prepare 清屏 + screen_elements 规划操作"
            } else {
                "已发出启动命令但窗口尚未出现（可能仍在加载），可稍后 list_windows 确认"
            },
        }))
    }
}

/// 打开文件夹/路径宏：一步打开资源管理器并等待窗口出现（替代 launch_app+导航多步流程）
pub struct ExplorerOpenTool;

impl Tool for ExplorerOpenTool {
    fn name(&self) -> &str {
        "explorer_open"
    }
    fn description(&self) -> &str {
        "打开文件资源管理器并定位到指定路径（一步宏）：启动 explorer + 轮询等待窗口出现返回位置，\
         替代 launch_app(\"explorer\") 后再手动导航的多步流程。path 传文件夹绝对路径；传文件路径则在父目录打开并选中该文件"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "文件夹或文件的绝对路径" }
            },
            "required": ["path"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let path = args["path"]
            .as_str()
            .ok_or("缺少参数 path")?
            .trim()
            .trim_matches('"')
            .to_string();
        if path.is_empty() {
            return Err("缺少参数 path".into());
        }
        let path = resolve_path(&path);
        if !std::path::Path::new(&path).exists() {
            return Err(format!("路径不存在: {path}"));
        }
        let start = std::time::Instant::now();
        crate::tools::silent_command("explorer")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("启动资源管理器失败: {e}"))?;
        // 等待资源管理器窗口出现（窗口标题 = 文件夹显示名；选文件模式 = 父目录名）
        let expect = std::path::Path::new(&path)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let mut window: Option<[i32; 4]> = None;
        while start.elapsed().as_millis() < 8000 {
            if !expect.is_empty() {
                if let Some(rect) = crate::capability::windows::find_window_rect(&expect) {
                    window = Some(rect);
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        Ok(json!({
            "ok": true,
            "window_ready": window.is_some(),
            "window_rect": window,
            "note": if window.is_some() {
                "资源管理器已打开，可直接 window_prepare 清屏 + screen_elements 规划操作"
            } else {
                "已发出打开命令但窗口未检测到（可能复用了同目录的现有窗口），可 list_windows 确认"
            },
        }))
    }
}

/// 本机 PowerShell 直连执行（不走 Docker 沙箱，返回结构化输出 + 超时）
pub struct PsExecTool;
impl Tool for PsExecTool {
    fn name(&self) -> &str {
        "ps_exec"
    }
    fn description(&self) -> &str {
        "在本机直接执行 PowerShell 命令（不走 Docker 沙箱），返回 stdout/stderr/exit_code/duration_ms，支持超时。用于构建、脚本、系统操作等需要真实本机环境的命令"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "要执行的 PowerShell 命令" },
                "timeout_secs": { "type": "integer", "description": "超时秒数（默认 60，最大 300）" }
            },
            "required": ["command"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::HighRisk
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let command = args["command"].as_str().ok_or("缺少参数 command")?;
        // 危险命令黑名单
        if let Some(reason) = dangerous_command(command) {
            return Err(format!("命令被安全策略拦截：{reason}"));
        }
        let timeout = args["timeout_secs"].as_u64().unwrap_or(60).clamp(1, 300);
        let start = std::time::Instant::now();

        let mut child = crate::tools::silent_command("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", command])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("启动 PowerShell 失败: {e}"))?;

        // 后台线程读取 stdout/stderr，避免输出量大时死锁
        let stdout_reader = child.stdout.take().map(|mut s| {
            std::thread::spawn(move || {
                use std::io::Read;
                let mut buf = String::new();
                let _ = s.read_to_string(&mut buf);
                buf
            })
        });
        let stderr_reader = child.stderr.take().map(|mut s| {
            std::thread::spawn(move || {
                use std::io::Read;
                let mut buf = String::new();
                let _ = s.read_to_string(&mut buf);
                buf
            })
        });

        let deadline = start + std::time::Duration::from_secs(timeout);
        let status = loop {
            if let Some(st) = child.try_wait().map_err(|e| e.to_string())? {
                break st;
            }
            if std::time::Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("命令超时（{timeout}s），已终止"));
            }
            // 用户点击停止：终止子进程而不是让它跑到底（spawn_blocking 无法中止，只能进程级 kill）
            if global_cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                return Err("任务已被用户停止，命令已终止".to_string());
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };

        let stdout = stdout_reader.and_then(|h| h.join().ok()).unwrap_or_default();
        let stderr = stderr_reader.and_then(|h| h.join().ok()).unwrap_or_default();
        let exit_code = status.code().unwrap_or(-1);
        let duration_ms = start.elapsed().as_millis() as u64;

        Ok(json!({
            "stdout": truncate_output(stdout),
            "stderr": truncate_output(stderr),
            "exit_code": exit_code,
            "duration_ms": duration_ms,
        }))
    }
}

fn run_shell(cmd: &str) -> Result<String, String> {
    // 危险命令黑名单：命中则拒绝执行（防误删/格式化/关机等）
    if let Some(reason) = dangerous_command(cmd) {
        return Err(format!("命令被安全策略拦截：{reason}"));
    }
    if docker_available() {
        // 沙箱：容器隔离（断网、限内存/CPU，仅挂载工作空间为白名单目录）
        run_in_docker(cmd)
    } else {
        // 降级：宿主机执行（已过 HITL 审批），工作目录限定为工作空间，明确警告
        let mut out = run_on_host(cmd)?;
        out.push_str("\n[沙箱] 警告：Docker 不可用，命令已在宿主机执行（已人工审批；工作目录限定为工作空间）");
        Ok(out)
    }
}

fn docker_available() -> bool {
    crate::tools::silent_command("docker")
        .args(["info", "--format", "{{.ServerVersion}}"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run_in_docker(cmd: &str) -> Result<String, String> {
    let mut args: Vec<String> = vec![
        "run".into(), "--rm".into(),
        "--network".into(), "none".into(),   // 断网隔离
        "--memory".into(), "256m".into(),
        "--cpus".into(), "1".into(),
    ];
    // 目录白名单：仅挂载工作空间（可读写），其余完全隔离；未设工作空间则完全隔离
    let mut workdir = "/workspace".to_string();
    let ws = get_workspace().unwrap_or_default();
    if !ws.is_empty() {
        args.push("-v".into());
        args.push(format!("{}:/workspace", docker_mount_path(&ws)));
    } else {
        workdir = "/".to_string();
    }
    args.push("-w".into());
    args.push(workdir);
    args.extend(["alpine:3.18".into(), "sh".into(), "-c".into(), cmd.to_string()]);

    let mut cmd = silent_command("docker");
    cmd.args(&args);
    let output = run_child_cancellable(cmd)?;
    let mut result = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.is_empty() {
        result.push_str("\n[stderr]\n");
        result.push_str(&stderr);
    }
    result.push_str("\n[沙箱] 已在 Docker 容器中隔离执行");
    Ok(truncate_output(result))
}

fn run_on_host(cmd: &str) -> Result<String, String> {
    #[cfg(windows)]
    let mut proc = crate::tools::silent_command("cmd");
    #[cfg(not(windows))]
    let mut proc = std::process::Command::new("sh");

    // 限定工作目录为工作空间，缩小宿主降级执行的波及范围
    if let Some(ws) = get_workspace() {
        if !ws.is_empty() {
            proc.current_dir(std::path::Path::new(&ws));
        }
    }

    #[cfg(windows)]
    proc.args(["/c", cmd]);
    #[cfg(not(windows))]
    proc.args(["-c", cmd]);

    let output = run_child_cancellable(proc).map_err(|e| format!("执行失败: {e}"))?;
    let mut result = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.is_empty() {
        result.push_str("\n[stderr]\n");
        result.push_str(&stderr);
    }
    Ok(truncate_output(result))
}

/// 危险命令黑名单：命中返回原因（用于沙箱前的第一道拦截）
fn dangerous_command(cmd: &str) -> Option<&'static str> {
    let lower = cmd.to_lowercase();
    const PATTERNS: &[(&str, &str)] = &[
        ("rm -rf /", "删除根目录"),
        ("rm -fr /", "删除根目录"),
        (":(){ :|:& };:", "fork 炸弹"),
        ("mkfs", "格式化文件系统"),
        ("shutdown", "关机"),
        ("reboot", "重启"),
        ("> /dev/sd", "覆写磁盘设备"),
        ("/dev/sda", "操作磁盘设备"),
        ("format c:", "格式化磁盘"),
        ("del /f /s c:\\", "删除系统盘"),
        ("rd /s /q c:\\", "删除系统盘"),
    ];
    for (pat, reason) in PATTERNS {
        if lower.contains(pat) {
            return Some(reason);
        }
    }
    None
}

/// Windows 路径转 Docker Desktop 挂载格式：`F:\dir` → `F:/dir`
fn docker_mount_path(p: &str) -> String {
    p.replace('\\', "/")
}

fn truncate_output(mut s: String) -> String {
    if s.chars().count() > 4000 {
        s = s.chars().take(4000).collect();
        s.push_str("\n...(已截断)");
    }
    s
}

// ---------------- P1「网」：HTTP 客户端 + 多渠道推送 ----------------

/// 通用 HTTP API 客户端
pub struct HttpRequestTool;

impl Tool for HttpRequestTool {
    fn name(&self) -> &str {
        "http_request"
    }
    fn description(&self) -> &str {
        "发送 HTTP 请求（GET/POST/PUT/DELETE/PATCH），支持自定义 headers 与 JSON/文本 body，自动解析响应（JSON 优先）。用于调用外部 API、Webhook、数据接口"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "method": { "type": "string", "enum": ["GET", "POST", "PUT", "DELETE", "PATCH"], "description": "HTTP 方法" },
                "url": { "type": "string", "description": "完整 URL" },
                "headers": { "type": "object", "description": "自定义请求头（键值对，可选）" },
                "body": { "type": "string", "description": "请求体（POST/PUT/PATCH 时，JSON 字符串或文本）" },
                "timeout_secs": { "type": "integer", "description": "超时秒数，默认 30" }
            },
            "required": ["method", "url"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let method = args["method"].as_str().unwrap_or("GET").to_uppercase();
        let url = args["url"].as_str().ok_or("缺少参数 url")?;
        let timeout = args["timeout_secs"].as_u64().unwrap_or(30).clamp(1, 120);
        let body = args["body"].as_str().unwrap_or("");

        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(timeout))
            .build()
            .map_err(|e| format!("创建客户端失败: {e}"))?;

        let mut req = match method.as_str() {
            "GET" => client.get(url),
            "POST" => client.post(url),
            "PUT" => client.put(url),
            "DELETE" => client.delete(url),
            "PATCH" => client.patch(url),
            _ => return Err(format!("不支持的方法: {method}")),
        };

        if let Some(headers) = args["headers"].as_object() {
            for (k, v) in headers {
                let val = v.as_str().unwrap_or("").to_string();
                req = req.header(k.as_str(), val);
            }
        }
        if !body.is_empty() && method != "GET" && method != "DELETE" {
            req = req.body(body.to_string());
        }

        let resp = req.send().map_err(|e| format!("请求失败: {e}"))?;
        let status = resp.status().as_u16();
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let text = resp.text().map_err(|e| format!("读取响应失败: {e}"))?;
        let json_value = serde_json::from_str::<Value>(&text).ok();

        Ok(json!({
            "status": status,
            "content_type": content_type,
            "body": text.chars().take(4000).collect::<String>(),
            "json": json_value,
        }))
    }
}

/// 多渠道消息推送（企业微信机器人 / 钉钉 / 飞书 / Slack / Telegram / Bark / Server酱）
pub struct NotifyTool;

impl Tool for NotifyTool {
    fn name(&self) -> &str {
        "notify"
    }
    fn description(&self) -> &str {
        "向外部渠道推送消息，把本地提醒发到手机或 IM。channel: wecom 企业微信机器人 / dingtalk 钉钉 / feishu 飞书 / slack / telegram / bark / serverchan。target 填 webhook URL（telegram 填 bot_token:chat_id，bark 填 key，serverchan 填 sendkey）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "channel": { "type": "string", "enum": ["wecom", "dingtalk", "feishu", "slack", "telegram", "bark", "serverchan"], "description": "推送渠道" },
                "target": { "type": "string", "description": "webhook URL 或凭证" },
                "message": { "type": "string", "description": "消息内容" }
            },
            "required": ["channel", "target", "message"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let channel = args["channel"].as_str().ok_or("缺少参数 channel")?;
        let target = args["target"].as_str().ok_or("缺少参数 target")?;
        let message = args["message"].as_str().ok_or("缺少参数 message")?;

        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| format!("创建客户端失败: {e}"))?;

        let resp = match channel {
            "wecom" => client
                .post(target)
                .json(&json!({ "msgtype": "text", "text": { "content": message } }))
                .send(),
            "dingtalk" => client
                .post(target)
                .json(&json!({ "msgtype": "text", "text": { "content": message } }))
                .send(),
            "feishu" => client
                .post(target)
                .json(&json!({ "msg_type": "text", "content": { "text": message } }))
                .send(),
            "slack" => client.post(target).json(&json!({ "text": message })).send(),
            "telegram" => {
                // target 格式: bot_token:chat_id
                let (token, chat_id) = target
                    .split_once(':')
                    .ok_or("telegram 的 target 应为 bot_token:chat_id")?;
                let url = format!("https://api.telegram.org/bot{token}/sendMessage");
                client
                    .post(&url)
                    .json(&json!({ "chat_id": chat_id, "text": message }))
                    .send()
            }
            "bark" => {
                let url = format!("https://api.day.app/{target}/{}", urlencode(message));
                client.get(&url).send()
            }
            "serverchan" => {
                let url = format!("https://sctapi.ftqq.com/{target}.send");
                client
                    .post(&url)
                    .form(&[("title", "白泽提醒"), ("desp", message)])
                    .send()
            }
            _ => return Err(format!("不支持的渠道: {channel}")),
        };

        let resp = resp.map_err(|e| format!("推送失败: {e}"))?;
        let status = resp.status().as_u16();
        let text = resp.text().unwrap_or_default();
        if status >= 200 && status < 300 {
            Ok(json!({ "ok": true, "channel": channel, "status": status, "response": text }))
        } else {
            Err(format!("推送失败（HTTP {status}）: {text}"))
        }
    }
}

/// 简单 URL 编码（用于 Bark 的路径消息）
fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---------------- P1「网」：邮件 + 数据库 ----------------

/// 从凭据库读取「邮件连接」配置：value 为 JSON，可含
/// smtp_host/smtp_port/imap_host/imap_port/username/password/from。
/// connection 名自动尝试 `mail:<名>` 与 `<名>` 两种 key。
fn resolve_mail_connection(store: &MemoryStore, name: &str) -> Result<Value, String> {
    let name = name.trim();
    if name.is_empty() {
        return Ok(json!({}));
    }
    let raw = crate::vault::get_plain(store, &format!("mail:{name}"))
        .or_else(|_| crate::vault::get_plain(store, name))
        .map_err(|e| format!("读取邮件凭据「{name}」失败: {e}"))?;
    let v: Value = serde_json::from_str(raw.trim())
        .map_err(|_| "邮件凭据不是合法 JSON（应为 {\"smtp_host\":…,\"username\":…,\"password\":…,…}）".to_string())?;
    // 非对象（如存了个纯字符串）按空配置处理，避免索引 panic
    if v.is_object() {
        Ok(v)
    } else {
        Ok(json!({}))
    }
}

/// 解码 RFC 2047 编码词（=?charset?B/Q?data?=），支持 UTF-8 / GBK / GB18030 / Big5 等常见字符集；
/// 相邻编码词（仅空白分隔）解码后直接拼接，未编码文本原样保留。用于修复中文主题/发件人乱码。
fn decode_mime_words(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = String::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let starts_word = bytes[i] == b'=' && i + 1 < bytes.len() && bytes[i + 1] == b'?';
        if starts_word {
            if let Some((decoded, consumed)) = parse_encoded_word(&raw[i..]) {
                out.push_str(&decoded);
                i += consumed;
                // 仅当空白后紧跟下一个编码词时才吃掉这段空白（RFC 2047 拼接规则）
                let mut j = i;
                while j < bytes.len() && matches!(bytes[j], b' ' | b'\t' | b'\r' | b'\n') {
                    j += 1;
                }
                if j + 1 < bytes.len() && bytes[j] == b'=' && bytes[j + 1] == b'?' {
                    i = j;
                }
                continue;
            }
        }
        // 普通字符原样复制
        let ch = raw[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// 结构化解析单个编码词 `=?charset?enc?data?=`，返回 (解码文本, 消耗字节数)。
/// 注意 data 里不允许出现 '?'（Q 编码中问号转义为 =3F），因此数据段的 `?=` 一定是结尾。
fn parse_encoded_word(s: &str) -> Option<(String, usize)> {
    let after = s.get(2..)?; // charset?enc?data?=
    let c1 = after.find('?')?;
    let charset = &after[..c1];
    let after2 = after.get(c1 + 1..)?; // enc?data?=
    let b2 = after2.as_bytes();
    if b2.len() < 3 || b2[1] != b'?' {
        return None;
    }
    let enc = &after2[0..1];
    if enc.to_ascii_uppercase() != "B" && enc.to_ascii_uppercase() != "Q" {
        return None;
    }
    let data_part = &after2[2..];
    let e = data_part.find("?=")?;
    let data = &data_part[..e];
    if data.contains('?') {
        return None;
    }
    let consumed = 2 + c1 + 3 + e + 2; // "=?" + charset + "?enc?" + data + "?="
    let decoded = decode_one_word(charset, enc, data)?;
    Some((decoded, consumed))
}

/// 解码单个编码词：enc=B（base64）/ Q（下划线转空格 + =XX 十六进制）
fn decode_one_word(charset: &str, enc: &str, data: &str) -> Option<String> {
    let raw: Vec<u8> = match enc.to_ascii_uppercase().as_str() {
        "B" => {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.decode(data).ok()?
        }
        "Q" => {
            let b = data.as_bytes();
            let mut out = Vec::with_capacity(b.len());
            let mut i = 0usize;
            while i < b.len() {
                match b[i] {
                    b'_' => {
                        out.push(b' ');
                        i += 1;
                    }
                    b'=' if i + 3 <= b.len() => {
                        out.push(u8::from_str_radix(&data[i + 1..i + 3], 16).ok()?);
                        i += 3;
                    }
                    c => {
                        out.push(c);
                        i += 1;
                    }
                }
            }
            out
        }
        _ => return None,
    };
    let cs = charset.trim();
    if cs.eq_ignore_ascii_case("utf-8") || cs.eq_ignore_ascii_case("us-ascii") {
        Some(String::from_utf8_lossy(&raw).into_owned())
    } else {
        let enc = encoding_rs::Encoding::for_label(cs.as_bytes())?;
        let (decoded, _, _) = enc.decode(&raw);
        Some(decoded.into_owned())
    }
}

/// 邮件发送（SMTP）
pub struct MailSendTool {
    store: Arc<MemoryStore>,
}

impl MailSendTool {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self { store }
    }
}

impl Tool for MailSendTool {
    fn name(&self) -> &str {
        "mail_send"
    }
    fn description(&self) -> &str {
        "通过 SMTP 发送邮件（发报告、通知等）。可直接传 SMTP 配置，或传 connection 名称从凭据库读取已保存的配置（vault_set 存 JSON：{\"smtp_host\":…,\"smtp_port\":…,\"username\":…,\"password\":…,\"from\":…}，key 为 mail:名称）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "connection": { "type": "string", "description": "凭据库名称：读取已保存的 SMTP 配置（直传参数优先）" },
                "smtp_host": { "type": "string", "description": "SMTP 服务器地址，如 smtp.qq.com" },
                "smtp_port": { "type": "integer", "description": "SMTP 端口，如 465/587" },
                "username": { "type": "string", "description": "登录账号" },
                "password": { "type": "string", "description": "密码或授权码" },
                "from": { "type": "string", "description": "发件人邮箱" },
                "to": { "type": "string", "description": "收件人邮箱" },
                "subject": { "type": "string", "description": "主题" },
                "body": { "type": "string", "description": "正文" }
            }
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let conn_name = args["connection"].as_str().unwrap_or("").trim().to_string();
        let stored = resolve_mail_connection(&self.store, &conn_name)?;
        let get_arg = |k: &str| -> Option<String> {
            args[k]
                .as_str()
                .map(|s| s.to_string())
                .or_else(|| stored[k].as_str().map(|s| s.to_string()))
        };
        let smtp_host = get_arg("smtp_host")
            .ok_or("缺少 SMTP 服务器（传 smtp_host 或 connection）")?;
        let smtp_port = args["smtp_port"]
            .as_u64()
            .or_else(|| stored["smtp_port"].as_u64())
            .unwrap_or(587) as u16;
        let username =
            get_arg("username").ok_or("缺少登录账号（username）")?;
        let password =
            get_arg("password").ok_or("缺少密码/授权码（password）")?;
        let from = get_arg("from").ok_or("缺少发件人（from）")?;
        let to = get_arg("to").ok_or("缺少收件人（to）")?;
        let subject = args["subject"].as_str().unwrap_or("");
        let body = args["body"].as_str().unwrap_or("");

        use lettre::message::header::ContentType;
        use lettre::transport::smtp::authentication::Credentials;
        use lettre::{Message, SmtpTransport, Transport};

        let email = Message::builder()
            .from(from.parse().map_err(|e| format!("发件人格式错误: {e}"))?)
            .to(to.parse().map_err(|e| format!("收件人格式错误: {e}"))?)
            .subject(subject)
            .header(ContentType::TEXT_PLAIN)
            .body(body.to_string())
            .map_err(|e| format!("构建邮件失败: {e}"))?;

        let creds = Credentials::new(username, password);
        let mailer = SmtpTransport::relay(&smtp_host)
            .map_err(|e| format!("SMTP 服务器错误: {e}"))?
            .port(smtp_port)
            .credentials(creds)
            .build();

        mailer.send(&email).map_err(|e| format!("发送失败: {e}"))?;
        Ok(json!({ "ok": true, "to": to }))
    }
}

/// 解析 RFC822 header 中的字段（简单按行匹配）
fn parse_header_field(header: &str, field: &str) -> String {
    for line in header.lines() {
        if let Some(rest) = line.strip_prefix(&format!("{field}:")) {
            return rest.trim().to_string();
        }
    }
    String::new()
}

/// 邮件收取（IMAP）
pub struct MailFetchTool {
    store: Arc<MemoryStore>,
}

impl MailFetchTool {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self { store }
    }
}

impl Tool for MailFetchTool {
    fn name(&self) -> &str {
        "mail_fetch"
    }
    fn description(&self) -> &str {
        "通过 IMAP 收取邮件（收验证码、读邮件）。返回最近 N 封的发件人/主题/日期/正文预览（主题/发件人自动做 MIME 中文解码）。可直接传 IMAP 配置，或传 connection 名称从凭据库读取（vault_set 存 JSON：{\"imap_host\":…,\"imap_port\":…,\"username\":…,\"password\":…}，key 为 mail:名称）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "connection": { "type": "string", "description": "凭据库名称：读取已保存的 IMAP 配置（直传参数优先）" },
                "imap_host": { "type": "string", "description": "IMAP 服务器地址，如 imap.qq.com" },
                "imap_port": { "type": "integer", "description": "IMAP 端口，如 993" },
                "username": { "type": "string", "description": "登录账号" },
                "password": { "type": "string", "description": "密码或授权码" },
                "folder": { "type": "string", "description": "邮箱文件夹，默认 INBOX" },
                "limit": { "type": "integer", "description": "最多取最近几封，默认 5" }
            }
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let conn_name = args["connection"].as_str().unwrap_or("").trim().to_string();
        let stored = resolve_mail_connection(&self.store, &conn_name)?;
        let get_arg = |k: &str| -> Option<String> {
            args[k]
                .as_str()
                .map(|s| s.to_string())
                .or_else(|| stored[k].as_str().map(|s| s.to_string()))
        };
        let imap_host = get_arg("imap_host")
            .ok_or("缺少 IMAP 服务器（传 imap_host 或 connection）")?;
        let imap_port = args["imap_port"]
            .as_u64()
            .or_else(|| stored["imap_port"].as_u64())
            .unwrap_or(993) as u16;
        let username =
            get_arg("username").ok_or("缺少登录账号（username）")?;
        let password =
            get_arg("password").ok_or("缺少密码/授权码（password）")?;
        let folder = args["folder"].as_str().unwrap_or("INBOX");
        let limit = args["limit"].as_u64().unwrap_or(5).clamp(1, 50) as usize;

        let tls = native_tls::TlsConnector::builder()
            .build()
            .map_err(|e| format!("创建 TLS 连接器失败: {e}"))?;
        let client = imap::connect((imap_host.as_str(), imap_port), &imap_host, &tls)
            .map_err(|e| format!("连接 IMAP 失败: {e}"))?;
        let mut session = client
            .login(&username, &password)
            .map_err(|(e, _)| format!("登录失败: {e}"))?;
        session.select(folder).map_err(|e| format!("选择文件夹失败: {e}"))?;

        let ids = session.search("ALL").map_err(|e| format!("搜索邮件失败: {e}"))?;
        let start = ids.len().saturating_sub(limit);
        let target: Vec<u32> = ids.iter().skip(start).copied().collect();
        if target.is_empty() {
            return Ok(json!({ "ok": true, "count": 0, "mails": [] }));
        }
        let seq = target
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let fetched = session
            .fetch(seq, "(RFC822.HEADER BODY[TEXT])")
            .map_err(|e| format!("获取邮件失败: {e}"))?;

        let mut mails = Vec::new();
        for msg in fetched.iter() {
            let header = msg
                .header()
                .map(|h| String::from_utf8_lossy(h).to_string())
                .unwrap_or_default();
            let body = msg
                .text()
                .map(|t| String::from_utf8_lossy(t).to_string())
                .unwrap_or_default();
            mails.push(json!({
                "from": decode_mime_words(&parse_header_field(&header, "From")),
                "subject": decode_mime_words(&parse_header_field(&header, "Subject")),
                "date": parse_header_field(&header, "Date"),
                "body_preview": body.chars().take(300).collect::<String>(),
            }));
        }
        // 最新的在前
        mails.reverse();
        let _ = session.logout();
        Ok(json!({ "ok": true, "count": mails.len(), "mails": mails }))
    }
}

/// SQLite 值 → 字符串（用于统一结果展示）
fn sqlite_val_to_str(v: rusqlite::types::Value) -> String {
    use rusqlite::types::Value as SV;
    match v {
        SV::Null => "null".to_string(),
        SV::Integer(i) => i.to_string(),
        SV::Real(f) => f.to_string(),
        SV::Text(s) => s,
        SV::Blob(b) => format!("<blob {}B>", b.len()),
    }
}

/// 执行查询，返回 { columns, rows, count }（统一 SQLite/MySQL/PostgreSQL）
fn db_rows(connection: &str, sql: &str) -> Result<Value, String> {
    if connection.starts_with("mysql://") {
        use mysql::prelude::Queryable;
        let pool = mysql::Pool::new(connection).map_err(|e| format!("连接 MySQL 失败: {e}"))?;
        let mut conn = pool.get_conn().map_err(|e| format!("获取连接失败: {e}"))?;
        let rows: Vec<mysql::Row> = conn.query(sql).map_err(|e| format!("查询失败: {e}"))?;
        let columns: Vec<String> = rows
            .first()
            .map(|r| r.columns().iter().map(|c| c.name_str().to_string()).collect())
            .unwrap_or_default();
        let data: Vec<Vec<String>> = rows
            .iter()
            .map(|r| {
                (0..r.columns().len())
                    .map(|i| r.get::<Option<String>, _>(i).unwrap_or(None).unwrap_or_default())
                    .collect()
            })
            .collect();
        Ok(json!({ "columns": columns, "rows": data, "count": data.len() }))
    } else if connection.starts_with("postgres://") || connection.starts_with("postgresql://") {
        let mut client = postgres::Client::connect(connection, postgres::NoTls)
            .map_err(|e| format!("连接 PostgreSQL 失败: {e}"))?;
        let rows = client.query(sql, &[]).map_err(|e| format!("查询失败: {e}"))?;
        let columns: Vec<String> = rows
            .first()
            .map(|r| r.columns().iter().map(|c| c.name().to_string()).collect())
            .unwrap_or_default();
        let data: Vec<Vec<String>> = rows
            .iter()
            .map(|r| {
                (0..r.columns().len())
                    .map(|i| r.try_get::<_, Option<String>>(i).unwrap_or(None).unwrap_or_default())
                    .collect()
            })
            .collect();
        Ok(json!({ "columns": columns, "rows": data, "count": data.len() }))
    } else {
        let conn = rusqlite::Connection::open(connection)
            .map_err(|e| format!("打开 SQLite 失败: {e}"))?;
        let mut stmt = conn.prepare(sql).map_err(|e| format!("准备语句失败: {e}"))?;
        let columns: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let mut q = stmt.query([]).map_err(|e| format!("查询失败: {e}"))?;
        let mut data = Vec::new();
        while let Some(row) = q.next().map_err(|e| e.to_string())? {
            let mut vals = Vec::new();
            for i in 0..columns.len() {
                let v: rusqlite::types::Value = row.get(i).map_err(|e| e.to_string())?;
                vals.push(sqlite_val_to_str(v));
            }
            data.push(vals);
        }
        Ok(json!({ "columns": columns, "rows": data, "count": data.len() }))
    }
}

/// 执行非查询语句（INSERT/UPDATE/DELETE/DDL），返回影响行数
fn db_execute(connection: &str, sql: &str) -> Result<u64, String> {
    if connection.starts_with("mysql://") {
        use mysql::prelude::Queryable;
        let pool = mysql::Pool::new(connection).map_err(|e| format!("连接 MySQL 失败: {e}"))?;
        let mut conn = pool.get_conn().map_err(|e| format!("获取连接失败: {e}"))?;
        conn.query_drop(sql).map_err(|e| format!("执行失败: {e}"))?;
        Ok(conn.affected_rows())
    } else if connection.starts_with("postgres://") || connection.starts_with("postgresql://") {
        let mut client = postgres::Client::connect(connection, postgres::NoTls)
            .map_err(|e| format!("连接 PostgreSQL 失败: {e}"))?;
        let affected = client.execute(sql, &[]).map_err(|e| format!("执行失败: {e}"))?;
        Ok(affected)
    } else {
        let conn = rusqlite::Connection::open(connection)
            .map_err(|e| format!("打开 SQLite 失败: {e}"))?;
        let affected = conn.execute(sql, []).map_err(|e| format!("执行失败: {e}"))?;
        Ok(affected as u64)
    }
}

/// SQLite 标识符加双引号并转义（表名可能含空格/特殊字符）
fn sqlite_quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// 探查数据库结构：返回所有表名及各表字段（name/type），供 LLM 依据真实结构生成 SQL。
/// 支持 SQLite / MySQL / PostgreSQL。
fn db_schema(connection: &str) -> Result<Value, String> {
    if connection.starts_with("mysql://") {
        use mysql::prelude::Queryable;
        let pool = mysql::Pool::new(connection).map_err(|e| format!("连接 MySQL 失败: {e}"))?;
        let mut conn = pool.get_conn().map_err(|e| format!("获取连接失败: {e}"))?;
        let tables: Vec<String> = conn
            .query("SELECT table_name FROM information_schema.tables WHERE table_schema = DATABASE() ORDER BY table_name")
            .map_err(|e| format!("读取表失败: {e}"))?
            .into_iter()
            .filter_map(|r: mysql::Row| r.get::<String, _>(0))
            .collect();
        let mut out = Vec::new();
        for t in tables {
            // 表名来自 information_schema（合法标识符），单引号转义后内联查询，避免 prepare 参数推断歧义
            let sql = format!(
                "SELECT column_name, column_type FROM information_schema.columns WHERE table_schema = DATABASE() AND table_name = '{}' ORDER BY ordinal_position",
                t.replace('\'', "''")
            );
            let rows: Vec<mysql::Row> = conn
                .query(&sql)
                .map_err(|e| format!("读取表 {t} 字段失败: {e}"))?;
            let mut cols: Vec<Value> = Vec::new();
            for row in rows {
                let name = row.get::<String, _>(0).unwrap_or_default();
                let ty = row.get::<String, _>(1).unwrap_or_default();
                cols.push(json!({ "name": name, "type": ty }));
            }
            out.push(json!({ "table": t, "columns": cols }));
        }
        return Ok(json!({ "engine": "mysql", "tables": out }));
    }

    if connection.starts_with("postgres://") || connection.starts_with("postgresql://") {
        let mut client = postgres::Client::connect(connection, postgres::NoTls)
            .map_err(|e| format!("连接 PostgreSQL 失败: {e}"))?;
        let table_rows = client
            .query(
                "SELECT table_name FROM information_schema.tables WHERE table_schema = 'public' ORDER BY table_name",
                &[],
            )
            .map_err(|e| format!("读取表失败: {e}"))?;
        let tables: Vec<String> = table_rows.iter().map(|r| r.get::<_, String>(0)).collect();
        let mut out = Vec::new();
        for t in tables {
            let col_rows = client
                .query(
                    "SELECT column_name, data_type FROM information_schema.columns WHERE table_schema = 'public' AND table_name = $1 ORDER BY ordinal_position",
                    &[&t.as_str()],
                )
                .map_err(|e| format!("读取表 {t} 字段失败: {e}"))?;
            let cols: Vec<Value> = col_rows
                .iter()
                .map(|r| json!({ "name": r.get::<_, String>(0), "type": r.get::<_, String>(1) }))
                .collect();
            out.push(json!({ "table": t, "columns": cols }));
        }
        return Ok(json!({ "engine": "postgresql", "tables": out }));
    }

    // 默认走 SQLite：sqlite_master 拿表名，PRAGMA table_info 拿字段
    let conn = rusqlite::Connection::open(connection)
        .map_err(|e| format!("打开 SQLite 失败: {e}"))?;
    let tables: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .map_err(|e| format!("读取表失败: {e}"))?;
        let names = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| format!("读取表失败: {e}"))?;
        names.filter_map(|x| x.ok()).collect()
    };
    let mut out = Vec::new();
    for t in tables {
        let q = format!("PRAGMA table_info({})", sqlite_quote_ident(&t));
        let cols: Vec<Value> = {
            let mut st2 = conn.prepare(&q).map_err(|e| format!("读取表 {t} 字段失败: {e}"))?;
            let rows = st2
                .query_map([], |r| {
                    Ok(json!({
                        "name": r.get::<_, String>(1)?,
                        "type": r.get::<_, String>(2)?,
                    }))
                })
                .map_err(|e| format!("读取表 {t} 字段失败: {e}"))?;
            rows.filter_map(|x| x.ok()).collect()
        };
        out.push(json!({ "table": t, "columns": cols }));
    }
    Ok(json!({ "engine": "sqlite", "tables": out }))
}

/// 数据库查询工具
pub struct DbQueryTool;

impl Tool for DbQueryTool {
    fn name(&self) -> &str {
        "db_query"
    }
    fn description(&self) -> &str {
        "对数据库执行查询（SELECT），返回列名与行数据。connection 填连接串：SQLite 填文件路径，MySQL 填 mysql://user:pass@host:port/db，PostgreSQL 填 postgres://user:pass@host:port/db。不确定表结构/字段时，先调 db_schema 探查，再据此写 SQL"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "connection": { "type": "string", "description": "连接串" },
                "sql": { "type": "string", "description": "SELECT 查询语句" }
            },
            "required": ["connection", "sql"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let input = args["connection"].as_str().ok_or("缺少参数 connection")?;
        let connection = resolve_db_connection(input);
        let sql = args["sql"].as_str().ok_or("缺少参数 sql")?;
        db_rows(&connection, sql)
    }
}

/// 数据库执行工具（写操作）
pub struct DbExecuteTool;

impl Tool for DbExecuteTool {
    fn name(&self) -> &str {
        "db_execute"
    }
    fn description(&self) -> &str {
        "对数据库执行写操作（INSERT/UPDATE/DELETE/DDL），返回影响行数。connection 格式同 db_query"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "connection": { "type": "string", "description": "连接串" },
                "sql": { "type": "string", "description": "写操作 SQL 语句" }
            },
            "required": ["connection", "sql"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let input = args["connection"].as_str().ok_or("缺少参数 connection")?;
        let connection = resolve_db_connection(input);
        let sql = args["sql"].as_str().ok_or("缺少参数 sql")?;
        let affected = db_execute(&connection, sql)?;
        Ok(json!({ "ok": true, "affected_rows": affected }))
    }
}

/// 数据库 schema 探查工具（自然语言查库的前置：先看有哪些表/字段，再写 SQL）
pub struct DbSchemaTool;

impl Tool for DbSchemaTool {
    fn name(&self) -> &str {
        "db_schema"
    }
    fn description(&self) -> &str {
        "探查数据库结构，返回所有表名及各表字段（name/type），供你依据真实结构写 SQL。当用户用自然语言提问（如「查上月销售额最高的客户」）时，先调本工具了解有哪些表和字段，再据此写 db_query 的 SQL。connection 格式同 db_query（SQLite 文件路径 / mysql:// / postgres://）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "connection": { "type": "string", "description": "连接串（SQLite 文件路径 / mysql://user:pass@host:port/db / postgres://...）" }
            },
            "required": ["connection"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let input = args["connection"].as_str().ok_or("缺少参数 connection")?;
        let connection = resolve_db_connection(input);
        db_schema(&connection)
    }
}

// ---------------- 写文件工具单元测试 ----------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// 生成一个独一无二的临时目录路径（不创建）
    fn tmpdir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("baize_tooltest_{tag}_{nanos}"))
    }

    #[test]
    fn write_file_creates_parent_and_writes() {
        let dir = tmpdir("write");
        let file = dir.join("a").join("b").join("c.txt");
        let _ = std::fs::remove_dir_all(&dir);

        let out = WriteFileTool
            .run(json!({ "path": file.display().to_string(), "content": "hello" }))
            .unwrap();
        assert_eq!(out["ok"].as_bool(), Some(true));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 唯一匹配时默认替换一次（多次出现需唯一上下文或 replace_all，见 requires_unique 用例）
    #[test]
    fn edit_file_replaces_once_by_default() {
        let dir = tmpdir("edit");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("f.txt");
        std::fs::write(&file, "foo bar baz").unwrap();

        EditFileTool
            .run(json!({ "path": file.display().to_string(), "old_string": "foo", "new_string": "X" }))
            .unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "X bar baz");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn edit_file_requires_unique_old_string() {
        let dir = tmpdir("edit_unique");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("f.txt");
        std::fs::write(&file, "foo bar foo").unwrap();

        let r = EditFileTool.run(json!({ "path": file.display().to_string(), "old_string": "foo", "new_string": "X" }));
        assert!(r.is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn edit_file_replace_all() {
        let dir = tmpdir("edit_all");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("f.txt");
        std::fs::write(&file, "foo foo").unwrap();

        EditFileTool
            .run(json!({ "path": file.display().to_string(), "old_string": "foo", "new_string": "X", "replace_all": true }))
            .unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "X X");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn edit_file_missing_old_string_errors() {
        let dir = tmpdir("edit_miss");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("f.txt");
        std::fs::write(&file, "abc").unwrap();

        let r = EditFileTool.run(json!({ "path": file.display().to_string(), "old_string": "zzz", "new_string": "y" }));
        assert!(r.is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn create_directory_is_idempotent() {
        let dir = tmpdir("mkdir");
        let target = dir.join("x").join("y");
        let _ = std::fs::remove_dir_all(&dir);

        // 连续两次创建应都成功（幂等）
        CreateDirectoryTool
            .run(json!({ "path": target.display().to_string() }))
            .unwrap();
        CreateDirectoryTool
            .run(json!({ "path": target.display().to_string() }))
            .unwrap();
        assert!(target.is_dir());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn move_file_renames_and_creates_target_parent() {
        let dir = tmpdir("move");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("a.txt");
        std::fs::write(&src, "data").unwrap();
        let dst = dir.join("sub").join("b.txt");

        MoveFileTool
            .run(json!({ "from": src.display().to_string(), "to": dst.display().to_string() }))
            .unwrap();
        assert!(!src.exists());
        assert_eq!(std::fs::read_to_string(&dst).unwrap(), "data");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod mime_decode_tests {
    use super::decode_mime_words;

    #[test]
    fn utf8_base64_word() {
        assert_eq!(decode_mime_words("=?UTF-8?B?5L2g5aW95LiW55WM?="), "你好世界");
    }

    #[test]
    fn utf8_q_word() {
        assert_eq!(decode_mime_words("=?utf-8?Q?=E4=BD=A0=E5=A5=BD?="), "你好");
    }

    #[test]
    fn gbk_word() {
        assert_eq!(decode_mime_words("=?GBK?B?suLK1A==?="), "测试");
    }

    #[test]
    fn adjacent_words_concatenate() {
        // 两个相邻编码词之间的空白按 RFC 2047 应被吞掉
        assert_eq!(
            decode_mime_words("=?UTF-8?B?6aG555uu?= =?UTF-8?B?5ZGo5oql?="),
            "项目周报"
        );
    }

    #[test]
    fn plain_text_and_mixed() {
        assert_eq!(decode_mime_words("Re: =?UTF-8?B?5L2g5aW9?=?"), "Re: 你好?");
        assert_eq!(decode_mime_words("纯文本主题"), "纯文本主题");
    }

    #[test]
    fn folded_header_with_prefix() {
        assert_eq!(
            decode_mime_words("xxx =?UTF-8?Q?=E5=9B=9E=E5=A4=8D?=: ok"),
            "xxx 回复: ok"
        );
    }
}
