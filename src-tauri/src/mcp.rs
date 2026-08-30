//! MCP（Model Context Protocol）客户端 —— stdio 传输
//!
//! 自实现的最小 JSON-RPC 2.0 over stdio 客户端：启动 MCP 服务器进程，
//! 完成 initialize → tools/list，把外部 MCP 工具包装成白泽 `Tool`。
//!
//! 说明：M1 用同步阻塞 I/O（每次工具调用做一次进程 stdio 往返）；生产版
//! 可换 async + 独立读写任务。权限启发式：读类工具只读，其余写（需审批）。

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::tools::{PermissionClass, Tool};

/// MCP 客户端配置（前端可编辑、可持久化、可运行时重建）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct McpConfig {
    pub enabled: bool,
    pub command: String,
    pub args: Vec<String>,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            command: "npx".to_string(),
            args: vec![
                "-y".to_string(),
                "@modelcontextprotocol/server-filesystem".to_string(),
                "D:\\".to_string(),
                "C:\\Users\\OMEN".to_string(),
            ],
        }
    }
}

impl McpConfig {
    /// 从环境变量构建初始配置（作为默认值；持久化配置会覆盖）
    pub fn from_env() -> Self {
        let mut c = Self::default();
        if let Ok(cmd) = std::env::var("BAIZE_MCP_COMMAND") {
            if !cmd.is_empty() {
                c.enabled = true;
                c.command = cmd;
                c.args = std::env::var("BAIZE_MCP_ARGS")
                    .map(|s| {
                        s.split(',')
                            .map(|x| x.trim().to_string())
                            .filter(|x| !x.is_empty())
                            .collect()
                    })
                    .unwrap_or_default();
            }
        }
        c
    }
}

/// 跨平台启动 MCP 服务器子进程
fn spawn(command: &str, args: &[String]) -> Result<Child, String> {
    #[cfg(windows)]
    {
        // Windows 上 npx/npm 等是 .cmd，需经 cmd.exe 执行
        let mut c = Command::new("cmd");
        c.arg("/c").arg(command).args(args);
        c.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("启动 MCP 服务器失败: {e}"))
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new(command);
        c.args(args);
        c.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("启动 MCP 服务器失败: {e}"))
    }
}

/// MCP 服务器的连接状态（进程 + stdio 句柄）
struct McpInner {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

/// 释放时终止 MCP 子进程（避免留下孤儿进程）
impl Drop for McpInner {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// JSON-RPC 往返：发送请求，读取匹配 id 的响应（跳过通知）
fn rpc_call(inner: &mut McpInner, method: &str, params: Value) -> Result<Value, String> {
    let id = inner.next_id;
    inner.next_id += 1;
    let req = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    writeln!(inner.stdin, "{}", req).map_err(|e| e.to_string())?;
    inner.stdin.flush().map_err(|e| e.to_string())?;

    loop {
        let mut line = String::new();
        let n = inner
            .stdout
            .read_line(&mut line)
            .map_err(|e| format!("读取 MCP 响应失败: {e}"))?;
        if n == 0 {
            return Err("MCP 服务器已关闭".to_string());
        }
        let v: Value =
            serde_json::from_str(line.trim()).map_err(|e| format!("解析 MCP 响应失败: {e}"))?;
        if v.get("id").is_none() {
            continue; // 跳过通知
        }
        if v["id"] == json!(id) {
            if let Some(err) = v.get("error") {
                return Err(format!("MCP 错误: {err}"));
            }
            return Ok(v["result"].clone());
        }
    }
}

#[derive(Debug, Clone)]
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

/// MCP 客户端：持有共享连接 + 工具清单
pub struct McpClient {
    inner: Arc<Mutex<McpInner>>,
    tools: Vec<McpToolInfo>,
}

impl McpClient {
    /// 启动 MCP 服务器并完成握手与工具发现
    pub fn connect(command: &str, args: &[String]) -> Result<Self, String> {
        let mut child = spawn(command, args)?;
        let stdin = child.stdin.take().ok_or("无法获取 MCP stdin")?;
        let stdout = child.stdout.take().ok_or("无法获取 MCP stdout")?;

        let client = McpClient {
            inner: Arc::new(Mutex::new(McpInner {
                child,
                stdin,
                stdout: BufReader::new(stdout),
                next_id: 1,
            })),
            tools: Vec::new(),
        };

        // 1) 初始化握手
        client.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "baize", "version": "0.1.0" }
            }),
        )?;
        client.notify("notifications/initialized", json!({}))?;

        // 2) 拉取工具清单
        let resp = client.request("tools/list", json!({}))?;
        let tools = parse_tools(&resp)?;

        Ok(McpClient {
            inner: client.inner,
            tools,
        })
    }

    fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let mut inner = self.inner.lock().unwrap();
        rpc_call(&mut inner, method, params)
    }

    fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap();
        let req = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        writeln!(inner.stdin, "{}", req).map_err(|e| e.to_string())?;
        inner.stdin.flush().map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn tools(&self) -> &[McpToolInfo] {
        &self.tools
    }

    /// 把每个 MCP 工具包装成可注册进 ToolRegistry 的适配器
    pub fn into_adapters(self) -> Vec<McpToolAdapter> {
        let inner = self.inner.clone();
        self.tools
            .into_iter()
            .map(|info| McpToolAdapter {
                info,
                inner: inner.clone(),
            })
            .collect()
    }
}

/// 将单个 MCP 工具包装为白泽 Tool
pub struct McpToolAdapter {
    info: McpToolInfo,
    inner: Arc<Mutex<McpInner>>,
}

impl Tool for McpToolAdapter {
    fn name(&self) -> &str {
        &self.info.name
    }
    fn description(&self) -> &str {
        &self.info.description
    }
    fn schema(&self) -> Value {
        self.info.schema.clone()
    }
    fn permission(&self) -> PermissionClass {
        // 启发式：读/查类工具只读；其余写（需审批）
        let n = self.info.name.to_ascii_lowercase();
        let readonly = ["read", "list", "search", "get", "tree", "info", "directory"]
            .iter()
            .any(|k| n.contains(k));
        if readonly {
            PermissionClass::ReadOnly
        } else {
            PermissionClass::Write
        }
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let mut inner = self.inner.lock().unwrap();
        let resp = rpc_call(
            &mut inner,
            "tools/call",
            json!({ "name": self.info.name, "arguments": args }),
        )?;
        Ok(extract_text(resp))
    }
}

/// 从 tools/call 结果里提取文本（拼接 content[].text），便于模型阅读
fn extract_text(resp: Value) -> Value {
    if let Some(content) = resp.get("content").and_then(|c| c.as_array()) {
        let texts: Vec<String> = content
            .iter()
            .filter_map(|item| item.get("text").and_then(|t| t.as_str()).map(|s| s.to_string()))
            .collect();
        if !texts.is_empty() {
            return json!(texts.join("\n"));
        }
    }
    resp
}

fn parse_tools(resp: &Value) -> Result<Vec<McpToolInfo>, String> {
    let arr = resp
        .get("tools")
        .and_then(|t| t.as_array())
        .ok_or("tools/list 响应缺少 tools 数组")?;
    let mut out = Vec::new();
    for t in arr {
        out.push(McpToolInfo {
            name: t["name"].as_str().unwrap_or("").to_string(),
            description: t["description"].as_str().unwrap_or("").to_string(),
            schema: t
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
        });
    }
    Ok(out)
}
