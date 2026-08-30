//! 办公文档解析（PDF / Word / Excel / PPT / CSV / TXT / MD）。
//!
//! 通过内嵌的 Python sidecar（read_document.py）实现富解析：
//!   ① 正文文本抽取；② 结构化表格抽取（可导出 CSV）；③ 内嵌图片抽取；④ 目录批量。
//! Python/依赖不可用时自动回退到内置纯文本抽取（`document::extract_text`）与
//! `spreadsheet::XlsxReadTool`，保证基础可读性不丢失。

use std::io::Write;

use serde_json::{json, Value};

use crate::tools::{resolve_path, PermissionClass, Tool};

/// 内嵌的 Python 解析脚本（编译期打入二进制，运行时落盘到临时目录执行）
const PARSER_PY: &str = include_str!("../resources/read_document.py");

/// 单次解析的子进程超时（秒）
const PY_TIMEOUT_SECS: u64 = 180;

/// 受支持的扩展名
fn is_supported(path: &str) -> bool {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    matches!(
        ext.as_str(),
        "pdf" | "docx" | "xlsx" | "pptx" | "csv" | "txt" | "md" | "xls"
    )
}

fn extension(path: &str) -> String {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase()
}

fn now_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// 收集目标文件：单个文件原样返回；目录则（可选递归）收集受支持文件
fn collect_files(path: &str, recursive: bool) -> Result<Vec<String>, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("无法访问路径 {path}: {e}"))?;
    let mut out = Vec::new();
    if meta.is_dir() {
        let mut stack = vec![path.to_string()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir)
                .map_err(|e| format!("读取目录失败 {dir}: {e}"))?
                .flatten()
            {
                let p = entry.path();
                if p.is_dir() {
                    if recursive {
                        stack.push(p.to_string_lossy().to_string());
                    }
                } else if is_supported(&p.to_string_lossy()) {
                    out.push(p.to_string_lossy().to_string());
                }
            }
        }
        out.sort();
    } else if is_supported(path) {
        out.push(path.to_string());
    } else {
        return Err(format!("不支持的文档格式: {path}"));
    }
    Ok(out)
}

/// 运行 Python sidecar，返回其 stdout 解析出的 JSON
fn run_python(request: &Value) -> Result<Value, String> {
    let script_dir = std::env::temp_dir().join(format!("baize_rd_script_{}", now_nanos()));
    std::fs::create_dir_all(&script_dir).map_err(|e| format!("创建脚本目录失败: {e}"))?;
    let script = script_dir.join("read_document.py");
    std::fs::write(&script, PARSER_PY).map_err(|e| format!("写入解析脚本失败: {e}"))?;

    use std::process::{Command, Stdio};
    let mut child = Command::new("python")
        .arg(&script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            format!(
                "启动 Python 失败（请确认已安装 Python 并执行 pip install pdfplumber python-docx openpyxl python-pptx）: {e}"
            )
        })?;

    {
        let stdin = child.stdin.as_mut().ok_or("无法打开 Python stdin")?;
        stdin
            .write_all(request.to_string().as_bytes())
            .map_err(|e| format!("写入请求失败: {e}"))?;
    }
    // 关闭 stdin 让脚本读到 EOF
    drop(child.stdin.take());

    let out_h = child.stdout.take().map(|mut s| {
        std::thread::spawn(move || {
            use std::io::Read;
            let mut b = String::new();
            let _ = s.read_to_string(&mut b);
            b
        })
    });
    let err_h = child.stderr.take().map(|mut s| {
        std::thread::spawn(move || {
            use std::io::Read;
            let mut b = String::new();
            let _ = s.read_to_string(&mut b);
            b
        })
    });

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(PY_TIMEOUT_SECS);
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => {}
            Err(e) => return Err(format!("等待 Python 退出失败: {e}")),
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("文档解析超时（{PY_TIMEOUT_SECS}s），已终止"));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };

    let stdout = out_h.and_then(|h| h.join().ok()).unwrap_or_default();
    let stderr = err_h.and_then(|h| h.join().ok()).unwrap_or_default();

    if !status.success() {
        return Err(format!("Python 解析异常退出（{}）：{stderr}", status.code().unwrap_or(-1)));
    }

    let resp: Value = serde_json::from_str(&stdout)
        .map_err(|e| format!("解析 Python 输出失败: {e}；stderr={stderr}"))?;
    if resp.get("ok").and_then(|v| v.as_bool()) == Some(true) {
        Ok(resp)
    } else {
        Err(resp
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("未知解析错误")
            .to_string())
    }
}

/// Python 不可用时的原生回退：只抽取纯文本（xlsx 走 calamine 结构化读取）
fn fallback_native(files: &[String], py_err: String) -> Result<Value, String> {
    let warnings = vec![format!("Python 解析不可用，已回退到内置文本抽取：{py_err}")];
    let mut out = Vec::new();
    for f in files {
        let ext = extension(f);
        let mut text = String::new();
        let mut tables: Value = json!([]);
        let mut stats = json!({});
        if ext == "xlsx" {
            match crate::spreadsheet::XlsxReadTool.run(json!({ "path": f })) {
                Ok(v) => {
                    let cols = v["columns"].clone();
                    let rows = v["rows"].clone();
                    if let Some(rs) = rows.as_array() {
                        for r in rs {
                            if let Some(arr) = r.as_array() {
                                let line = arr
                                    .iter()
                                    .flat_map(|c| c.as_str())
                                    .collect::<Vec<_>>()
                                    .join(" | ");
                                text.push_str(&line);
                                text.push('\n');
                            }
                        }
                    }
                    stats = json!({ "sheet": v["sheet"].clone() });
                    tables = json!([{ "columns": cols, "rows": rows }]);
                }
                Err(e) => text = format!("[读取失败] {e}"),
            }
        } else {
            match crate::document::extract_text(f) {
                Ok(t) => text = t,
                Err(e) => text = format!("[读取失败] {e}"),
            }
        }
        let tables_count = tables.as_array().map(|a| a.len()).unwrap_or(0);
        out.push(json!({
            "path": f,
            "format": ext,
            "text": text,
            "chars": text.chars().count(),
            "truncated": false,
            "stats": stats,
            "tables": tables,
            "tables_count": tables_count,
            "images": [],
            "images_count": 0,
            "csv_files": [],
        }));
    }
    Ok(json!({ "ok": true, "count": out.len(), "files": out, "warnings": warnings }))
}

/// 统一解析入口（供 Tool 与前端命令复用）
pub fn run(args: Value) -> Result<Value, String> {
    let raw_path = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or("缺少参数 path")?;
    let path = resolve_path(raw_path);
    let recursive = args.get("recursive").and_then(|v| v.as_bool()).unwrap_or(true);

    let files = collect_files(&path, recursive)?;
    if files.is_empty() {
        return Err(format!("未找到可解析的文档：{path}"));
    }

    let out_dir = std::env::temp_dir().join(format!("baize_rd_{}", now_nanos()));
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("创建输出目录失败: {e}"))?;

    let request = json!({
        "paths": files,
        "extract_text": args.get("extract_text").and_then(|v| v.as_bool()).unwrap_or(true),
        "extract_tables": args.get("extract_tables").and_then(|v| v.as_bool()).unwrap_or(true),
        "extract_images": args.get("extract_images").and_then(|v| v.as_bool()).unwrap_or(true),
        "export_csv": args.get("export_csv").and_then(|v| v.as_bool()).unwrap_or(false),
        "csv_dir": args.get("csv_dir").and_then(|v| v.as_str()),
        "max_chars": args.get("max_chars").and_then(|v| v.as_u64()).unwrap_or(20_000),
        "out_dir": out_dir.to_string_lossy(),
    });

    match run_python(&request) {
        Ok(resp) => Ok(resp),
        Err(py_err) => fallback_native(&files, py_err),
    }
}

/// 前端/模型共用的一体化文档读取工具
pub struct ReadDocumentTool;

impl Tool for ReadDocumentTool {
    fn name(&self) -> &str {
        "read_document"
    }
    fn description(&self) -> &str {
        "解析办公文档（PDF/Word/Excel/PPT/CSV/TXT/MD），提取正文文本、结构化表格和内嵌图片；可把表格导出为 CSV，支持目录批量。path 填单个文件路径或目录"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "单个文件路径，或目录（目录则批量解析）" },
                "extract_text": { "type": "boolean", "description": "是否提取文本，默认 true" },
                "extract_tables": { "type": "boolean", "description": "是否提取表格，默认 true" },
                "extract_images": { "type": "boolean", "description": "是否提取图片，默认 true" },
                "export_csv": { "type": "boolean", "description": "是否把表格导出为 CSV 文件，默认 false" },
                "csv_dir": { "type": "string", "description": "CSV 导出目录（缺省用源文件同目录）" },
                "max_chars": { "type": "integer", "description": "单文件文本截断上限（字符数），默认 20000" },
                "recursive": { "type": "boolean", "description": "目录批量时是否递归子目录，默认 true" }
            },
            "required": ["path"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        run(args)
    }
}

/// 文档解析所需的 Python 库（模块名 → pip 包名）
const DOC_LIBS: &[(&str, &str)] = &[
    ("pdfplumber", "pdfplumber"),
    ("docx", "python-docx"),
    ("openpyxl", "openpyxl"),
    ("pptx", "python-pptx"),
    ("pypdf", "pypdf"),
];

/// 单次探测脚本：输出一行 JSON {"ver": 版本, "missing": [缺失的 pip 包名]}
const DEPS_PROBE: &str = r#"
import sys, json
libs = [("pdfplumber","pdfplumber"),("docx","python-docx"),("openpyxl","openpyxl"),("pptx","python-pptx"),("pypdf","pypdf")]
missing = []
for mod, pkg in libs:
    try:
        __import__(mod)
    except Exception:
        missing.append(pkg)
print(json.dumps({"ver": sys.version.split()[0], "missing": missing}))
"#;

/// 检查 Python 及文档解析库是否就绪，返回结构化报告（供 env_check / 安装引导展示）。
/// 用单次子进程探测，避免逐库反复启动 Python。
pub fn deps_report() -> Value {
    use std::process::Command;
    let all_missing = || -> Vec<String> { DOC_LIBS.iter().map(|(_, pkg)| pkg.to_string()).collect() };
    let mut python: Option<String> = None;
    let mut missing: Vec<String> = all_missing();

    if let Ok(out) = Command::new("python").args(["-c", DEPS_PROBE]).output() {
        if out.status.success() {
            if let Ok(parsed) = serde_json::from_slice::<Value>(&out.stdout) {
                python = parsed.get("ver").and_then(|v| v.as_str()).map(String::from);
                if let Some(arr) = parsed.get("missing").and_then(|v| v.as_array()) {
                    missing = arr.iter().flat_map(|v| v.as_str()).map(String::from).collect();
                }
            }
        } else if let Ok(o) = Command::new("python").arg("--version").output() {
            // Python 存在但探针失败（如版本过旧），仅回报版本号
            if o.status.success() {
                python = Some(String::from_utf8_lossy(&o.stdout).trim().to_string());
            }
        }
    }

    json!({
        "python": python,
        "ready": python.is_some() && missing.is_empty(),
        "missing": missing,
        "install_command": "pip install pdfplumber python-docx openpyxl python-pptx pypdf",
    })
}

/// 检查文档解析依赖（Python + 各解析库）的安装引导工具
pub struct DocumentDepsTool;

impl Tool for DocumentDepsTool {
    fn name(&self) -> &str {
        "check_document_deps"
    }
    fn description(&self) -> &str {
        "检查办公文档解析（read_document）所需的 Python 运行时与解析库是否就绪；缺失时给出安装命令。解析 PDF/Word/Excel/PPT 前后可先调用它了解依赖状态"
    }
    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, _args: Value) -> Result<Value, String> {
        Ok(deps_report())
    }
}