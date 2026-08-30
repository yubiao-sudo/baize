use std::sync::Arc;

use serde_json::{json, Value};

use crate::tools::{PermissionClass, Tool, ToolRegistry};

/// 插件工具（manifest 中的一项）
pub struct PluginTool {
    name: String,
    description: String,
    parameters: Value,
    command: String,
}

/// 从 manifest JSON 解析插件工具列表
pub fn load_plugin_manifest(json_str: &str) -> Result<Vec<PluginTool>, String> {
    let v: Value = serde_json::from_str(json_str).map_err(|e| format!("解析 manifest 失败: {e}"))?;
    let tools = v["tools"].as_array().ok_or("manifest 缺少 tools 数组")?;
    let mut out = Vec::new();
    for t in tools {
        let name = t["name"].as_str().unwrap_or("").to_string();
        let description = t["description"].as_str().unwrap_or("").to_string();
        let parameters = t
            .get("parameters")
            .cloned()
            .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
        let command = t["command"].as_str().unwrap_or("").to_string();
        if !name.is_empty() && !command.is_empty() {
            out.push(PluginTool {
                name,
                description,
                parameters,
                command,
            });
        }
    }
    Ok(out)
}

impl Tool for PluginTool {
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
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        // 占位符替换：{key} → 参数值
        let mut cmd = self.command.clone();
        if let Some(obj) = args.as_object() {
            for (k, v) in obj {
                let val = match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                cmd = cmd.replace(&format!("{{{k}}}"), &val);
            }
        }
        let stdout = run_command(&cmd);
        Ok(json!({ "stdout": stdout }))
    }
}

pub(crate) fn run_command(cmd: &str) -> String {
    #[cfg(windows)]
    let output = std::process::Command::new("cmd").args(["/c", cmd]).output();
    #[cfg(not(windows))]
    let output = std::process::Command::new("sh").args(["-c", cmd]).output();

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

/// plugin_load 工具：加载 manifest 插件并注册其工具
pub struct PluginLoadTool {
    tools: Arc<ToolRegistry>,
}

impl PluginLoadTool {
    pub fn new(tools: Arc<ToolRegistry>) -> Self {
        Self { tools }
    }
}

impl Tool for PluginLoadTool {
    fn name(&self) -> &str {
        "plugin_load"
    }
    fn description(&self) -> &str {
        "加载一个插件 manifest（JSON 文件），注册其中定义的命令工具（供后续调用）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "插件 manifest JSON 文件的绝对路径" }
            },
            "required": ["path"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let path = args["path"].as_str().ok_or("缺少参数 path")?;
        let json =
            std::fs::read_to_string(path).map_err(|e| format!("读取 manifest 失败: {e}"))?;
        let plugins = load_plugin_manifest(&json)?;
        let count = plugins.len();
        for p in plugins {
            self.tools.register_ns("plugin", Box::new(p));
        }
        Ok(json!({ "ok": true, "loaded": count }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_manifest_parses_tools() {
        let json =
            r#"{"name":"test","tools":[{"name":"hello","description":"say hi","command":"echo {name}"}]}"#;
        let tools = load_plugin_manifest(json).unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "hello");
    }
}
