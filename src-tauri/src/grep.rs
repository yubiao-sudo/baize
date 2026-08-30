//! 全局文件内容搜索：实时递归遍历 + 正则匹配，无需预建索引
//!
//! 适合小到中等规模目录；返回命中文件、行号与行内容。

use serde_json::{json, Value};

use crate::tools::{resolve_path, PermissionClass, Tool};

pub struct GrepTool;

impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep_files"
    }
    fn description(&self) -> &str {
        "递归搜索目录（或单个文件）内的文本内容，用正则匹配，返回命中文件、行号与行内容。实时遍历不建索引，适合小到中等规模目录"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "要搜索的目录或文件，缺省为工作空间根目录" },
                "pattern": { "type": "string", "description": "正则表达式，如 error|warn" },
                "ignore_case": { "type": "boolean", "description": "忽略大小写，默认 false" },
                "include": { "type": "string", "description": "仅搜索这些扩展名，逗号分隔，如 rs,toml,md（缺省不限）" },
                "max_results": { "type": "integer", "description": "最多返回条数，默认 200" }
            },
            "required": ["pattern"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let pattern = args["pattern"].as_str().ok_or("缺少参数 pattern")?;
        let raw_path = args["path"].as_str().unwrap_or(".");
        let path = resolve_path(raw_path);
        let ignore_case = args["ignore_case"].as_bool().unwrap_or(false);
        let max_results = args["max_results"].as_u64().unwrap_or(200).clamp(1, 5000) as usize;
        let include: Option<Vec<String>> = args["include"].as_str().map(|s| {
            s.split(',')
                .map(|x| x.trim().trim_start_matches('.').to_lowercase())
                .filter(|x| !x.is_empty())
                .collect()
        });

        let re = regex::RegexBuilder::new(pattern)
            .case_insensitive(ignore_case)
            .build()
            .map_err(|e| format!("正则表达式无效: {e}"))?;

        let meta = std::fs::metadata(&path).map_err(|e| format!("路径不可访问: {e}"))?;
        let mut results: Vec<Value> = Vec::new();

        if meta.is_file() {
            search_file(std::path::Path::new(&path), &re, max_results, &mut results);
        } else {
            for entry in walkdir::WalkDir::new(&path) {
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                if !entry.file_type().is_file() {
                    continue;
                }
                if !ext_allowed(entry.path(), &include) {
                    continue;
                }
                search_file(entry.path(), &re, max_results, &mut results);
                if results.len() >= max_results {
                    break;
                }
            }
        }

        Ok(json!({
            "path": path,
            "pattern": pattern,
            "count": results.len(),
            "matches": results,
        }))
    }
}

/// 按扩展名白名单过滤文件（None 表示不限）
fn ext_allowed(path: &std::path::Path, include: &Option<Vec<String>>) -> bool {
    match include {
        None => true,
        Some(exts) => match path.extension().and_then(|e| e.to_str()) {
            Some(ext) => exts.contains(&ext.to_lowercase()),
            None => false,
        },
    }
}

/// 在单个文件内搜索，把命中行（含行号）追加到 results，直到达到上限
fn search_file(path: &std::path::Path, re: &regex::Regex, max_results: usize, results: &mut Vec<Value>) {
    if results.len() >= max_results {
        return;
    }
    // 非 UTF-8 / 二进制文件直接跳过
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return,
    };
    for (idx, line) in content.lines().enumerate() {
        if results.len() >= max_results {
            break;
        }
        if re.is_match(line) {
            let text = if line.chars().count() > 500 {
                let mut t: String = line.chars().take(500).collect();
                t.push('…');
                t
            } else {
                line.to_string()
            };
            results.push(json!({
                "path": path.display().to_string(),
                "line": idx + 1,
                "text": text,
            }));
        }
    }
}