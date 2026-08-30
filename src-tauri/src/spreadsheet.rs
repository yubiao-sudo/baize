//! 表格结构化读写：CSV / Excel(xlsx)
//!
//! 读取用 calamine（xlsx），写入用 rust_xlsxwriter；CSV 用 csv crate。
//! 统一返回 { columns, rows, count } 结构，方便 Agent 直接消费。

use calamine::Reader;
use serde_json::{json, Value};

use crate::tools::{resolve_path, PermissionClass, Tool};

/// 把 JSON 单元格值统一转为字符串（数字/布尔/null 也转字符串）
fn cell_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        _ => v.to_string(),
    }
}

/// 读取 args["rows"]（二维数组），返回 Vec<Vec<String>>
fn parse_rows(args: &Value) -> Result<Vec<Vec<String>>, String> {
    let arr = args
        .get("rows")
        .and_then(|v| v.as_array())
        .ok_or("缺少参数 rows（二维字符串数组）")?;
    let mut rows = Vec::with_capacity(arr.len());
    for (i, row) in arr.iter().enumerate() {
        let cells = row
            .as_array()
            .ok_or_else(|| format!("rows 第 {i} 行不是数组"))?;
        rows.push(cells.iter().map(cell_to_string).collect());
    }
    Ok(rows)
}

/// 读取 args["headers"]（一维数组，可选）
fn parse_headers(args: &Value) -> Option<Vec<String>> {
    args.get("headers")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().map(cell_to_string).collect())
}

/// 确保文件父目录存在
fn ensure_parent(path: &str) -> Result<(), String> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建父目录失败: {e}"))?;
        }
    }
    Ok(())
}

// ───────────────── CSV ─────────────────

pub struct CsvReadTool;

impl Tool for CsvReadTool {
    fn name(&self) -> &str {
        "csv_read"
    }
    fn description(&self) -> &str {
        "读取 CSV 文件为结构化数据，返回列名与行数据。has_headers=true 时首行作为列名（默认 true）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "CSV 文件路径（绝对路径或相对工作空间）" },
                "has_headers": { "type": "boolean", "description": "首行是否为列名，默认 true" }
            },
            "required": ["path"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let path = resolve_path(args["path"].as_str().ok_or("缺少参数 path")?);
        let has_headers = args["has_headers"].as_bool().unwrap_or(true);

        let mut rdr = csv::ReaderBuilder::new()
            .has_headers(has_headers)
            .flexible(true)
            .from_path(&path)
            .map_err(|e| format!("打开 CSV 失败: {e}"))?;

        let columns: Vec<String> = if has_headers {
            rdr.headers()
                .map_err(|e| format!("读取 CSV 表头失败: {e}"))?
                .iter()
                .map(|s| s.to_string())
                .collect()
        } else {
            Vec::new()
        };

        let mut rows = Vec::new();
        for rec in rdr.records() {
            let rec = rec.map_err(|e| format!("读取 CSV 行失败: {e}"))?;
            rows.push(rec.iter().map(|s| s.to_string()).collect::<Vec<String>>());
        }
        let count = rows.len();
        Ok(json!({ "path": path, "columns": columns, "rows": rows, "count": count }))
    }
}

pub struct CsvWriteTool;

impl Tool for CsvWriteTool {
    fn name(&self) -> &str {
        "csv_write"
    }
    fn description(&self) -> &str {
        "把结构化数据（headers + rows）写入 CSV 文件（写操作，需授权）。headers 可选一维数组作为列名，rows 为二维字符串数组"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "目标 CSV 文件路径" },
                "headers": { "type": "array", "description": "列名（可选）" },
                "rows": { "type": "array", "description": "行数据（二维数组）" }
            },
            "required": ["path", "rows"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let path = resolve_path(args["path"].as_str().ok_or("缺少参数 path")?);
        let headers = parse_headers(&args);
        let rows = parse_rows(&args)?;

        ensure_parent(&path)?;

        let mut wtr = csv::WriterBuilder::new()
            .from_path(&path)
            .map_err(|e| format!("创建 CSV 失败: {e}"))?;
        if let Some(h) = &headers {
            wtr.write_record(h)
                .map_err(|e| format!("写入 CSV 表头失败: {e}"))?;
        }
        for row in &rows {
            wtr.write_record(row)
                .map_err(|e| format!("写入 CSV 行失败: {e}"))?;
        }
        wtr.flush().map_err(|e| format!("刷新 CSV 失败: {e}"))?;

        Ok(json!({
            "ok": true,
            "path": path,
            "columns": headers.unwrap_or_default().len(),
            "rows_written": rows.len(),
        }))
    }
}

// ───────────────── Excel (xlsx) ─────────────────

/// calamine 单元格值 → 字符串
fn data_to_string(d: &calamine::Data) -> String {
    use calamine::Data;
    match d {
        Data::Int(v) => v.to_string(),
        Data::Float(v) => v.to_string(),
        Data::Bool(v) => v.to_string(),
        Data::String(v) => v.clone(),
        Data::DateTime(v) => match v.as_datetime() {
            Some(dt) => dt.to_string(),
            None => format!("{v:?}"),
        },
        Data::DateTimeIso(v) => v.clone(),
        Data::DurationIso(v) => v.clone(),
        Data::Error(v) => format!("{v:?}"),
        Data::Empty => String::new(),
    }
}

pub struct XlsxReadTool;

impl Tool for XlsxReadTool {
    fn name(&self) -> &str {
        "xlsx_read"
    }
    fn description(&self) -> &str {
        "读取 Excel(.xlsx) 文件为结构化数据，返回列名与行数据。has_headers=true 时首行作为列名（默认 true），可选指定 sheet 名"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": ".xlsx 文件路径" },
                "sheet": { "type": "string", "description": "工作表名，缺省取第一个" },
                "has_headers": { "type": "boolean", "description": "首行是否为列名，默认 true" }
            },
            "required": ["path"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let path = resolve_path(args["path"].as_str().ok_or("缺少参数 path")?);
        let sheet = args["sheet"].as_str().map(|s| s.to_string());
        let has_headers = args["has_headers"].as_bool().unwrap_or(true);

        let mut workbook: calamine::Xlsx<_> =
            calamine::open_workbook(&path).map_err(|e| format!("打开 Excel 失败: {e}"))?;

        let sheet_name = match &sheet {
            Some(s) => s.clone(),
            None => workbook
                .sheet_names()
                .first()
                .cloned()
                .ok_or("工作簿没有可用工作表")?,
        };

        let range = workbook
            .worksheet_range(&sheet_name)
            .map_err(|e| format!("读取工作表失败: {e}"))?;

        let mut all: Vec<Vec<String>> = Vec::new();
        for row in range.rows() {
            all.push(row.iter().map(data_to_string).collect());
        }

        let (columns, rows) = if has_headers && !all.is_empty() {
            let mut iter = all.into_iter();
            let cols = iter.next().unwrap_or_default();
            (cols, iter.collect())
        } else {
            (Vec::new(), all)
        };
        let count = rows.len();

        Ok(json!({
            "path": path,
            "sheet": sheet_name,
            "columns": columns,
            "rows": rows,
            "count": count,
        }))
    }
}

pub struct XlsxWriteTool;

impl Tool for XlsxWriteTool {
    fn name(&self) -> &str {
        "xlsx_write"
    }
    fn description(&self) -> &str {
        "把结构化数据（headers + rows）写入 Excel(.xlsx) 文件（写操作，需授权）。headers 可选作为列名写入首行，rows 为二维字符串数组"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "目标 .xlsx 文件路径" },
                "sheet": { "type": "string", "description": "工作表名，默认 Sheet1" },
                "headers": { "type": "array", "description": "列名（可选）" },
                "rows": { "type": "array", "description": "行数据（二维数组）" }
            },
            "required": ["path", "rows"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let path = resolve_path(args["path"].as_str().ok_or("缺少参数 path")?);
        let sheet_name = args["sheet"].as_str().unwrap_or("Sheet1").to_string();
        let headers = parse_headers(&args);
        let rows = parse_rows(&args)?;

        ensure_parent(&path)?;

        let mut workbook = rust_xlsxwriter::Workbook::new();
        {
            let worksheet = workbook.add_worksheet();
            if sheet_name != "Sheet1" {
                worksheet
                    .set_name(sheet_name.as_str())
                    .map_err(|e| format!("设置工作表名失败: {e}"))?;
            }
            let mut r: u32 = 0;
            if let Some(h) = &headers {
                for (c, v) in h.iter().enumerate() {
                    worksheet
                        .write_string(r, c as u16, v.as_str())
                        .map_err(|e| format!("写入 Excel 表头失败: {e}"))?;
                }
                r += 1;
            }
            for row in &rows {
                for (c, v) in row.iter().enumerate() {
                    worksheet
                        .write_string(r, c as u16, v.as_str())
                        .map_err(|e| format!("写入 Excel 单元格失败: {e}"))?;
                }
                r += 1;
            }
        }
        workbook
            .save(&path)
            .map_err(|e| format!("保存 Excel 失败: {e}"))?;

        Ok(json!({
            "ok": true,
            "path": path,
            "sheet": sheet_name,
            "columns": headers.unwrap_or_default().len(),
            "rows_written": rows.len(),
        }))
    }
}