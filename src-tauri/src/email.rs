//! 邮件能力：读取与发送本地 Outlook 邮件。
//!
//! 与 [`crate::calendar`] 一致，通过 PowerShell 调用 Outlook COM（`Outlook.Application`），
//! 本地、免联网、零额外依赖；Outlook 未安装时返回清晰指引。
//!
//! 工具：
//!   - [`ListMailTool`]（`list_mail`，只读）：读取收件箱最近 N 封（标题/发件人/时间/已读/附件/摘要）。
//!   - [`SendMailTool`]（`send_mail`，高危需审批）：按收件人/主题/正文发送邮件。

use std::process::Command;

use serde_json::{json, Value};

use crate::tools::{PermissionClass, Tool};

/// 读取收件箱的 PowerShell 脚本（-Count 为简单整数，安全走 -File 参数）
const LIST_MAIL_SCRIPT: &str = r#"
param([int]$Count = 20)
$ErrorActionPreference = 'Stop'
try {
    $ol = New-Object -ComObject Outlook.Application
} catch {
    Write-Output '{"ok":false,"error":"未检测到已安装的 Outlook 桌面版（Outlook 未安装或 COM 启动失败）"}'
    exit 0
}
$ns = $ol.GetNamespace('MAPI')
$inbox = $ns.GetDefaultFolder(6)          # olFolderInbox
$items = $inbox.Items
$items.Sort('[ReceivedTime]', $true)
$rows = @()
$i = 0
foreach ($it in $items) {
    if ($i -ge $Count) { break }
    $i++
    $body = [string]$it.Body
    if ($body.Length -gt 200) { $body = $body.Substring(0, 200) + '…' }
    $rows += [PSCustomObject]@{
        subject        = [string]$it.Subject
        from           = [string]$it.SenderName
        addr           = [string]$it.SenderEmailAddress
        time           = if ($it.ReceivedTime) { $it.ReceivedTime.ToString('yyyy-MM-dd HH:mm') } else { '' }
        unread         = [bool]$it.UnRead
        has_attachment = [bool]($it.Attachments.Count -gt 0)
        body           = $body
    }
}
@{ ok = $true; folder = '收件箱'; count = @($rows).Count; mails = @($rows) } | ConvertTo-Json -Depth 5
"#;

/// 发送邮件的 PowerShell 脚本（正文经 JSON 参数文件读取，避免命令行引号/换行转义问题）
const SEND_MAIL_SCRIPT: &str = r#"
param([string]$ParamsFile)
$ErrorActionPreference = 'Stop'
$p = Get-Content -Raw -Encoding UTF8 $ParamsFile | ConvertFrom-Json
try {
    $ol = New-Object -ComObject Outlook.Application
} catch {
    Write-Output '{"ok":false,"error":"未检测到已安装的 Outlook 桌面版（Outlook 未安装或 COM 启动失败）"}'
    exit 0
}
if (-not $p.to) { Write-Output '{"ok":false,"error":"缺少收件人 to"}'; exit 0 }
$mail = $ol.CreateItem(0)                    # olMailItem
$mail.To = [string]$p.to
if ($p.cc)  { $mail.CC  = [string]$p.cc }
if ($p.bcc) { $mail.BCC = [string]$p.bcc }
$mail.Subject = [string]$p.subject
$mail.HTMLBody = ([string]$p.body) -replace "`n", '<br/>'
$mail.Send()
@{ ok = $true; to = [string]$p.to; subject = [string]$p.subject } | ConvertTo-Json -Depth 3
"#;

/// 运行 PowerShell：把脚本写到临时 .ps1，用 -File 执行并返回 stdout
fn run_powershell(script: &str, script_args: &[String]) -> Result<String, String> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let script_path = std::env::temp_dir().join(format!("baize_mail_{ts}.ps1"));
    std::fs::write(&script_path, script).map_err(|e| format!("写入临时脚本失败: {e}"))?;
    let script_str = script_path.to_string_lossy().to_string();

    let mut shell = "powershell";
    let mut ok = Command::new(shell)
        .args(["-NoProfile", "-Command", "exit 0"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        shell = "pwsh";
        ok = Command::new(shell)
            .args(["-NoProfile", "-Command", "exit 0"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
    }

    let out = if ok {
        let mut cmd = Command::new(shell);
        cmd.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", &script_str]);
        for a in script_args {
            cmd.arg(a);
        }
        cmd.output()
    } else {
        let _ = std::fs::remove_file(&script_path);
        return Err("未检测到 PowerShell，无法访问 Outlook 邮件".to_string());
    };

    let _ = std::fs::remove_file(&script_path);

    let out = out.map_err(|e| format!("启动 {shell} 失败: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!("邮件操作失败: {err}"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// 读取收件箱工具（只读）
pub struct ListMailTool;

impl Tool for ListMailTool {
    fn name(&self) -> &str {
        "list_mail"
    }
    fn description(&self) -> &str {
        "读取本地 Outlook 收件箱最近的邮件（标题/发件人/时间/已读/附件/正文摘要），返回结构化 JSON。\
         用于「帮我看看有没有新邮件」「最近谁发了我什么」。count 读取条数（默认 20）。需本机安装并登录 Outlook。"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "count": { "type": "integer", "description": "读取最近 N 封，默认 20" }
            }
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let count = args["count"].as_u64().unwrap_or(20).clamp(1, 200);
        let raw = run_powershell(LIST_MAIL_SCRIPT, &[count.to_string()])?;
        serde_json::from_str::<Value>(raw.trim())
            .map_err(|e| format!("解析邮件列表失败: {e}（原始输出: {}）", raw.trim()))
    }
}

/// 发送邮件工具（高危，需审批）
pub struct SendMailTool;

impl Tool for SendMailTool {
    fn name(&self) -> &str {
        "send_mail"
    }
    fn description(&self) -> &str {
        "通过本地 Outlook 发送邮件。to 收件人（支持逗号分隔多人）、subject 主题、body 正文（纯文本，自动转 HTML）。\
         可选 cc 抄送 / bcc 密送。发送是真实对外副作用，需人工审批。需本机安装并登录 Outlook。"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "to": { "type": "string", "description": "收件人邮箱，多人用逗号分隔" },
                "subject": { "type": "string", "description": "邮件主题" },
                "body": { "type": "string", "description": "邮件正文（纯文本）" },
                "cc": { "type": "string", "description": "抄送（可选）" },
                "bcc": { "type": "string", "description": "密送（可选）" }
            },
            "required": ["to", "subject", "body"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::HighRisk
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let to = args["to"].as_str().unwrap_or("").trim().to_string();
        let subject = args["subject"].as_str().unwrap_or("").trim().to_string();
        let body = args["body"].as_str().unwrap_or("").to_string();
        if to.is_empty() {
            return Err("缺少收件人 to".to_string());
        }
        if subject.is_empty() {
            return Err("缺少邮件主题 subject".to_string());
        }

        let params = json!({
            "to": to,
            "subject": subject,
            "body": body,
            "cc": args["cc"].as_str().unwrap_or(""),
            "bcc": args["bcc"].as_str().unwrap_or(""),
        });

        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let params_path = std::env::temp_dir().join(format!("baize_sendmail_params_{ts}.json"));
        std::fs::write(&params_path, params.to_string()).map_err(|e| format!("写入参数文件失败: {e}"))?;
        let params_str = params_path.to_string_lossy().to_string();

        let result = run_powershell(SEND_MAIL_SCRIPT, &[params_str]);
        let _ = std::fs::remove_file(&params_path);
        result.and_then(|raw| {
            serde_json::from_str::<Value>(raw.trim())
                .map_err(|e| format!("解析发送结果失败: {e}（原始输出: {}）", raw.trim()))
        })
    }
}