//! Office 文档生成与格式转换。
//!
//!   ① `document_write`：markdown → .docx（封面/目录/字体分级/表格设计/代码块/引用块/页码）
//!   ② `pptx_write`：大纲或 markdown → .pptx（16:9 主题、封面、强调条、分区页、备注、页码）
//!   ③ `document_convert`：docx→pdf（Word/WPS COM）、md→docx、docx→md、pdf 合并/拆分、xlsx↔csv
//!
//! Python 侧依赖：python-docx / python-pptx / pypdf / openpyxl
//! （与 read_document 共用同一套依赖，check_document_deps 可检测）。

use std::io::Write;

use serde_json::{json, Value};

use crate::progress::{self, Progress};
use crate::tools::{resolve_path, PermissionClass, Tool};

/// 内嵌的 Python 生成脚本（编译期打入二进制，运行时落盘临时目录执行）
const OFFICE_PY: &str = include_str!("../resources/office_write.py");

/// 单次生成的子进程超时（秒）：COM 导出 PDF 可能较慢
const PY_TIMEOUT_SECS: u64 = 240;

fn now_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// 运行 Python sidecar，返回 stdout 解析出的 JSON。
/// `progress` 非空时：脚本 stderr 的进度协议行实时转发到执行流；长时间无信号时按「已耗时」发心跳。
fn run_python(request: &Value, progress: Option<&Progress>) -> Result<Value, String> {
    let script_dir = std::env::temp_dir().join(format!("baize_ow_script_{}", now_nanos()));
    std::fs::create_dir_all(&script_dir).map_err(|e| format!("创建脚本目录失败: {e}"))?;
    let script = script_dir.join("office_write.py");
    std::fs::write(&script, OFFICE_PY).map_err(|e| format!("写入生成脚本失败: {e}"))?;

    use std::process::Stdio;
    let mut child = crate::tools::python_program()
        .arg(&script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            format!(
                "启动 Python 失败（安装包内置 pyoffice 缺失且系统未装 Python；可尝试 pip install python-docx python-pptx pypdf openpyxl）: {e}"
            )
        })?;

    {
        let stdin = child.stdin.as_mut().ok_or("无法打开 Python stdin")?;
        stdin
            .write_all(request.to_string().as_bytes())
            .map_err(|e| format!("写入请求失败: {e}"))?;
    }
    drop(child.stdin.take());

    let out_h = child.stdout.take().map(crate::progress::drain_stdout);
    let err_h = child
        .stderr
        .take()
        .map(|s| crate::progress::drain_stderr(s, progress.cloned()));

    let started = std::time::Instant::now();
    let deadline = started + std::time::Duration::from_secs(PY_TIMEOUT_SECS);
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => {}
            Err(e) => return Err(format!("等待 Python 退出失败: {e}")),
        }
        if let Some(p) = progress {
            let elapsed = started.elapsed().as_millis();
            if p.idle_ms() > 1500 {
                let hb = progress::heartbeat_pct(elapsed, p.pct().max(12.0), 90.0);
                p.set(hb, &format!("生成中… 已用时 {}s", elapsed / 1000));
            }
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            let msg = format!("文档生成超时（{PY_TIMEOUT_SECS}s），已终止");
            if let Some(p) = progress {
                p.fail(&msg);
            }
            return Err(msg);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };

    let stdout = out_h.and_then(|h| h.join().ok()).unwrap_or_default();
    let stderr = err_h.and_then(|h| h.join().ok()).unwrap_or_default();

    if !status.success() {
        // Python 脚本异常时也会往 stdout 写 JSON 错误，优先取它
        if let Ok(resp) = serde_json::from_str::<Value>(&stdout) {
            if let Some(err) = resp.get("error").and_then(|v| v.as_str()) {
                return Err(err.to_string());
            }
        }
        return Err(format!(
            "Python 生成异常退出（{}）：{stderr}",
            status.code().unwrap_or(-1)
        ));
    }

    let resp: Value = serde_json::from_str(&stdout)
        .map_err(|e| format!("解析 Python 输出失败: {e}；stderr={stderr}"))?;
    if resp.get("ok").and_then(|v| v.as_bool()) == Some(true) {
        Ok(resp)
    } else {
        Err(resp
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("未知生成错误")
            .to_string())
    }
}

/// 末级文件名（进度条标题用）
fn file_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| path.to_string())
}

/// 带进度条执行一次 Python 生成/转换：统一「建句柄 → 执行 → 完成/失败」样板
fn run_with_progress(
    label: String,
    icon: &str,
    finished: &str,
    req: &Value,
) -> Result<Value, String> {
    let pr = Progress::new(label).with_icon(icon);
    pr.set(4.0, "正在准备生成器…");
    match run_python(req, Some(&pr)) {
        Ok(v) => {
            pr.done(finished);
            Ok(v)
        }
        Err(e) => {
            pr.fail(&e);
            Err(e)
        }
    }
}

/// docx → pdf：PowerShell COM 走本机 Word，失败回退 WPS（KWps.Application）。
/// COM 引擎不给内部回调，用「心跳计时器」按已耗时缓慢推进（文案如实标注用时）。
fn docx_to_pdf(src: &str, out: &str) -> Result<Value, String> {
    let pr = Progress::new(format!("转换 PDF · {}", file_name(src))).with_icon("🔀");
    pr.set(5.0, "正在启动 Word/WPS 导出引擎…");
    let result = {
        let _tick = pr.spawn_ticker(85.0, "正在导出 PDF");
        docx_to_pdf_com(src, out)
    };
    match &result {
        Ok(_) => pr.done(&format!("已导出 PDF · {}", file_name(out))),
        Err(e) => pr.fail(e),
    }
    result
}

fn docx_to_pdf_com(src: &str, out: &str) -> Result<Value, String> {
    let ps = format!(
        r#"
$ErrorActionPreference = 'Stop'
$out = $null
function Try-Word {{
  try {{
    $w = New-Object -ComObject Word.Application
    $w.Visible = $false
    $w.DisplayAlerts = 0
    $d = $w.Documents.Open('{src}', $false, $true)
    $d.SaveAs([ref]'{out}', [ref]17)
    $d.Close($false)
    $w.Quit()
    return 'word'
  }} catch {{
    if ($w) {{ $w.Quit() }}
    return $null
  }}
}}
function Try-Wps {{
  try {{
    $w = New-Object -ComObject KWps.Application
    $w.Visible = $false
    $d = $w.Documents.Open('{src}')
    $d.ExportAsFixedFormat('{out}', 17)
    $d.Close()
    $w.Quit()
    return 'wps'
  }} catch {{
    if ($w) {{ $w.Quit() }}
    return $null
  }}
}}
$engine = Try-Word
if (-not $engine) {{ $engine = Try-Wps }}
if ($engine) {{
  if (Test-Path '{out}') {{ Write-Output ('{{"ok":true,"engine":"' + $engine + '"}}') }}
  else {{ Write-Output '{{"ok":false,"error":"导出完成但未找到输出文件"}}' }}
}} else {{
  Write-Output '{{"ok":false,"error":"未检测到可用的 Word 或 WPS（COM 启动失败），无法导出 PDF"}}'
}}
"#
    );
    let output = crate::tools::silent_command("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &ps])
        .output()
        .map_err(|e| format!("启动 PowerShell 失败: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let resp: Value = serde_json::from_str(stdout.trim())
        .map_err(|_| format!("COM 导出输出无法解析：{stdout}"))?;
    if resp.get("ok").and_then(|v| v.as_bool()) == Some(true) {
        Ok(resp)
    } else {
        Err(resp
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("COM 导出失败")
            .to_string())
    }
}

// ───────────────────────── document_write ─────────────────────────

/// markdown → 设计排版 .docx
pub struct DocumentWriteTool;

impl Tool for DocumentWriteTool {
    fn name(&self) -> &str {
        "document_write"
    }
    fn description(&self) -> &str {
        "把 Markdown 内容生成排版精美的 Word 文档（.docx）。内置专业排版设计：封面页（标题/副标题/作者/日期/装饰线）、可自动生成目录域、中西文字体分级（标题微软雅黑深蓝色阶+底线装饰、正文宋体小四 1.5 倍行距+首行缩进）、表格深蓝表头白字+斑马纹+彩色边框、代码块灰底+左侧强调线、引用块、页脚页码。content 传 Markdown（支持 #标题、**粗**、*斜*、`码`、[链接](url)、表格、```代码块、>引用、![图片](本地路径)、列表）。title 传了会自动生成封面。theme: classic(默认，宋体正文) 或 modern(全雅黑)"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "content": { "type": "string", "description": "Markdown 正文内容" },
                "path": { "type": "string", "description": "输出 .docx 文件路径" },
                "title": { "type": "string", "description": "文档主标题（提供则生成封面页）" },
                "subtitle": { "type": "string", "description": "副标题（封面用）" },
                "author": { "type": "string", "description": "作者署名（封面用）" },
                "toc": { "type": "boolean", "description": "是否生成目录页（默认 false）" },
                "theme": { "type": "string", "enum": ["classic", "modern"], "description": "主题：classic=宋体正文（默认），modern=全微软雅黑" },
                "indent_body": { "type": "boolean", "description": "正文首行缩进两字符（默认 true）" }
            },
            "required": ["content", "path"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let content = args["content"]
            .as_str()
            .ok_or("缺少参数 content（Markdown 内容）")?
            .to_string();
        let path = resolve_path(args["path"].as_str().ok_or("缺少参数 path（.docx 输出路径）")?);
        if !path.to_lowercase().ends_with(".docx") {
            return Err("输出路径必须以 .docx 结尾".into());
        }
        let req = json!({
            "op": "docx",
            "content": content,
            "path": path,
            "title": args.get("title").and_then(|v| v.as_str()),
            "subtitle": args.get("subtitle").and_then(|v| v.as_str()),
            "author": args.get("author").and_then(|v| v.as_str()),
            "toc": args.get("toc").and_then(|v| v.as_bool()).unwrap_or(false),
            "theme": args.get("theme").and_then(|v| v.as_str()).unwrap_or("classic"),
            "indent_body": args.get("indent_body").and_then(|v| v.as_bool()).unwrap_or(true),
        });
        run_with_progress(
            format!("生成 Word 文档 · {}", file_name(&path)),
            "📄",
            "Word 文档已生成",
            &req,
        )
    }
}

// ───────────────────────── pptx_write ─────────────────────────

/// 大纲/JSON → 主题设计 .pptx
pub struct PptxWriteTool;

impl Tool for PptxWriteTool {
    fn name(&self) -> &str {
        "pptx_write"
    }
    fn description(&self) -> &str {
        "生成排版精美的 PowerPoint 演示文稿（.pptx）。内置 16:9 主题设计：深蓝封面页（大标题+强调色条+装饰块）、内容页标题下强调色条、分区过渡页、页码与页脚、演讲者备注。两种输入任选：① slides 数组 [{title, bullets:[\"要点\" 或 {\"text\":\"要点\",\"level\":0/1}, ...], notes:\"备注\"}]（bullets 为空则生成分区页）；② content 传 Markdown（# 行=每页标题，## 行=强调要点，- 行=普通要点，缩进两级=子要点）。title/subtitle/author 用于封面"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "输出 .pptx 文件路径" },
                "title": { "type": "string", "description": "演示文稿主标题（封面页）" },
                "subtitle": { "type": "string", "description": "副标题（封面页）" },
                "author": { "type": "string", "description": "作者署名（封面页）" },
                "slides": { "type": "array", "description": "页数组：[{title, bullets, notes}]，bullets 为空数组时生成分区过渡页",
                    "items": { "type": "object", "properties": {
                        "title": { "type": "string" },
                        "bullets": { "type": "array", "items": { "type": ["string", "object"] } },
                        "notes": { "type": ["string", "array"] }
                    } } },
                "content": { "type": "string", "description": "Markdown 大纲（与 slides 二选一）" }
            },
            "required": ["path"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let path = resolve_path(args["path"].as_str().ok_or("缺少参数 path（.pptx 输出路径）")?);
        if !path.to_lowercase().ends_with(".pptx") {
            return Err("输出路径必须以 .pptx 结尾".into());
        }
        let req = json!({
            "op": "pptx",
            "path": path,
            "title": args.get("title").and_then(|v| v.as_str()),
            "subtitle": args.get("subtitle").and_then(|v| v.as_str()),
            "author": args.get("author").and_then(|v| v.as_str()),
            "slides": args.get("slides").cloned().unwrap_or(json!([])),
            "content": args.get("content").and_then(|v| v.as_str()),
        });
        run_with_progress(
            format!("生成演示文稿 · {}", file_name(&path)),
            "🎞",
            "演示文稿已生成",
            &req,
        )
    }
}

// ───────────────────────── document_convert ─────────────────────────

/// 文档格式转换枢纽
pub struct DocumentConvertTool;

impl Tool for DocumentConvertTool {
    fn name(&self) -> &str {
        "document_convert"
    }
    fn description(&self) -> &str {
        "文档格式转换：op=docx_to_pdf（Word/WPS COM 导出 PDF）、md_to_docx（Markdown 转 Word，同 document_write 排版设计）、docx_to_md（Word 转 Markdown，保留标题层级与表格）、pdf_merge（合并多个 PDF，srcs 传数组）、pdf_split（PDF 按页拆分为多个单页文件）、xlsx_to_csv / csv_to_xlsx（Excel 与 CSV 互转，表头加粗深蓝底）。out 可省略：默认输出到源文件同目录同名"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "op": { "type": "string", "enum": ["docx_to_pdf", "md_to_docx", "docx_to_md", "pdf_merge", "pdf_split", "xlsx_to_csv", "csv_to_xlsx"], "description": "转换操作" },
                "src": { "type": "string", "description": "源文件路径（pdf_merge 用 srcs 数组）" },
                "srcs": { "type": "array", "items": { "type": "string" }, "description": "pdf_merge：要合并的 PDF 路径列表（按顺序）" },
                "out": { "type": "string", "description": "输出路径（可省略，默认同目录同名）" },
                "out_dir": { "type": "string", "description": "pdf_split：输出目录（可省略）" },
                "sheet": { "type": "string", "description": "xlsx_to_csv：工作表名（可省略，默认活动表）" },
                "title": { "type": "string", "description": "md_to_docx：封面主标题（可选）" },
                "subtitle": { "type": "string", "description": "md_to_docx：封面副标题（可选）" },
                "author": { "type": "string", "description": "md_to_docx：作者（可选）" },
                "toc": { "type": "boolean", "description": "md_to_docx：是否生成目录页（可选）" }
            },
            "required": ["op"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let op = args["op"].as_str().ok_or("缺少参数 op")?.to_string();
        match op.as_str() {
            "docx_to_pdf" => {
                let src = resolve_path(args["src"].as_str().ok_or("缺少参数 src")?);
                let out = match args.get("out").and_then(|v| v.as_str()) {
                    Some(o) => resolve_path(o),
                    None => pdf_out_default(&src),
                };
                docx_to_pdf(&src, &out)
            }
            "md_to_docx" => {
                let src = resolve_path(args["src"].as_str().ok_or("缺少参数 src")?);
                let content = std::fs::read_to_string(&src)
                    .map_err(|e| format!("读取 Markdown 失败: {e}"))?;
                let out = match args.get("out").and_then(|v| v.as_str()) {
                    Some(o) => resolve_path(o),
                    None => docx_out_default(&src),
                };
                let req = json!({
                    "op": "docx",
                    "content": content,
                    "path": out,
                    "title": args.get("title").and_then(|v| v.as_str()),
                    "subtitle": args.get("subtitle").and_then(|v| v.as_str()),
                    "author": args.get("author").and_then(|v| v.as_str()),
                    "toc": args.get("toc").and_then(|v| v.as_bool()).unwrap_or(false),
                });
                run_with_progress(
                    format!("转换 Word · {}", file_name(&src)),
                    "🔀",
                    "已转换为 Word 文档",
                    &req,
                )
            }
            "docx_to_md" | "pdf_split" | "xlsx_to_csv" | "csv_to_xlsx" => {
                let src = resolve_path(args["src"].as_str().ok_or("缺少参数 src")?);
                let req = json!({
                    "op": op,
                    "src": src,
                    "out": args.get("out").map(|v| resolve_path(v.as_str().unwrap_or(""))),
                    "out_dir": args.get("out_dir").map(|v| resolve_path(v.as_str().unwrap_or(""))),
                    "sheet": args.get("sheet").and_then(|v| v.as_str()),
                });
                let (label, icon, done) = match op.as_str() {
                    "docx_to_md" => (format!("转换 Markdown · {}", file_name(&src)), "📄", "已转换为 Markdown"),
                    "pdf_split" => (format!("拆分 PDF · {}", file_name(&src)), "✂", "PDF 已按页拆分"),
                    "xlsx_to_csv" => (format!("导出 CSV · {}", file_name(&src)), "📊", "已导出 CSV"),
                    _ => (format!("生成 Excel · {}", file_name(&src)), "📊", "已生成 Excel"),
                };
                run_with_progress(label, icon, done, &req)
            }
            "pdf_merge" => {
                let srcs: Vec<String> = args["srcs"]
                    .as_array()
                    .ok_or("pdf_merge 需要 srcs 数组（按顺序的 PDF 路径）")?
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| resolve_path(s))
                    .collect();
                if srcs.len() < 2 {
                    return Err("pdf_merge 至少需要 2 个源文件".into());
                }
                let out = resolve_path(
                    args.get("out")
                        .and_then(|v| v.as_str())
                        .ok_or("pdf_merge 需要输出路径 out")?,
                );
                let req = json!({ "op": "pdf_merge", "srcs": srcs, "out": out });
                run_with_progress(
                    format!("合并 PDF · {} 个文件", req["srcs"].as_array().map(|a| a.len()).unwrap_or(0)),
                    "📎",
                    "PDF 已合并",
                    &req,
                )
            }
            other => Err(format!("不支持的转换操作: {other}")),
        }
    }
}

/// docx → pdf 默认输出路径（同目录同名 .pdf）
fn pdf_out_default(src: &str) -> String {
    match src.rfind('.') {
        Some(i) => format!("{}.pdf", &src[..i]),
        None => format!("{src}.pdf"),
    }
}

/// md → docx 默认输出路径（同目录同名 .docx）
fn docx_out_default(src: &str) -> String {
    match src.rfind('.') {
        Some(i) => format!("{}.docx", &src[..i]),
        None => format!("{src}.docx"),
    }
}
