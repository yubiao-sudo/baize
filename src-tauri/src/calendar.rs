//! 日历感知调度：读取本地 Outlook/系统日历。
//!
//! 通过 PowerShell 调用 Outlook COM 对象模型（`Outlook.Application`）读取默认日历，
//! 无需联网、无需额外依赖；Outlook 未安装或 COM 启动失败时返回清晰指引。
//!
//! 工具：
//!   - [`CalendarEventsTool`]（`calendar_events`）：读取未来 N 天的日历事件（含重复事件展开），
//!     返回结构化 JSON（主题/开始/结束/地点/会议类型）。Agent 可据此感知日程并配合 set_reminder 提醒。

use serde_json::{json, Value};

use crate::tools::{PermissionClass, Tool};

/// 读取日历的 PowerShell 脚本（写到临时文件再用 -File 执行，避免命令行引号转义问题）
const CAL_SCRIPT: &str = r#"
param([int]$Days = 7)
$ErrorActionPreference = 'Stop'
try {
    $ol = New-Object -ComObject Outlook.Application
} catch {
    Write-Output '{"ok":false,"error":"未检测到已安装的 Outlook 桌面版（Outlook 未安装或 COM 启动失败）"}'
    exit 0
}
$ns = $ol.GetNamespace('MAPI')
$folder = $ns.GetDefaultFolder(9)          # olFolderCalendar
$items = $folder.Items
$items.IncludeRecurrences = $true
$items.Sort('[Start]')
$start = (Get-Date).Date
$end = $start.AddDays($Days)
$filter = "[Start] >= '" + $start.ToString('yyyy-MM-dd HH:mm') + "' AND [Start] < '" + $end.ToString('yyyy-MM-dd HH:mm') + "'"
$rows = @()
foreach ($it in $items.Restrict($filter)) {
    if (-not $it.Start -or $it.Start.Year -gt 4500) { continue }   # 跳过无有效开始时间的重复项
    $kind = switch ($it.MeetingStatus) { 1 {'会议'} 2 {'受邀会议'} 3 {'已取消'} 4 {'已取消'} default {'日程'} }
    $rows += [PSCustomObject]@{
        subject   = [string]$it.Subject
        start     = $it.Start.ToString('yyyy-MM-dd HH:mm')
        end       = if ($it.End) { $it.End.ToString('yyyy-MM-dd HH:mm') } else { '' }
        location  = [string]$it.Location
        kind      = $kind
        recurring = [bool]$it.IsRecurring
    }
}
@{ ok = $true; days = $Days; count = @($rows).Count; events = @($rows) } | ConvertTo-Json -Depth 5
"#;

/// 运行日历读取脚本，返回 stdout 原文
fn run_calendar_script(days: u64) -> Result<String, String> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let script_path = std::env::temp_dir().join(format!("baize_cal_{ts}.ps1"));
    std::fs::write(&script_path, CAL_SCRIPT).map_err(|e| format!("写入临时脚本失败: {e}"))?;

    let script_str = script_path.to_string_lossy().to_string();

    // 探测 powershell（Windows 自带）或 pwsh
    let mut shell = "powershell";
    let mut ok = crate::tools::silent_command(shell)
        .args(["-NoProfile", "-Command", "exit 0"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        shell = "pwsh";
        ok = crate::tools::silent_command(shell)
            .args(["-NoProfile", "-Command", "exit 0"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
    }

    let out = if ok {
        crate::tools::silent_command(shell)
            .args([
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
                &script_str,
                "-Days",
                &days.to_string(),
            ])
            .output()
    } else {
        let _ = std::fs::remove_file(&script_path);
        return Err("未检测到 PowerShell，无法读取 Outlook 日历".to_string());
    };

    let _ = std::fs::remove_file(&script_path);

    let out = out.map_err(|e| format!("启动 {shell} 失败: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!("读取日历失败: {err}"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// 日历事件工具（本地 Outlook/系统日历，只读）
pub struct CalendarEventsTool;

impl Tool for CalendarEventsTool {
    fn name(&self) -> &str {
        "calendar_events"
    }
    fn description(&self) -> &str {
        "读取本地 Outlook/系统日历中未来 N 天的日程事件（含重复事件的展开实例），返回结构化 JSON（主题/开始/结束/地点/会议类型）。\
         用于「今天/本周有什么安排」「帮我看看明天的会议」。days 向前看天数（默认 7）。需本机安装并登录 Outlook 桌面版。"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "days": { "type": "integer", "description": "向前看 N 天，默认 7" }
            }
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let days = args["days"].as_u64().unwrap_or(7).clamp(1, 365);
        let raw = run_calendar_script(days)?;
        serde_json::from_str::<Value>(raw.trim())
            .map_err(|e| format!("解析日历结果失败: {e}（原始输出: {}）", raw.trim()))
    }
}