//! 跨应用数据管道 + 可视化（Data Pipeline & Visualization）
//!
//! 对应《白泽自主进化》功能三：数据散在网页/Excel/文本里，白泽一键完成
//! 「采集 → 清洗 → 存储 → 可视化 → 报告」，全程无需手工搬运。
//!
//! 管线：data_ingest（读取 CSV/JSON/文件 → 结构化表）
//!   → data_clean（去重/补缺/类型转换/裁剪）
//!   → data_aggregate（分组聚合：sum/avg/count/min/max）
//!   → data_viz（生成 ECharts 交互 HTML 并在内置浏览器渲染）
//!   → data_report（生成 Markdown 图文周报到文档窗口）
//!   → data_export（把结果表写回 CSV / JSON 文件）

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::browser::BrowserState;
use crate::markdown::MarkdownState;
use crate::tools::{PermissionClass, Tool, resolve_path};

/// 结构化表格：列名 + 行（均为字符串）
#[derive(Debug, Clone, Default)]
struct Table {
    columns: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Table {
    fn to_json(&self) -> Value {
        json!({
            "columns": self.columns,
            "rows": self.rows,
            "count": self.rows.len(),
        })
    }

    fn column_index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c == name)
    }
}

// ───────────────── 解析 ─────────────────

/// 读取输入：若 input 是存在的本地文件路径则读取文件内容，否则当作原始文本
fn read_input(input: &str) -> Result<String, String> {
    let p = std::path::Path::new(input);
    if p.is_file() {
        std::fs::read_to_string(input).map_err(|e| e.to_string())
    } else {
        Ok(input.to_string())
    }
}

/// 按指定格式解析文本为结构化表
fn parse_text(raw: &str, format: &str) -> Result<Table, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Table::default());
    }
    if format == "json" || (format == "auto" && trimmed.starts_with('[') || trimmed.starts_with('{')) {
        parse_json(trimmed)
    } else {
        parse_csv(trimmed)
    }
}

/// 极简 CSV 解析（支持带引号字段、首行表头）
fn parse_csv(text: &str) -> Result<Table, String> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .from_reader(text.as_bytes());
    let headers = reader
        .headers()
        .map_err(|e| format!("CSV 表头解析失败: {e}"))?
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    let mut rows = Vec::new();
    for rec in reader.records() {
        let rec = rec.map_err(|e| format!("CSV 行解析失败: {e}"))?;
        let mut row: Vec<String> = rec.iter().map(|s| s.to_string()).collect();
        // 补齐列宽
        while row.len() < headers.len() {
            row.push(String::new());
        }
        rows.push(row);
    }
    Ok(Table {
        columns: headers,
        rows,
    })
}

/// JSON 解析：数组对象 / {columns, rows} 两种结构
fn parse_json(text: &str) -> Result<Table, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("JSON 解析失败: {e}"))?;
    // 结构一：{columns: [...], rows: [[..], ..]}
    if let (Some(cols), Some(rows)) = (v.get("columns"), v.get("rows")) {
        let columns = cols
            .as_array()
            .map(|a| a.iter().map(scalar_to_str).collect::<Vec<_>>())
            .unwrap_or_default();
        let mut out_rows = Vec::new();
        if let Some(arr) = rows.as_array() {
            for r in arr {
                if let Some(rr) = r.as_array() {
                    out_rows.push(rr.iter().map(scalar_to_str).collect::<Vec<_>>());
                }
            }
        }
        return Ok(Table {
            columns,
            rows: out_rows,
        });
    }
    // 结构二：对象数组
    let arr = v
        .as_array()
        .ok_or_else(|| "JSON 需为对象数组或 {columns, rows} 结构".to_string())?;
    let mut columns: Vec<String> = Vec::new();
    // 收集列名（保持出现顺序）
    for item in arr {
        if let Some(obj) = item.as_object() {
            for k in obj.keys() {
                if !columns.contains(k) {
                    columns.push(k.clone());
                }
            }
        }
    }
    let mut rows = Vec::new();
    for item in arr {
        let obj = item.as_object().cloned().unwrap_or_default();
        let row = columns
            .iter()
            .map(|c| obj.get(c).map(scalar_to_str).unwrap_or_default())
            .collect::<Vec<_>>();
        rows.push(row);
    }
    Ok(Table { columns, rows })
}

fn scalar_to_str(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

fn parse_num(s: &str) -> Option<f64> {
    let t = s.trim().replace(',', "");
    if t.is_empty() {
        return None;
    }
    t.parse::<f64>().ok()
}

fn json_to_table(v: &Value) -> Result<Table, String> {
    let columns = v
        .get("columns")
        .and_then(|c| c.as_array())
        .map(|a| a.iter().map(scalar_to_str).collect::<Vec<_>>())
        .ok_or("数据缺少 columns 字段")?;
    let rows = v
        .get("rows")
        .and_then(|r| r.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|r| {
                    r.as_array()
                        .map(|rr| rr.iter().map(scalar_to_str).collect::<Vec<_>>())
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(Table { columns, rows })
}

// ───────────────── 清洗 ─────────────────

/// 对表格做基础清洗：去重、裁剪、补齐缺失、类型转换
fn clean_table(table: &Table, args: &Value) -> Table {
    let dedup = args["dedup"].as_bool().unwrap_or(true);
    let trim = args["trim"].as_bool().unwrap_or(true);
    let fill = args["fill"].as_str().unwrap_or("");

    let mut rows: Vec<Vec<String>> = table
        .rows
        .iter()
        .map(|r| {
            r.iter()
                .map(|c| if trim { c.trim().to_string() } else { c.clone() })
                .collect()
        })
        .collect();

    // 补齐缺失
    if !fill.is_empty() || fill == "" {
        for r in &mut rows {
            for c in r.iter_mut() {
                if c.is_empty() {
                    *c = fill.to_string();
                }
            }
        }
    }

    // 去重（整行一致）
    if dedup {
        let mut seen = std::collections::HashSet::new();
        rows.retain(|r| seen.insert(r.join("\u{1f}")));
    }

    // 类型转换（可选：把某列尝试转为数值，便于后续聚合）
    let mut columns = table.columns.clone();
    let numeric_cols: Vec<String> = args
        .get("numeric_columns")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    for name in &numeric_cols {
        if let Some(idx) = columns.iter().position(|c| c == name) {
            columns[idx] = name.clone();
            for r in &mut rows {
                if let Some(n) = parse_num(&r[idx]) {
                    r[idx] = n.to_string();
                }
            }
        }
    }

    Table { columns, rows }
}

// ───────────────── ECharts HTML ─────────────────

fn build_echarts_html(chart: &str, title: &str, categories: &[String], values: &[f64]) -> String {
    let cats_json = json!(categories).to_string();
    let vals_json = json!(values).to_string();

    let series_js = match chart {
        "line" => "series:[{type:'line',data:ser,smooth:true}]".to_string(),
        "pie" => "series:[{type:'pie',radius:'55%',data:ser.map(function(v,i){return {name:cats[i],value:v}})}]".to_string(),
        _ => "series:[{type:'bar',data:ser}]".to_string(),
    };

    // 饼图无直角坐标轴；柱状/折线用类目轴 + 数值轴
    let coord_js = if chart == "pie" {
        "legend:{orient:'vertical',left:'left'}".to_string()
    } else {
        "grid:{left:48,right:24,top:32,bottom:48},xAxis:{type:'category',data:cats},yAxis:{type:'value'}".to_string()
    };
    let tooltip_js = if chart == "pie" {
        "tooltip:{trigger:'item'}".to_string()
    } else {
        "tooltip:{trigger:'axis'}".to_string()
    };

    format!(
        r#"<!DOCTYPE html>
<html lang="zh"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{title}</title>
<script src="https://cdn.jsdelivr.net/npm/echarts@5/dist/echarts.min.js"></script>
<style>
  body{{margin:0;background:#0b0f1a;color:#e6edf3;font-family:system-ui,sans-serif;}}
  #chart{{width:100vw;height:88vh;}}
  h1{{padding:16px 24px 0;font-size:18px;font-weight:600;}}
</style></head>
<body>
<h1>{title}</h1>
<div id="chart"></div>
<script>
var cats={cats_json};
var ser={vals_json};
var chart=echarts.init(document.getElementById('chart'),'dark');
chart.setOption({{
  {tooltip},
  {coord},
  {series}
}});
window.addEventListener('resize',function(){{chart.resize();}});
</script>
</body></html>"#,
        title = title,
        cats_json = cats_json,
        vals_json = vals_json,
        tooltip = tooltip_js,
        coord = coord_js,
        series = series_js,
    )
}

// ───────────────── 存储/导出 ─────────────────

fn export_table(table: &Table, path: &str, format: &str) -> Result<usize, String> {
    let path = resolve_path(path);
    match format {
        "json" => {
            let content = table.to_json().to_string();
            std::fs::write(&path, content).map_err(|e| e.to_string())?;
            Ok(table.rows.len())
        }
        _ => {
            // CSV
            let mut wtr = csv::Writer::from_path(&path).map_err(|e| e.to_string())?;
            wtr.write_record(&table.columns).map_err(|e| e.to_string())?;
            for r in &table.rows {
                wtr.write_record(r).map_err(|e| e.to_string())?;
            }
            wtr.flush().map_err(|e| e.to_string())?;
            Ok(table.rows.len())
        }
    }
}

// ───────────────── 工具 ─────────────────

/// data_ingest：采集并解析数据
pub struct DataIngestTool;

impl Tool for DataIngestTool {
    fn name(&self) -> &str {
        "data_ingest"
    }
    fn description(&self) -> &str {
        "采集并解析数据：读取本地 CSV/JSON 文件（或直接传入文本内容），转换为结构化表格 {columns, rows, count}。format: csv|json|auto（默认 auto 按内容自动识别）。返回的 data 可继续传给 data_clean / data_aggregate / data_viz / data_export"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "input": { "type": "string", "description": "本地文件绝对路径，或 CSV/JSON 文本内容" },
                "format": { "type": "string", "enum": ["auto", "csv", "json"], "description": "数据格式，默认 auto" }
            },
            "required": ["input"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let input = args["input"].as_str().ok_or("缺少参数 input")?;
        let format = args["format"].as_str().unwrap_or("auto");
        let raw = read_input(input)?;
        let table = parse_text(&raw, format)?;
        Ok(table.to_json())
    }
}

/// data_clean：清洗数据
pub struct DataCleanTool;

impl Tool for DataCleanTool {
    fn name(&self) -> &str {
        "data_clean"
    }
    fn description(&self) -> &str {
        "清洗表格数据：去重（按整行）、裁剪空白、缺失填充、指定列转数值。输入为 data_ingest 的返回（或 {columns, rows} 对象）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "data": { "type": "object", "description": "表格对象 {columns, rows}（来自 data_ingest）" },
                "dedup": { "type": "boolean", "description": "是否按整行去重，默认 true" },
                "trim": { "type": "boolean", "description": "是否裁剪首尾空白，默认 true" },
                "fill": { "type": "string", "description": "缺失值填充，默认空串" },
                "numeric_columns": { "type": "array", "items": { "type": "string" }, "description": "要转为数值的列名列表" }
            },
            "required": ["data"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let table = json_to_table(&args["data"])?;
        let cleaned = clean_table(&table, &args);
        Ok(cleaned.to_json())
    }
}

/// data_aggregate：分组聚合
pub struct DataAggregateTool;

impl Tool for DataAggregateTool {
    fn name(&self) -> &str {
        "data_aggregate"
    }
    fn description(&self) -> &str {
        "对表格按列分组聚合。group_by 为分组列名（留空则全量聚合），value 为数值列名，op 为聚合方式 sum/avg/count/min/max。返回结果额外带 categories/values 数组，可直接传给 data_viz"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "data": { "type": "object", "description": "表格对象 {columns, rows}（来自 data_ingest / data_clean）" },
                "group_by": { "type": "string", "description": "分组列名，留空表示全量聚合" },
                "value": { "type": "string", "description": "要聚合的数值列名" },
                "op": { "type": "string", "enum": ["sum", "avg", "count", "min", "max"], "description": "聚合方式，默认 sum" }
            },
            "required": ["data", "value"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let table = json_to_table(&args["data"])?;
        let result = aggregate_json(&table, &args)?;
        Ok(result)
    }
}

fn aggregate_json(table: &Table, args: &Value) -> Result<Value, String> {
    let group_by = args["group_by"].as_str().unwrap_or("");
    let agg_col = args["value"].as_str().unwrap_or("");
    let agg_fn = args["op"].as_str().unwrap_or("sum");

    let vi = table
        .column_index(agg_col)
        .ok_or_else(|| format!("数值列不存在: {agg_col}"))?;
    let gi = if group_by.is_empty() {
        None
    } else {
        Some(
            table
                .column_index(group_by)
                .ok_or_else(|| format!("分组列不存在: {group_by}"))?,
        )
    };

    let mut groups: Vec<(String, Vec<f64>)> = Vec::new();
    let mut idx_map: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for r in &table.rows {
        let key = gi
            .map(|i| r.get(i).cloned().unwrap_or_default())
            .unwrap_or_else(|| "(全部)".to_string());
        let val = r.get(vi).and_then(|s| parse_num(s)).unwrap_or(0.0);
        let pos = *idx_map.entry(key.clone()).or_insert_with(|| {
            groups.push((key, Vec::new()));
            groups.len() - 1
        });
        groups[pos].1.push(val);
    }

    let mut categories = Vec::new();
    let mut values = Vec::new();
    let mut rows = Vec::new();
    for (key, nums) in groups {
        let agg = match agg_fn {
            "avg" | "mean" => nums.iter().sum::<f64>() / nums.len().max(1) as f64,
            "count" => nums.len() as f64,
            "min" => nums.iter().cloned().fold(f64::INFINITY, f64::min),
            "max" => nums.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
            _ => nums.iter().sum::<f64>(),
        };
        categories.push(key.clone());
        values.push(agg);
        rows.push(vec![key, format!("{agg}")]);
    }

    Ok(json!({
        "columns": [
            if group_by.is_empty() { "group".to_string() } else { group_by.to_string() },
            format!("{agg_col}_{agg_fn}"),
        ],
        "rows": rows,
        "categories": categories,
        "values": values,
    }))
}

/// data_viz：生成 ECharts 图表并在浏览器渲染
pub struct DataVizTool {
    app: AppHandle,
    browser: Arc<Mutex<BrowserState>>,
}

impl DataVizTool {
    pub fn new(app: AppHandle, browser: Arc<Mutex<BrowserState>>) -> Self {
        Self { app, browser }
    }
}

impl Tool for DataVizTool {
    fn name(&self) -> &str {
        "data_viz"
    }
    fn description(&self) -> &str {
        "把表格数据生成 ECharts 交互图表并在内置浏览器窗口渲染。chart: bar柱状|line折线|pie饼图。x 为分类列名，y 为数值列名；亦可直接传 data_aggregate 的结果（自动用 categories/values）。生成完整 HTML 并展示"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "data": { "type": "object", "description": "表格对象 {columns, rows}，或 data_aggregate 的返回（含 categories/values）" },
                "chart": { "type": "string", "enum": ["bar", "line", "pie"], "description": "图表类型，默认 bar" },
                "x": { "type": "string", "description": "分类列名（可选，缺省取第一列）" },
                "y": { "type": "string", "description": "数值列名（可选，缺省取第二列）" },
                "title": { "type": "string", "description": "图表标题" }
            },
            "required": ["data"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let chart = args["chart"].as_str().unwrap_or("bar");
        let title = args["title"].as_str().unwrap_or("数据图表").to_string();
        let data = &args["data"];

        // 优先复用 data_aggregate 输出的 categories/values
        let (categories, values) = if data.get("categories").and_then(|v| v.as_array()).is_some()
            && data.get("values").and_then(|v| v.as_array()).is_some()
        {
            let cats: Vec<String> = data["categories"]
                .as_array()
                .map(|a| a.iter().map(scalar_to_str).collect())
                .unwrap_or_default();
            let vals: Vec<f64> = data["values"]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_f64()).collect())
                .unwrap_or_default();
            (cats, vals)
        } else {
            let table = json_to_table(data)?;
            let xi = args["x"]
                .as_str()
                .and_then(|c| table.column_index(c))
                .unwrap_or(0);
            let yi = args["y"]
                .as_str()
                .and_then(|c| table.column_index(c))
                .unwrap_or(1.min(table.columns.len().saturating_sub(1)));
            let cats = table
                .rows
                .iter()
                .map(|r| r.get(xi).cloned().unwrap_or_default())
                .collect::<Vec<_>>();
            let vals = table
                .rows
                .iter()
                .filter_map(|r| r.get(yi).and_then(|v| parse_num(v)))
                .collect::<Vec<_>>();
            (cats, vals)
        };

        let html = build_echarts_html(chart, &title, &categories, &values);

        {
            let mut s = self.browser.lock().unwrap();
            s.open_tab("html", &title, &html);
        }
        crate::windows::ensure_browser_window(&self.app);
        let snap = self.browser.lock().unwrap().snapshot();
        let _ = self.app.emit_to("browser", "browser-update", &snap);

        Ok(json!({
            "ok": true,
            "chart": chart,
            "title": title,
            "categories": categories,
            "values": values,
        }))
    }
}

/// data_report：生成图文报告到文档窗口
pub struct DataReportTool {
    app: AppHandle,
    markdown: Arc<Mutex<MarkdownState>>,
}

impl DataReportTool {
    pub fn new(app: AppHandle, markdown: Arc<Mutex<MarkdownState>>) -> Self {
        Self { app, markdown }
    }
}

impl Tool for DataReportTool {
    fn name(&self) -> &str {
        "data_report"
    }
    fn description(&self) -> &str {
        "把表格数据 + 结论整理成一份 Markdown 报告，写入右侧文档窗口。title 为报告标题，data 可选（追加数据摘要表），conclusion 为结论/洞察"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "title": { "type": "string", "description": "报告标题" },
                "data": { "type": "object", "description": "可选：表格对象 {columns, rows}，用于生成数据摘要表" },
                "conclusion": { "type": "string", "description": "结论与洞察（Markdown 文本）" }
            },
            "required": ["title"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let title = args["title"].as_str().unwrap_or("数据报告").to_string();
        let mut md = format!("# {title}\n\n");
        // 时间戳
        md.push_str(&format!(
            "> 由白泽自动生成 · {}\n\n",
            chrono::Local::now().format("%Y-%m-%d %H:%M")
        ));

        if let Some(data) = args.get("data") {
            if let Ok(table) = json_to_table(data) {
                if !table.columns.is_empty() {
                    md.push_str("## 数据摘要\n\n");
                    md.push_str("| ");
                    md.push_str(&table.columns.join(" | "));
                    md.push_str(" |\n");
                    md.push_str(&"---|".repeat(table.columns.len()));
                    md.push('\n');
                    let max_rows = table.rows.len().min(50);
                    for r in table.rows.iter().take(max_rows) {
                        md.push_str("| ");
                        md.push_str(&r.join(" | "));
                        md.push_str(" |\n");
                    }
                    md.push('\n');
                }
            }
        }

        if let Some(conclusion) = args["conclusion"].as_str() {
            if !conclusion.trim().is_empty() {
                md.push_str("## 结论与洞察\n\n");
                md.push_str(conclusion.trim());
                md.push('\n');
            }
        }

        crate::markdown::write_document(&self.app, &self.markdown, &title, &md);
        Ok(json!({ "ok": true, "title": title, "chars": md.chars().count() }))
    }
}

/// data_export：把结果表写回 CSV/JSON
pub struct DataExportTool;

impl Tool for DataExportTool {
    fn name(&self) -> &str {
        "data_export"
    }
    fn description(&self) -> &str {
        "把表格数据导出为 CSV 或 JSON 文件（写入本地）。format: csv|json"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "data": { "type": "object", "description": "表格对象 {columns, rows}" },
                "path": { "type": "string", "description": "输出文件路径" },
                "format": { "type": "string", "enum": ["csv", "json"], "description": "导出格式，默认 csv" }
            },
            "required": ["data", "path"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let table = json_to_table(&args["data"])?;
        let path = args["path"].as_str().ok_or("缺少参数 path")?;
        let format = args["format"].as_str().unwrap_or("csv");
        let n = export_table(&table, path, format)?;
        Ok(json!({ "ok": true, "path": resolve_path(path), "rows": n }))
    }
}