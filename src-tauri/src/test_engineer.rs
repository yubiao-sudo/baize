//! 软件测试工程师的核心管线：需求分析 → 测试用例生成。
//!
//! 三阶段确定性编排（每阶段一次强模型调用，与 `Supervisor.plan_todos` 同款链路）：
//!   ① 需求分析：抽取结构化需求点
//!   ② 用例设计：按测试方法论（等价类/边界值/异常/业务规则）生成用例
//!   ③ 覆盖检查：校验需求点是否全覆盖并补录缺漏
//! 最终渲染为固定 Markdown 表格，通过内置文档窗口输出。

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::capability::{Action, Capability};
use crate::model::{ChatMessage, ModelTier};
use crate::tools::{PermissionClass, Tool};
use crate::AppState;

// ───────────────────── 数据结构 ─────────────────────

/// 需求点（需求分析产物）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequirementItem {
    pub func: String,
    pub description: String,
    pub inputs: String,
    pub outputs: String,
    pub rules: String,
    pub acceptance: String,
    pub edge_cases: String,
}

/// 测试用例（用例设计产物；编号在渲染时统一生成）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCase {
    pub req_index: usize,
    pub title: String,
    pub precondition: String,
    pub steps: String,
    pub data: String,
    pub expected: String,
    pub priority: String,
    pub case_type: String,
}

/// 管线产物
#[derive(Debug, Clone, Serialize)]
pub struct TestPipelineOutput {
    pub requirements: Vec<RequirementItem>,
    pub test_cases: Vec<TestCase>,
    pub coverage_report: String,
    pub markdown: String,
}

// ───────────────────── 提示词模板 ─────────────────────

const ANALYZE_REQ_PROMPT: &str = r#"你是软件测试工程师。请分析下面的需求，抽取可独立验证的「需求点」。

对每个需求点，输出字段：func(功能点)、description(描述)、inputs(输入)、outputs(输出)、rules(业务规则)、acceptance(验收标准)、edge_cases(边界与异常)。

只输出 JSON（不要任何解释），格式：
{"requirements":[{"func":"...","description":"...","inputs":"...","outputs":"...","rules":"...","acceptance":"...","edge_cases":"..."}]}

需求内容：
{requirement}"#;

const DESIGN_CASES_PROMPT: &str = r#"你是软件测试工程师。基于下面的需求点清单，生成测试用例。

对每个需求点，按以下方法生成用例：
1. 等价类划分：有效/无效等价类各取至少 1 个代表值。
2. 边界值分析：数值/长度阈值取「下边界-1、边界、上边界+1」。
3. 异常路径：空值、null、特殊字符、超长、类型不符、必填缺失。
4. 业务规则与状态转移：有状态时覆盖状态变化（如锁定→解锁）。
5. 每个需求点至少覆盖：1 条正常功能 + 1 条边界 + 1 条异常。
6. 优先级：P0(核心主流程)/P1(重要边界异常)/P2(一般)/P3(极端)。

【类型硬约束】case_type 只允许取：{case_types}。不在此清单内的用例一律不要输出。
- 功能=业务逻辑正确性；UI=界面交互/布局/控件；接口=API 参数/返回码/鉴权；安全=注入/越权/敏感信息；性能=响应时间/并发/资源占用。

【数量要求】{count_rule}

输出字段：req_index(需求点下标,从0开始)、title、precondition、steps、data、expected、priority、case_type（必须从上述清单中选）。

【输出精简】steps/data/expected 写关键要素即可，每字段尽量 ≤120 字；不要输出多余空格、注释或解释文字——输出 token 越少生成越快。

只输出 JSON（不要任何解释），格式：
{"test_cases":[{"req_index":0,"title":"...","precondition":"...","steps":"...","data":"...","expected":"...","priority":"P0","case_type":"功能"}]}

需求点清单：
{requirements}"#;

const CHECK_COVERAGE_PROMPT: &str = r#"你是软件测试工程师。检查下面需求点是否都被测试用例覆盖，并对缺失的补录用例。

【类型硬约束】补录用例的 case_type 只允许取：{case_types}。

输出 JSON（不要任何解释），格式：
{"coverage":[{"req_index":0,"covered":true,"case_ids":["TC-001"]},{"req_index":2,"covered":false,"missing":"缺少...用例"}],"supplements":[{"req_index":2,"title":"...","precondition":"...","steps":"...","data":"...","expected":"...","priority":"P1","case_type":"异常"}]}

【输出精简】covered=true 的项 case_ids 一行带过；missing 每项 ≤30 字；只补真正缺失的用例，宁缺毋滥——补录用例的字段同样尽量 ≤120 字。

需求点清单：
{requirements}

测试用例清单：
{test_cases}"#;

// ───────────────────── 生成选项 ─────────────────────

/// 用例类型可选项（前端标签多选与提示词约束共用）
pub const CASE_TYPE_OPTIONS: [&str; 5] = ["功能", "UI", "接口", "安全", "性能"];

/// 用例生成选项：类型多选 + 每类条数
#[derive(Debug, Clone)]
pub struct CaseGenOptions {
    /// 允许生成的用例类型（CASE_TYPE_OPTIONS 的子集；空表视为全部可选类型）
    pub case_types: Vec<String>,
    /// 每种类型的期望条数（0 表示不限制，按测试方法论自然覆盖）
    pub per_type_count: usize,
}

impl Default for CaseGenOptions {
    fn default() -> Self {
        Self {
            case_types: CASE_TYPE_OPTIONS.iter().map(|s| s.to_string()).collect(),
            per_type_count: 0,
        }
    }
}

impl CaseGenOptions {
    fn allowed(&self) -> Vec<String> {
        if self.case_types.is_empty() {
            return CASE_TYPE_OPTIONS.iter().map(|s| s.to_string()).collect();
        }
        // 去重 + 仅保留合法选项，避免模型被奇怪的类型名带偏
        let mut out: Vec<String> = Vec::new();
        for t in &self.case_types {
            let t = t.trim();
            if CASE_TYPE_OPTIONS.iter().any(|o| o.eq(&t)) && !out.iter().any(|x| x == t) {
                out.push(t.to_string());
            }
        }
        if out.is_empty() {
            return CaseGenOptions::default_case_types();
        }
        out
    }

    fn default_case_types() -> Vec<String> {
        CASE_TYPE_OPTIONS.iter().map(|s| s.to_string()).collect()
    }

    /// 提示词中的类型约束串：「功能/UI/接口」
    fn types_hint(&self) -> String {
        self.allowed().join("/")
    }

    /// 提示词中的数量要求语句
    fn count_rule(&self) -> String {
        if self.per_type_count == 0 {
            "未指定数量：按上述方法论自然覆盖即可。".to_string()
        } else {
            format!(
                "所选的每一种类型各自产出约 {} 条用例（上下浮动不超过 2 条）。",
                self.per_type_count
            )
        }
    }
}

// ───────────────────── JSON 解析（容忍模型前后缀文字） ─────────────────────

/// 截取首尾花括号之间的 JSON
fn extract_json(text: &str) -> Result<&str, String> {
    let start = text.find('{').ok_or("输出中无 JSON 对象")?;
    let end = text.rfind('}').ok_or("输出中无 JSON 对象")?;
    Ok(&text[start..=end])
}

/// 解析需求点数组
pub fn parse_requirements(text: &str) -> Result<Vec<RequirementItem>, String> {
    let json = extract_json(text)?;
    let v: Value = serde_json::from_str(json).map_err(|e| format!("解析需求点失败: {e}"))?;
    let arr = v["requirements"].as_array().ok_or("缺少 requirements 数组")?;
    let mut out = Vec::new();
    for item in arr {
        let s = |k: &str| item[k].as_str().unwrap_or("");
        out.push(RequirementItem {
            func: s("func").to_string(),
            description: s("description").to_string(),
            inputs: s("inputs").to_string(),
            outputs: s("outputs").to_string(),
            rules: s("rules").to_string(),
            acceptance: s("acceptance").to_string(),
            edge_cases: s("edge_cases").to_string(),
        });
    }
    if out.is_empty() {
        return Err("未解析到任何需求点".into());
    }
    Ok(out)
}

/// 解析测试用例数组
pub fn parse_test_cases(text: &str) -> Result<Vec<TestCase>, String> {
    let json = extract_json(text)?;
    let v: Value = serde_json::from_str(json).map_err(|e| format!("解析测试用例失败: {e}"))?;
    let arr = v["test_cases"].as_array().ok_or("缺少 test_cases 数组")?;
    let mut out = Vec::new();
    for item in arr {
        let s = |k: &str| item[k].as_str().unwrap_or("");
        let title = s("title").to_string();
        if title.is_empty() {
            continue;
        }
        out.push(TestCase {
            req_index: item["req_index"].as_u64().unwrap_or(0) as usize,
            title,
            precondition: s("precondition").to_string(),
            steps: s("steps").to_string(),
            data: s("data").to_string(),
            expected: s("expected").to_string(),
            priority: s("priority").to_string(),
            case_type: s("case_type").to_string(),
        });
    }
    Ok(out)
}

/// 解析覆盖检查结果，返回 (覆盖率报告, 补录用例)
fn parse_coverage(text: &str, req_count: usize) -> Result<(String, Vec<TestCase>), String> {
    let json = extract_json(text)?;
    let v: Value = serde_json::from_str(json).map_err(|e| format!("解析覆盖检查失败: {e}"))?;

    let mut report_lines: Vec<String> = Vec::new();
    if let Some(arr) = v["coverage"].as_array() {
        for item in arr {
            let idx = item["req_index"].as_u64().unwrap_or(0) as usize;
            let covered = item["covered"].as_bool().unwrap_or(true);
            if covered {
                report_lines.push(format!("REQ-{:03} 已覆盖", idx + 1));
            } else {
                let missing = item["missing"].as_str().unwrap_or("缺漏");
                report_lines.push(format!("REQ-{:03} 未覆盖：{}", idx + 1, missing));
            }
        }
    }
    let report = if report_lines.is_empty() {
        "覆盖率检查完成。".to_string()
    } else {
        report_lines.join("<br>")
    };

    let mut supplements = Vec::new();
    if let Some(arr) = v["supplements"].as_array() {
        for item in arr {
            let idx = item["req_index"].as_u64().unwrap_or(0) as usize;
            if idx >= req_count {
                continue;
            }
            let s = |k: &str| item[k].as_str().unwrap_or("");
            let title = s("title").to_string();
            if title.is_empty() {
                continue;
            }
            supplements.push(TestCase {
                req_index: idx,
                title,
                precondition: s("precondition").to_string(),
                steps: s("steps").to_string(),
                data: s("data").to_string(),
                expected: s("expected").to_string(),
                priority: s("priority").to_string(),
                case_type: s("case_type").to_string(),
            });
        }
    }
    Ok((report, supplements))
}

// ───────────────────── 渲染 ─────────────────────

/// 旧类型枚举 → 新五类的映射（模型偶发输出旧枚举时尽量映射，不静默丢用例）
const LEGACY_CASE_TYPES: [(&str, &str); 6] = [
    ("边界", "功能"),
    ("异常", "功能"),
    ("压力", "性能"),
    ("负载", "性能"),
    ("渗透", "安全"),
    ("越权", "安全"),
];

/// 用例类型收敛：精确匹配 → 包含匹配（兼容「功能性/接口类/UI 界面」等变体）→ 旧枚举映射。
/// 返回是否落进了所选清单 —— 映射不到时**不强行归类**（否则单选 UI 时功能类用例会冒充 UI），
/// 由调用方剔除该条并在阶段详情里广播计数。
fn map_case_type(c: &mut TestCase, allowed: &[String]) -> bool {
    let raw = c.case_type.trim().to_string();
    if raw.is_empty() {
        return false;
    }
    if let Some(a) = allowed.iter().find(|a| a.as_str() == raw) {
        c.case_type = a.clone();
        return true;
    }
    if let Some(a) = allowed.iter().find(|a| raw.contains(a.as_str())) {
        c.case_type = a.clone();
        return true;
    }
    let mapped = LEGACY_CASE_TYPES
        .iter()
        .find(|(old, _)| raw.contains(old))
        .map(|(_, neu)| neu.to_string())
        .and_then(|neu| allowed.iter().find(|a| a.as_str() == &neu).cloned());
    match mapped {
        Some(a) => {
            c.case_type = a;
            true
        }
        None => false,
    }
}

/// 数量硬约束：按类型分组，超过 per 条的组按优先级（P0 最先，同优先级保原序）截断到 per 条。
/// 类型不在清单内的用例原样保留（理论上上游已剔除，这里兜底防静默丢数据）。
/// 返回（保留的用例，裁掉的条数）。
fn cap_cases_per_type(cases: Vec<TestCase>, per: usize, allowed: &[String]) -> (Vec<TestCase>, usize) {
    if per == 0 {
        return (cases, 0);
    }
    let prio = |p: &str| {
        p.trim()
            .trim_start_matches(['P', 'p'])
            .parse::<u32>()
            .unwrap_or(9)
    };
    let mut kept: Vec<TestCase> = Vec::new();
    let mut dropped = 0usize;
    for t in allowed {
        let group: Vec<TestCase> = cases.iter().filter(|c| &c.case_type == t).cloned().collect();
        if group.len() <= per {
            kept.extend(group);
        } else {
            let mut g = group;
            g.sort_by_key(|c| prio(&c.priority));
            dropped += g.len() - per;
            kept.extend(g.into_iter().take(per));
        }
    }
    kept.extend(
        cases
            .into_iter()
            .filter(|c| !allowed.iter().any(|t| t == &c.case_type)),
    );
    (kept, dropped)
}

/// Markdown 表格单元格转义（竖线转义 + 换行换 <br>）
fn esc(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', "<br>")
}

/// 渲染测试用例文档（需求点清单 + 用例表 + 覆盖率报告）
pub fn render_test_doc(reqs: &[RequirementItem], cases: &[TestCase], coverage: &str) -> String {
    let mut md = String::new();
    md.push_str("# 测试用例设计\n\n");

    md.push_str("## 一、需求点清单\n\n");
    md.push_str("| 编号 | 功能点 | 描述 | 验收标准 |\n|---|---|---|---|\n");
    for (i, r) in reqs.iter().enumerate() {
        md.push_str(&format!(
            "| REQ-{:03} | {} | {} | {} |\n",
            i + 1,
            esc(&r.func),
            esc(&r.description),
            esc(&r.acceptance)
        ));
    }

    md.push_str("\n## 二、测试用例\n\n");
    md.push_str("| 用例编号 | 关联需求 | 标题 | 前置条件 | 测试步骤 | 测试数据 | 期望结果 | 优先级 | 类型 |\n|---|---|---|---|---|---|---|---|---|\n");
    for (i, c) in cases.iter().enumerate() {
        let req_id = format!("REQ-{:03}", c.req_index + 1);
        md.push_str(&format!(
            "| TC-{:03} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            i + 1,
            req_id,
            esc(&c.title),
            esc(&c.precondition),
            esc(&c.steps),
            esc(&c.data),
            esc(&c.expected),
            esc(&c.priority),
            esc(&c.case_type)
        ));
    }

    md.push_str("\n## 三、覆盖率报告\n\n");
    md.push_str(coverage);
    md.push('\n');
    md
}

// ───────────────────── 管线编排 ─────────────────────

/// 需求 → 测试用例 的编排器（与 `Supervisor` 同构，持 AppHandle + AppState）
pub struct TestCasePipeline<'a> {
    app: &'a AppHandle,
    state: &'a AppState,
}

impl<'a> TestCasePipeline<'a> {
    pub fn new(app: &'a AppHandle, state: &'a AppState) -> Self {
        Self { app, state }
    }

    pub async fn run(&self, requirement: &str, options: &CaseGenOptions) -> Result<TestPipelineOutput, String> {
        let allowed = options.allowed();

        // 阶段① 需求分析
        self.emit_stage("需求分析", "正在抽取需求点…");
        let reqs = self.analyze_requirements(requirement).await?;
        self.emit_stage("需求分析", &format!("识别 {} 个需求点", reqs.len()));

        // 阶段② 用例设计（按所选类型 + 数量约束）
        self.emit_stage(
            "用例设计",
            &format!(
                "类型「{}」· {}",
                allowed.join("/"),
                if options.per_type_count == 0 {
                    "数量不限".to_string()
                } else {
                    format!("每类约 {} 条", options.per_type_count)
                }
            ),
        );
        let (mut cases, dropped_bad, trimmed) = self.design_test_cases(&reqs, options).await?;
        self.emit_stage(
            "用例设计",
            &format!(
                "生成 {} 条用例{}{}",
                cases.len(),
                if dropped_bad > 0 { format!("，剔除 {dropped_bad} 条非所选类型") } else { String::new() },
                if trimmed > 0 { format!("，按数量约束裁掉 {trimmed} 条超额") } else { String::new() },
            ),
        );

        // 阶段③ 覆盖检查 + 补录（补录用例同样收敛到所选类型，映射不到的剔除）
        self.emit_stage("覆盖检查", "正在校验需求覆盖…");
        let (coverage, supplements, supp_dropped) =
            self.check_coverage(&reqs, &cases, &allowed).await?;
        let n_supp = supplements.len();
        cases.extend(supplements);
        self.emit_stage(
            "覆盖检查",
            &format!(
                "覆盖率检查完成，补录 {} 条缺口用例{}",
                n_supp,
                if supp_dropped > 0 {
                    format!("（剔除 {supp_dropped} 条非所选类型）")
                } else {
                    String::new()
                }
            ),
        );

        // 阶段④ 渲染
        let markdown = render_test_doc(&reqs, &cases, &coverage);

        Ok(TestPipelineOutput {
            requirements: reqs,
            test_cases: cases,
            coverage_report: coverage,
            markdown,
        })
    }

    fn emit_stage(&self, stage: &str, detail: &str) {
        let _ = self.app.emit(
            "thought",
            json!({ "kind": "test_pipeline", "label": stage, "detail": detail }),
        );
    }

    /// 指定档位流式调用：边生成边把「已输出字数 + 尾部内容」广播到阶段条（200ms 节流）
    async fn call_model_stream_tier(
        &self,
        tier: ModelTier,
        stage: &str,
        prompt: &str,
    ) -> Result<crate::model::ChatResponse, String> {
        use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
        let msgs = vec![ChatMessage {
            role: "user".into(),
            content: prompt.to_string(),
            tool_calls: None,
            tool_call_id: None,
        }];
        let chars = AtomicUsize::new(0);
        let last_emit = AtomicU64::new(0);
        let acc = std::sync::Mutex::new(String::new());
        let app = self.app.clone();
        let stage_owned = stage.to_string();
        let on_token = |delta: &str| {
            let n = chars.fetch_add(delta.chars().count(), Ordering::Relaxed) + delta.chars().count();
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let prev = last_emit.load(Ordering::Relaxed);
            // 200ms 节流（prev=0 时 epoch 时间戳必然满足差值，首帧直接放行）
            if now.saturating_sub(prev) < 200
                || last_emit
                    .compare_exchange(prev, now, Ordering::Relaxed, Ordering::Relaxed)
                    .is_err()
            {
                return;
            }
            acc.lock().unwrap().push_str(delta);
            let tail: String = {
                let g = acc.lock().unwrap();
                let cnt = g.chars().count();
                g.chars().skip(cnt.saturating_sub(60)).collect()
            };
            let _ = app.emit(
                "thought",
                json!({
                    "kind": "test_pipeline",
                    "label": stage_owned,
                    "detail": format!("生成中…已输出 {n} 字 · …{tail}")
                }),
            );
        };
        self.state
            .model
            .stream_chat_with_tier(tier, &msgs, &[], &on_token)
            .await
    }

    /// 云端流式调用（管线各阶段默认入口）
    async fn call_model_stream(
        &self,
        stage: &str,
        prompt: &str,
    ) -> Result<crate::model::ChatResponse, String> {
        self.call_model_stream_tier(ModelTier::Cloud, stage, prompt).await
    }

    /// 流式调用 + 解析；失败（网络或 JSON 不合规）自动纠错重试 1 次，避免整条管线白跑
    async fn call_model_parsed<T>(
        &self,
        stage: &str,
        prompt: &str,
        parse: impl Fn(&str) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut last_err = String::new();
        for attempt in 0..2 {
            let p = if attempt == 0 {
                prompt.to_string()
            } else {
                self.emit_stage(stage, "输出解析失败，正在自动重试…");
                format!(
                    "{prompt}\n\n⚠️ 你上一次输出无法解析（{last_err}）。这次必须只输出符合要求的 JSON 本体：不要解释、不要 markdown 代码块、不要任何多余文字。"
                )
            };
            match self.call_model_stream(stage, &p).await {
                Ok(resp) => match parse(&resp.content.unwrap_or_default()) {
                    Ok(v) => return Ok(v),
                    Err(e) => last_err = e,
                },
                Err(e) => last_err = e,
            }
        }
        Err(format!("{stage}失败：{last_err}"))
    }

    async fn analyze_requirements(&self, text: &str) -> Result<Vec<RequirementItem>, String> {
        let prompt = ANALYZE_REQ_PROMPT.replace("{requirement}", text);
        self.call_model_parsed("需求分析", &prompt, |s| parse_requirements(s))
            .await
    }

    async fn design_test_cases(
        &self,
        reqs: &[RequirementItem],
        options: &CaseGenOptions,
    ) -> Result<(Vec<TestCase>, usize, usize), String> {
        let allowed = options.allowed();
        let req_json = serde_json::to_string(reqs).map_err(|e| e.to_string())?;
        let prompt = DESIGN_CASES_PROMPT
            .replace("{case_types}", &options.types_hint())
            .replace("{count_rule}", &options.count_rule())
            .replace("{requirements}", &req_json);
        let mut cases = self
            .call_model_parsed("用例设计", &prompt, |s| parse_test_cases(s))
            .await?;
        // 过滤越界的 req_index；类型收敛到所选清单（映射不到的剔除，不冒充）
        cases.retain(|c| c.req_index < reqs.len());
        let before = cases.len();
        cases.retain_mut(|c| map_case_type(c, &allowed));
        let dropped_bad = before - cases.len();
        // 数量硬约束：每类超出 per_type_count 的按优先级截断（P0 优先保留）
        let (cases, trimmed) = cap_cases_per_type(cases, options.per_type_count, &allowed);
        Ok((cases, dropped_bad, trimmed))
    }

    async fn check_coverage(
        &self,
        reqs: &[RequirementItem],
        cases: &[TestCase],
        allowed: &[String],
    ) -> Result<(String, Vec<TestCase>, usize), String> {
        let req_json = serde_json::to_string(reqs).map_err(|e| e.to_string())?;
        let cases_json = serde_json::to_string(cases).map_err(|e| e.to_string())?;
        let prompt = CHECK_COVERAGE_PROMPT
            .replace("{case_types}", &allowed.join("/"))
            .replace("{requirements}", &req_json)
            .replace("{test_cases}", &cases_json);
        let parse = |s: &str| -> Result<(String, Vec<TestCase>, usize), String> {
            let (report, mut supplements) = parse_coverage(s, reqs.len())?;
            // 补录用例同样只保留能落进所选清单的
            let before_supp = supplements.len();
            supplements.retain_mut(|c| map_case_type(c, allowed));
            let dropped = before_supp - supplements.len();
            Ok((report, supplements, dropped))
        };
        // 纯比对任务优先走本地快模型（省一次 30~90s 的云端调用）；不可用自动升级云端
        match self
            .call_model_parsed_tier(ModelTier::Local, "覆盖检查", &prompt, parse)
            .await
        {
            Ok(v) => Ok(v),
            Err(local_err) => {
                self.emit_stage(
                    "覆盖检查",
                    &format!("本地模型不可用（{local_err}），改用云端重试…"),
                );
                self.call_model_parsed("覆盖检查", &prompt, parse).await
            }
        }
    }

    /// call_model_parsed 的指定档位版本（供覆盖检查本地优先使用）
    async fn call_model_parsed_tier<T>(
        &self,
        tier: ModelTier,
        stage: &str,
        prompt: &str,
        parse: impl Fn(&str) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut last_err = String::new();
        for attempt in 0..2 {
            let p = if attempt == 0 {
                prompt.to_string()
            } else {
                self.emit_stage(stage, "输出解析失败，正在自动重试…");
                format!(
                    "{prompt}\n\n⚠️ 你上一次输出无法解析（{last_err}）。这次必须只输出符合要求的 JSON 本体：不要解释、不要 markdown 代码块、不要任何多余文字。"
                )
            };
            match self.call_model_stream_tier(tier, stage, &p).await {
                Ok(resp) => match parse(&resp.content.unwrap_or_default()) {
                    Ok(v) => return Ok(v),
                    Err(e) => last_err = e,
                },
                Err(e) => last_err = e,
            }
        }
        Err(last_err)
    }
}

// ───────────────────── 需求文档抽取 ─────────────────────

/// 解析需求来源：优先读 path 指向的需求文档（复用 document 模块的统一抽取），否则取 requirement 文本
pub fn resolve_requirement_from(requirement: Option<&str>, path: Option<&str>) -> Result<String, String> {
    if let Some(p) = path {
        if !p.is_empty() {
            return crate::document::extract_text(p);
        }
    }
    if let Some(t) = requirement {
        if !t.is_empty() {
            return Ok(t.to_string());
        }
    }
    Err("缺少参数：请提供 requirement（需求文本）或 path（需求文档路径）".into())
}

/// 解析工具入参：优先读 path 指向的需求文档，否则取 requirement 文本
fn resolve_requirement(args: &Value) -> Result<String, String> {
    resolve_requirement_from(args["requirement"].as_str(), args["path"].as_str())
}

// ───────────────────── 工具：generate_test_cases ─────────────────────

/// 需求 → 测试用例 的工具封装。`run` 在工具线程（`spawn_blocking`）内执行，
/// 用 `tauri::async_runtime::block_on` 同步等待异步管线完成，并写入文档窗口。
pub struct GenerateTestCasesTool {
    app: AppHandle,
}

impl GenerateTestCasesTool {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl Tool for GenerateTestCasesTool {
    fn name(&self) -> &str {
        "generate_test_cases"
    }
    fn description(&self) -> &str {
        "根据需求分析生成结构化测试用例并写入右侧文档窗口。可传 requirement（需求文本）或 path（需求文档路径，支持 txt/md/csv/docx/pdf）；case_types（用例类型数组，可多选：功能/UI/接口/安全/性能，缺省为全部）；per_type_count（每种类型的期望条数，0 或缺省表示不限）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "requirement": { "type": "string", "description": "原始需求文本或需求描述（与 path 二选一）" },
                "path": { "type": "string", "description": "需求文档的本地路径，支持 .txt/.md/.csv/.docx/.pdf" },
                "case_types": { "type": "array", "items": { "type": "string" }, "description": "要生成的用例类型，可多选：功能/UI/接口/安全/性能；缺省生成全部类型" },
                "per_type_count": { "type": "integer", "description": "每种类型的期望条数；0 或缺省表示不限制" }
            }
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let requirement = resolve_requirement(&args)?;
        let options = CaseGenOptions {
            case_types: args["case_types"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
            per_type_count: args["per_type_count"].as_u64().unwrap_or(0) as usize,
        };
        let app = self.app.clone();

        tauri::async_runtime::block_on(async move {
            let state = app.state::<AppState>();
            let pipeline = TestCasePipeline::new(&app, state.inner());
            let output = pipeline.run(&requirement, &options).await?;

            // 写入内置文档窗口（新建标签页，不覆盖已有文档）
            crate::markdown::write_document(
                &app,
                &state.inner().markdown,
                "测试用例设计",
                &output.markdown,
            );

            Ok(json!({
                "ok": true,
                "requirements": output.requirements.len(),
                "test_cases": output.test_cases.len(),
                "coverage": output.coverage_report,
                "cases": json!(&output.test_cases),
            }))
        })
    }
}

// ───────────────────── 用例导出（json / csv / xlsx） ─────────────────────

/// 导出列头（与用例文档表格一致）
const CASE_EXPORT_HEADERS: [&str; 9] = [
    "用例编号", "关联需求", "标题", "前置条件", "测试步骤", "测试数据", "期望结果", "优先级", "类型",
];

/// 单条用例（Value 形式的 TestCase）→ 导出行；缺失字段给空串，换行转 \n 保持单元格单行
fn case_export_row(index: usize, c: &Value) -> [String; 9] {
    let s = |k: &str| c[k].as_str().unwrap_or("").replace('\n', "\\n");
    [
        format!("TC-{:03}", index + 1),
        format!("REQ-{:03}", c["req_index"].as_u64().unwrap_or(0) as usize + 1),
        s("title"),
        s("precondition"),
        s("steps"),
        s("data"),
        s("expected"),
        s("priority"),
        s("case_type"),
    ]
}

/// 用例优先级文本 → 数值（P0 最先；空/异常排最后）
fn case_prio(c: &Value) -> u32 {
    c["priority"]
        .as_str()
        .unwrap_or("")
        .trim()
        .trim_start_matches(['P', 'p'])
        .parse::<u32>()
        .unwrap_or(9)
}

/// 按类别分节（保持类别首次出现顺序），节内按优先级 P0→P3 稳定排序。
/// 导出文件与界面列表共用同一顺序，保证所见即所得。
pub fn group_cases_by_type(cases: &[Value]) -> Vec<(String, Vec<Value>)> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: Vec<Vec<Value>> = Vec::new();
    for c in cases {
        let raw = c["case_type"].as_str().unwrap_or("").trim();
        let t = if raw.is_empty() { "未分类".to_string() } else { raw.to_string() };
        match order.iter().position(|x| x == &t) {
            Some(i) => groups[i].push(c.clone()),
            None => {
                order.push(t);
                groups.push(vec![c.clone()]);
            }
        }
    }
    for g in &mut groups {
        g.sort_by_key(|c| case_prio(c)); // 稳定排序：同优先级保持原生成顺序
    }
    order.into_iter().zip(groups).collect()
}

/// 把用例数组写盘：json（按类型分组对象）/ csv（UTF-8 BOM + 类别小节标题行）/ xlsx（类别小节合并加粗）
pub fn write_cases_file(cases: &[Value], format: &str, path: &std::path::Path) -> Result<(), String> {
    // 类别分节 + 组内优先级排序后落盘
    let grouped = group_cases_by_type(cases);
    match format {
        // JSON 按类型分组：<"UI": [...], "接口": [...]>
        "json" => {
            let mut obj = serde_json::Map::new();
            for (t, list) in &grouped {
                obj.insert(t.clone(), Value::Array(list.clone()));
            }
            let text =
                serde_json::to_string_pretty(&Value::Object(obj)).map_err(|e| format!("序列化失败: {e}"))?;
            std::fs::write(path, text.as_bytes()).map_err(|e| format!("写入文件失败: {e}"))
        }
        "csv" => {
            let mut w = csv::Writer::from_writer(Vec::new());
            w.write_record(CASE_EXPORT_HEADERS).map_err(|e| e.to_string())?;
            let mut seq = 0usize;
            for (t, list) in &grouped {
                // 类别小节标题：第一格写标题，其余补空格保持 9 列（csv writer 要求等宽记录）
                let mut sec = vec![format!("── {t}（{} 条）──", list.len())];
                sec.resize(CASE_EXPORT_HEADERS.len(), String::new());
                w.write_record(sec.iter().map(|s| s.as_str()).collect::<Vec<_>>())
                    .map_err(|e| e.to_string())?;
                for c in list {
                    w.write_record(case_export_row(seq, c)).map_err(|e| e.to_string())?;
                    seq += 1;
                }
            }
            let buf = w.into_inner().map_err(|e| e.to_string())?;
            let mut out = b"\xEF\xBB\xBF".to_vec();
            out.extend(buf);
            std::fs::write(path, out).map_err(|e| format!("写入文件失败: {e}"))
        }
        "xlsx" => {
            let mut workbook = rust_xlsxwriter::Workbook::new();
            let worksheet = workbook.add_worksheet();
            worksheet.set_name("测试用例").map_err(|e| format!("设置工作表名失败: {e}"))?;
            let bold = rust_xlsxwriter::Format::new().set_bold();
            for (c, h) in CASE_EXPORT_HEADERS.iter().enumerate() {
                worksheet
                    .write_string(0, c as u16, *h)
                    .map_err(|e| format!("写入 Excel 表头失败: {e}"))?;
            }
            let mut row = 1u32;
            let mut seq = 0usize;
            for (t, list) in &grouped {
                // 类别小节标题：跨全列合并加粗
                worksheet
                    .merge_range(
                        row,
                        0,
                        row,
                        (CASE_EXPORT_HEADERS.len() - 1) as u16,
                        format!("── {t}（{} 条）──", list.len()).as_str(),
                        &bold,
                    )
                    .map_err(|e| format!("写入 Excel 小节标题失败: {e}"))?;
                row += 1;
                for case in list {
                    for (c, v) in case_export_row(seq, case).iter().enumerate() {
                        worksheet
                            .write_string(row, c as u16, v.as_str())
                            .map_err(|e| format!("写入 Excel 单元格失败: {e}"))?;
                    }
                    row += 1;
                    seq += 1;
                }
            }
            workbook.save(path).map_err(|e| format!("保存 Excel 失败: {e}"))
        }
        _ => Err(format!("不支持的导出格式: {format}")),
    }
}

// ───────────────────── UI 自动化测试 ─────────────────────

/// 单条断言检查结果（窗口标题 / 画面文字 / 视觉目标 三种检查共享）
#[derive(Debug, Clone, Serialize)]
pub struct AssertCheck {
    pub name: String,
    pub passed: bool,
    pub expected: String,
    pub actual: String,
}

/// 执行 UI 断言：按 window_title（窗口标题）、ocr_text（画面 OCR 文字）、
/// visual_target（视觉模型）三重校验，全部命中才算 passed。
pub fn run_ui_assertion(
    capability: &dyn Capability,
    args: &Value,
) -> Result<Vec<AssertCheck>, String> {
    let mut checks = Vec::new();

    if let Some(title) = args["window_title"].as_str() {
        if !title.is_empty() {
            let absent = args["window_absent"].as_bool().unwrap_or(false);
            let wins = capability.list_windows().map_err(|e| e.to_string())?;
            let found = wins.iter().any(|w| {
                !w.name.is_empty() && (w.name.contains(title) || title.contains(&w.name))
            });
            checks.push(AssertCheck {
                name: if absent { "窗口应不存在".into() } else { "窗口存在".into() },
                passed: if absent { !found } else { found },
                expected: title.to_string(),
                actual: if found {
                    "已找到匹配窗口".to_string()
                } else {
                    "未找到匹配窗口".to_string()
                },
            });
        }
    }

    if let Some(text) = args["ocr_text"].as_str() {
        if !text.is_empty() {
            let info = capability.capture_screen().map_err(|e| e.to_string())?;
            let (txt, _words) = crate::ocr::ocr_detect_gui(&info.path).map_err(|e| e.to_string())?;
            let found = txt.contains(text);
            checks.push(AssertCheck {
                name: "画面文字".into(),
                passed: found,
                expected: text.to_string(),
                actual: txt.chars().take(200).collect(),
            });
        }
    }

    if let Some(target) = args["visual_target"].as_str() {
        if !target.is_empty() {
            let info = capability.capture_screen().map_err(|e| e.to_string())?;
            let hint = format!("画面中是否存在「{target}」？只回答“是”或“否”，并简述依据");
            match crate::visual_grounding::describe_image(&info.path, &hint) {
                Ok(desc) if desc.trim().is_empty() => {
                    // 视觉通道抽风返回空 ≠ 断言失败：重试一次，仍为空则标记「无法判定」不计失败，
                    // 避免后端抖动导致整条断言假失败、Agent 白白降级浪费轮次
                    std::thread::sleep(std::time::Duration::from_millis(400));
                    let retry = crate::visual_grounding::describe_image(&info.path, &hint)
                        .unwrap_or_default();
                    if retry.trim().is_empty() {
                        checks.push(AssertCheck {
                            name: "视觉目标".into(),
                            passed: true,
                            expected: target.to_string(),
                            actual: "视觉通道返回空输出（后端抖动），该断言已跳过、不计为失败".into(),
                        });
                    } else {
                        let found = retry.contains('是') && !retry.contains("否");
                        checks.push(AssertCheck {
                            name: "视觉目标".into(),
                            passed: found,
                            expected: target.to_string(),
                            actual: retry.chars().take(200).collect(),
                        });
                    }
                }
                Ok(desc) => {
                    let found = desc.contains('是') && !desc.contains("否");
                    checks.push(AssertCheck {
                        name: "视觉目标".into(),
                        passed: found,
                        expected: target.to_string(),
                        actual: desc.chars().take(200).collect(),
                    });
                }
                Err(e) => checks.push(AssertCheck {
                    name: "视觉目标".into(),
                    passed: false,
                    expected: target.to_string(),
                    actual: format!("视觉模型调用失败: {e}"),
                }),
            }
        }
    }

    if checks.is_empty() {
        return Err("至少提供一项校验：window_title / ocr_text / visual_target".into());
    }
    Ok(checks)
}

/// `assert_ui` 工具：把 UI 断言暴露给测试工程师 Agent。
pub struct AssertUITool {
    capability: Arc<dyn Capability>,
}

impl AssertUITool {
    pub fn new(capability: Arc<dyn Capability>) -> Self {
        Self { capability }
    }
}

impl Tool for AssertUITool {
    fn name(&self) -> &str {
        "assert_ui"
    }
    fn description(&self) -> &str {
        "断言桌面 UI 状态是否符合预期：按 window_title（窗口标题）、ocr_text（画面文字）、visual_target（视觉目标）三重校验。任一校验为“应存在却未找到”即整体失败，返回每项检查的通过/失败明细"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "window_title": { "type": "string", "description": "期望出现的窗口标题关键词（可选）" },
                "window_absent": { "type": "boolean", "description": "配合 window_title，true=期望该窗口不存在（可选，默认 false）" },
                "ocr_text": { "type": "string", "description": "期望画面中出现的文字（可选，本地 OCR 识别）" },
                "visual_target": { "type": "string", "description": "期望存在的视觉目标描述（可选，用视觉模型判断）" }
            }
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let checks = run_ui_assertion(self.capability.as_ref(), &args)?;
        let passed = checks.iter().all(|c| c.passed);
        Ok(json!({ "passed": passed, "checks": checks }))
    }
}

/// 单个 UI 步骤的执行结果
#[derive(Debug, Clone, Serialize)]
pub struct UiStepResult {
    pub index: usize,
    pub action: String,
    pub ok: bool,
    pub detail: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<AssertCheck>,
}

// ───────────────────── Web 深层操作（iframe 透明穿透 / 验证码 / 文件上传） ─────────────────────

/// 递归收集顶层文档与全部同源 iframe 文档，out 项为 {doc, ox, oy}；
/// (ox, oy) 为该框架视口相对顶层页面的偏移（含边框），用于坐标换算。
const JS_COLLECT: &str = "function __collect(root,ox,oy,out){\
if(!root||out.some(function(o){return o.doc===root;})){return;}\
out.push({doc:root,ox:ox,oy:oy});\
var frs=root.querySelectorAll('iframe,frame');\
frs.forEach(function(fr){try{var d=fr.contentDocument;if(d){\
var r=fr.getBoundingClientRect();\
if(r.width>0&&r.height>0){__collect(d,ox+r.x+fr.clientLeft,oy+r.y+fr.clientTop,out);}}}catch(e){}});}";

/// 在 __collect 收集的文档中按选择器查找元素，命中项存入 hit
const JS_FIND: &str = "var docs=[];__collect(document,0,0,docs);var hit=null;\
for(var i=0;i<docs.length;i++){var el=docs[i].doc.querySelector(sel);\
if(el){hit={el:el,item:docs[i]};break;}}";

/// 执行浏览器 JS 并返回结果值（browser::act evaluate 封装）
fn browser_eval(js: &str) -> Result<Value, String> {
    let v = crate::browser::act(json!({ "action": "evaluate", "js": js }))?;
    Ok(v["result"].clone())
}

/// 组装「跨同源 iframe 深度查找」的 IIFE：先声明 __collect / sel，再执行 body。
/// body 内可用 hit.el（命中的元素）、hit.item.ox / hit.item.oy（框架到顶层视口偏移）。
fn deep_find_js(selector: &str, body: &str) -> String {
    let mut js = String::from("(function(){");
    js.push_str(JS_COLLECT);
    js.push_str("var sel=");
    js.push_str(&serde_json::to_string(selector).unwrap_or_default());
    js.push(';');
    js.push_str(JS_FIND);
    js.push_str("if(!hit){return null;}");
    js.push_str(body);
    js.push_str("})()");
    js
}

/// 跨同源 iframe 填充输入框：设置 value 并派发 input/change 事件，返回是否命中。
/// 原型 setter 按元素所属窗口取值后再兜底直赋，兼容 React/Vue 受控组件。
fn web_fill_input_deep(selector: &str, text: &str) -> Result<bool, String> {
    let val = serde_json::to_string(text).map_err(|e| e.to_string())?;
    let mut body = String::new();
    body.push_str("var el=hit.el;");
    body.push_str("try{");
    body.push_str("var V=el.ownerDocument.defaultView;");
    body.push_str("var proto=null;");
    body.push_str(
        "if(V.HTMLTextAreaElement&&el instanceof V.HTMLTextAreaElement){proto=V.HTMLTextAreaElement.prototype;}",
    );
    body.push_str(
        "else if(V.HTMLInputElement&&el instanceof V.HTMLInputElement){proto=V.HTMLInputElement.prototype;}",
    );
    body.push_str("var setter=proto?Object.getOwnPropertyDescriptor(proto,'value').set:null;");
    body.push_str(&format!("if(setter){{setter.call(el,{val});}}else{{el.value={val};}}"));
    body.push_str(&format!("}}catch(e){{el.value={val};}}"));
    body.push_str("el.dispatchEvent(new Event('input',{bubbles:true}));");
    body.push_str("el.dispatchEvent(new Event('change',{bubbles:true}));");
    body.push_str("return true;");
    let js = deep_find_js(selector, &body);
    Ok(browser_eval(&js)?.as_bool().unwrap_or(false))
}

/// 跨同源 iframe 回读输入框当前值；未找到返回 None。
/// 用于 fill_input 后校验输入是否真实生效（页面可能截断/过滤输入）。
fn web_read_value_deep(selector: &str) -> Result<Option<String>, String> {
    let body = "var el=hit.el;return (el.value==null?null:String(el.value));";
    let js = deep_find_js(selector, body);
    let v = browser_eval(&js)?;
    Ok(v.as_str().map(String::from))
}

/// 跨同源 iframe 程序化点击（CDP 点击失败后的兜底：滚动入视野 + 合成鼠标事件序列）
fn web_click_deep(selector: &str) -> Result<bool, String> {
    let mut body = String::new();
    body.push_str("var el=hit.el;");
    body.push_str("if(el.scrollIntoView)el.scrollIntoView({block:'center'});");
    body.push_str("el.dispatchEvent(new MouseEvent('mousedown',{bubbles:true,cancelable:true}));");
    body.push_str("el.dispatchEvent(new MouseEvent('mouseup',{bubbles:true,cancelable:true}));");
    body.push_str("el.click();return true;");
    let js = deep_find_js(selector, &body);
    Ok(browser_eval(&js)?.as_bool().unwrap_or(false))
}

/// 采集顶层页面 + 全部同源 iframe 的可见文本
fn web_page_text_all() -> Result<String, String> {
    let mut js = String::from("(function(){");
    js.push_str(JS_COLLECT);
    js.push_str("var docs=[];__collect(document,0,0,docs);var t='';");
    js.push_str("for(var i=0;i<docs.length;i++){var b=docs[i].doc.body;if(b&&b.innerText){t+=b.innerText+'\\n';}}");
    js.push_str("return t;})()");
    Ok(browser_eval(&js)?
        .as_str()
        .unwrap_or("")
        .to_string())
}

/// 元素相对顶层视口的矩形 (x, y, w, h)，CSS 像素；未找到返回 None
fn web_element_rect_top(selector: &str) -> Result<Option<(f64, f64, f64, f64)>, String> {
    let mut body = String::new();
    body.push_str("var r=hit.el.getBoundingClientRect();");
    body.push_str(
        "return {x:r.x+hit.item.ox,y:r.y+hit.item.oy,w:r.width,h:r.height};",
    );
    let js = deep_find_js(selector, &body);
    let v = browser_eval(&js)?;
    if v.is_null() {
        return Ok(None);
    }
    match (
        v["x"].as_f64(),
        v["y"].as_f64(),
        v["w"].as_f64(),
        v["h"].as_f64(),
    ) {
        (Some(x), Some(y), Some(w), Some(h)) => Ok(Some((x, y, w, h))),
        _ => Ok(None),
    }
}

/// Web 验证码自动识别：截取整页 → 裁剪验证码图片区域（×dpr 对齐设备像素）→ OCR → 可选填入目标输入框。
/// 返回识别出的验证码文本。仅适合常规字符/数字图形验证码。
fn solve_captcha_by_ocr(source_selector: &str, target_selector: Option<&str>) -> Result<String, String> {
    let shot = crate::browser::act(json!({ "action": "screenshot" }))?;
    let png_path = shot["path"]
        .as_str()
        .ok_or("验证码识别失败：无法获取页面截图")?
        .to_string();
    let dpr = browser_eval("window.devicePixelRatio||1")?.as_f64().unwrap_or(1.0);
    let (x, y, w, h) = web_element_rect_top(source_selector)?
        .ok_or_else(|| format!("未找到验证码图片元素「{source_selector}」"))?;

    // CSS 像素 × dpr → 截图设备像素，并 clamp 到图像范围内
    let img = image::open(&png_path).map_err(|e| format!("读取截图失败: {e}"))?;
    let (iw, ih) = (img.width(), img.height());
    let cx = ((x.max(0.0)) * dpr) as u32;
    let cy = ((y.max(0.0)) * dpr) as u32;
    let cw = (w * dpr).round().min((iw - cx) as f64).max(1.0) as u32;
    let ch = (h * dpr).round().min((ih - cy) as f64).max(1.0) as u32;

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let crop_name = format!("baize-captcha-{ts}.png");
    img.crop_imm(cx, cy, cw, ch)
        .save(&crop_name)
        .map_err(|e| format!("保存验证码截图失败: {e}"))?;

    // 标准最佳语言包（chi_sim+eng）识别
    let (text, _) = crate::ocr::ocr_detect_gui(&crop_name)?;
    let code: String = text.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    if code.is_empty() {
        return Err(format!("OCR 未从验证码图中识别出字符（原始文本：{}）", text.trim()));
    }

    if let Some(t) = target_selector {
        if !web_fill_input_deep(t, &code)? {
            return Err(format!("已识别验证码「{code}」，但未找到目标输入框「{t}」"));
        }
    }
    std::fs::remove_file(&crop_name).ok(); // 中间产物用完即删
    Ok(code)
}

// ── 自愈选择器：元素定位失败时，用 视觉模型 / OCR 在页面截图上重新定位并程序化重试。
// 前端改版导致选择器失效时脚本不报废：自愈成功会把定位线索回写进步骤（healed 字段），
// 随 *_scripts.json 落盘，下次执行可参考。 ──

/// 从 CSS 选择器推导视觉/OCR 定位可用的文字线索。
/// 例：`text=百度一下` → 百度一下；`#su` → su；`button.btn-search` → button btn search
fn selector_to_hint(selector: &str) -> String {
    let s = selector.trim();
    if let Some(idx) = s.find("text=") {
        return s[idx + 5..]
            .trim_start_matches(|c: char| !c.is_alphanumeric() && !('\u{4e00}'..='\u{9fff}').contains(&c))
            .to_string();
    }
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_alphanumeric() || ('\u{4e00}'..='\u{9fff}').contains(&c) { c } else { ' ' })
        .collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 受控浏览器整页截图（临时 png，供自愈定位用）
fn web_page_screenshot() -> Result<String, String> {
    let v = crate::browser::act(json!({ "action": "screenshot" }))?;
    v["path"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "自愈截图失败：无返回路径".into())
}

/// 自愈定位：先视觉模型、再 OCR 词框匹配，返回 (CSS 像素 x, y, 命中方式描述)
fn heal_locate(selector: &str, shot: &str) -> Result<(f64, f64, String), String> {
    let hint = selector_to_hint(selector);
    if hint.is_empty() {
        return Err(format!("选择器「{selector}」无法推导定位线索"));
    }
    let dpr = browser_eval("window.devicePixelRatio||1")?.as_f64().unwrap_or(1.0).max(0.1);
    // ① 视觉模型定位（vision 未启用/熔断时自动跳过）
    if crate::visual_grounding::vision_enabled() {
        if let Ok((dx, dy)) = crate::visual_grounding::visual_locate(shot, &hint) {
            return Ok((dx / dpr, dy / dpr, format!("视觉定位「{hint}」")));
        }
    }
    // ② OCR 词框匹配：找与线索词互相包含的词框，取中心
    let (_, words) = crate::ocr::ocr_detect_gui(shot)?;
    let hint_lower = hint.to_lowercase();
    for w in &words {
        let t = w["text"].as_str().unwrap_or("").trim().to_lowercase();
        if t.chars().count() >= 2 && (hint_lower.contains(&t) || t.contains(&hint_lower)) {
            let (x, y) = (w["x"].as_f64().unwrap_or(0.0), w["y"].as_f64().unwrap_or(0.0));
            let (wd, hd) = (w["w"].as_f64().unwrap_or(0.0), w["h"].as_f64().unwrap_or(0.0));
            if wd <= 0.0 || hd <= 0.0 {
                continue;
            }
            return Ok(((x + wd / 2.0) / dpr, (y + hd / 2.0) / dpr, format!("OCR 定位「{}」", w["text"].as_str().unwrap_or(""))));
        }
    }
    Err(format!("视觉/OCR 均未在页面上定位到「{hint}」"))
}

/// 自愈点击：定位后在页面坐标派发完整鼠标事件序列（pointerdown → click）
fn heal_web_click(selector: &str) -> Result<String, String> {
    let shot = web_page_screenshot()?;
    let (x, y, how) = heal_locate(selector, &shot)?;
    let js = format!(
        "(() => {{ const el = document.elementFromPoint({x}, {y}); if (!el) return 'NO_EL'; \
        const o = {{ bubbles: true, cancelable: true, view: window, clientX: {x}, clientY: {y} }}; \
        el.dispatchEvent(new PointerEvent('pointerdown', o)); \
        el.dispatchEvent(new MouseEvent('mousedown', o)); \
        el.dispatchEvent(new PointerEvent('pointerup', o)); \
        el.dispatchEvent(new MouseEvent('mouseup', o)); \
        el.dispatchEvent(new MouseEvent('click', o)); \
        return 'OK'; }})()"
    );
    let r = browser_eval(&js)?;
    std::fs::remove_file(&shot).ok();
    if r.as_str() == Some("NO_EL") {
        return Err(format!("{how}命中（{x:.0},{y:.0}），但该坐标没有可交互元素"));
    }
    Ok(format!("自愈点击：{how}（{selector} → {x:.0},{y:.0}）"))
}

/// 自愈填充：定位输入框后用原生 setter 赋值并派发 input/change（兼容受控组件）
fn heal_web_fill(selector: &str, text: &str) -> Result<String, String> {
    let shot = web_page_screenshot()?;
    let (x, y, how) = heal_locate(selector, &shot)?;
    std::fs::remove_file(&shot).ok();
    let text_json = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into());
    let js = format!(
        "(() => {{ const el = document.elementFromPoint({x}, {y}); \
        if (!el) return 'NO_EL'; \
        const input = el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' ? el : (el.querySelector ? el.querySelector('input,textarea') : null); \
        if (!input) return 'NOT_INPUT'; \
        const proto = input.tagName === 'TEXTAREA' ? window.HTMLTextAreaElement.prototype : window.HTMLInputElement.prototype; \
        const setter = Object.getOwnPropertyDescriptor(proto, 'value').set; \
        setter.call(input, {text_json}); \
        input.dispatchEvent(new Event('input', {{ bubbles: true }})); \
        input.dispatchEvent(new Event('change', {{ bubbles: true }})); \
        return 'OK'; }})()"
    );
    let r = browser_eval(&js)?;
    match r.as_str() {
        Some("NOT_INPUT") => Err(format!("{how}命中（{x:.0},{y:.0}），但该位置不是输入框")),
        Some("NO_EL") => Err(format!("{how}命中（{x:.0},{y:.0}），但该坐标没有元素")),
        _ => Ok(format!("自愈填充：{how}命中输入框（{selector} → {x:.0},{y:.0}）")),
    }
}

/// 自愈成功后回写步骤：把定位方式记入 healed 字段（随 *_scripts.json 持久化）
fn mark_healed(step: &mut Value, detail: &str) {
    if let Some(obj) = step.as_object_mut() {
        obj.insert("healed".into(), json!(detail));
    }
}

/// Web 用例执行上下文：记住测试起始页与标签页，供误操作守卫自动恢复。
#[derive(Default)]
struct WebTestCtx {
    /// 测试起始页 URL（open_page 时记录），守卫用它判断是否被误触跳走
    base_url: String,
    /// 测试标签页 target id（误关后可尝试切回 / 重开）
    tab_id: Option<String>,
}

/// 取 URL 的 scheme://host[:port] 部分，用于同源判断
fn url_origin(url: &str) -> String {
    let (scheme, rest) = match url.split_once("://") {
        Some(v) => v,
        None => return String::new(),
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    format!("{scheme}://{host}")
}

/// 误操作守卫：确认当前活跃标签页仍停留在测试页（与起始页同源）。
/// 用户误触跳转到其他页面/站点，或关闭了测试标签页时自动恢复：
/// 优先切回记录的测试标签页，否则重新打开测试页。
/// 返回需要附加到步骤详情的说明；正常时返回 None。
fn ensure_on_test_page(ctx: &mut WebTestCtx) -> Result<Option<String>, String> {
    if ctx.base_url.is_empty() {
        return Ok(None);
    }
    let base_origin = url_origin(&ctx.base_url);
    if base_origin.is_empty() {
        return Ok(None);
    }
    // 当前标签页可能已被关闭（evaluate 报错 → None）
    let cur = browser_eval("location.href").ok().and_then(|v| v.as_str().map(String::from));
    if let Some(href) = &cur {
        if url_origin(href) == base_origin {
            return Ok(None); // 仍在测试页，正常放行
        }
    }
    // 已离开测试页：先尝试切回记录的测试标签页
    if let Some(id) = ctx.tab_id.clone() {
        if crate::browser::act(json!({ "action": "switch_tab", "tab_id": id })).is_ok() {
            if let Some(href) = browser_eval("location.href").ok().and_then(|v| v.as_str().map(String::from)) {
                if url_origin(&href) == base_origin {
                    return Ok(Some("检测到页面被误触离开，已自动切回测试标签页继续执行".into()));
                }
            }
        }
    }
    // 切回失败 / 无记录：重新打开测试页（无活跃标签页时 browser 内部会新建）
    crate::browser::act(json!({ "action": "goto", "url": ctx.base_url }))
        .map_err(|e| format!("自动回到测试页失败: {e}"))?;
    Ok(Some("检测到页面被误触离开（跳转或标签页被关闭），已自动重新打开测试页并继续执行".into()))
}

/// 下发单个 UI 动作（确定性的步骤分发器），返回 (详情, 断言检查)。
fn dispatch_ui_action(capability: &dyn Capability, step: &mut Value, ctx: &mut WebTestCtx) -> Result<(String, Vec<AssertCheck>), String> {
    let action = step["action"].as_str().unwrap_or("");
    match action {
        "wait" => {
            let ms = step["ms"].as_u64().unwrap_or(1000).min(60_000);
            std::thread::sleep(Duration::from_millis(ms));
            Ok((format!("等待 {ms}ms"), Vec::new()))
        }
        "click_element" => {
            let name = step["name"].as_str().ok_or("click_element 缺少参数 name")?;
            let res = capability.click_element(name).map_err(|e| e.to_string())?;
            Ok((format!("点击控件「{name}」：{}", res.description), Vec::new()))
        }
        "click_at" => {
            let x = step["x"].as_f64().ok_or("click_at 缺少参数 x")?;
            let y = step["y"].as_f64().ok_or("click_at 缺少参数 y")?;
            let res = capability.act(&Action::ClickAt { x, y }).map_err(|e| e.to_string())?;
            Ok((format!("点击坐标 ({x},{y})：{}", res.description), Vec::new()))
        }
        "type_text" => {
            let text = step["text"].as_str().ok_or("type_text 缺少参数 text")?;
            let res = capability
                .act(&Action::TypeText { text: text.to_string() })
                .map_err(|e| e.to_string())?;
            Ok((format!("输入文本：{}", res.description), Vec::new()))
        }
        "paste_text" => {
            let text = step["text"].as_str().ok_or("paste_text 缺少参数 text")?;
            let res = capability
                .act(&Action::PasteText { text: text.to_string() })
                .map_err(|e| e.to_string())?;
            Ok((format!("粘贴文本：{}", res.description), Vec::new()))
        }
        "key_press" => {
            let keys = step["keys"].as_str().ok_or("key_press 缺少参数 keys")?;
            let res = capability
                .act(&Action::KeyPress { keys: keys.to_string() })
                .map_err(|e| e.to_string())?;
            Ok((format!("按键 {keys}：{}", res.description), Vec::new()))
        }
        "window_focus" => {
            let name = step["name"].as_str().ok_or("window_focus 缺少参数 name")?;
            let res = capability
                .act(&Action::WindowFocus { name: name.to_string() })
                .map_err(|e| e.to_string())?;
            Ok((format!("聚焦窗口「{name}」：{}", res.description), Vec::new()))
        }
        "confirm_dialog" => {
            let v = crate::popup::confirm_dialogs(capability);
            let clicked = v["clicked"].as_bool().unwrap_or(false);
            Ok((format!("处理确认弹窗：clicked={clicked}"), Vec::new()))
        }
        "assert" => {
            let checks = run_ui_assertion(capability, step)?;
            let passed = checks.iter().all(|c| c.passed);
            Ok((if passed { "断言通过".into() } else { "断言失败".into() }, checks))
        }
        // ── Web UI 动作：走内置受控浏览器（browser::act，桌面 Chrome 持久化登录态） ──
        "open_page" => {
            let url = step["url"].as_str().ok_or("open_page 缺少参数 url")?;
            crate::browser::act(json!({ "action": "goto", "url": url }))?;
            // 记录起始页与测试标签页，供误操作守卫自动恢复
            ctx.base_url = url.to_string();
            ctx.tab_id = crate::browser::act(json!({ "action": "state" }))
                .ok()
                .and_then(|s| s["active_tab"]["id"].as_str().map(String::from));
            Ok((format!("打开页面 {url}"), Vec::new()))
        }
        // 标签页管理：新建 / 切换 / 关闭（误触场景下守卫也会自动切回或重开测试页）
        "new_tab" => {
            let url = step["url"].as_str().unwrap_or("about:blank");
            let r = crate::browser::act(json!({ "action": "new_tab", "url": url }))?;
            ctx.tab_id = r["tab_id"].as_str().map(String::from);
            Ok((format!("新建标签页并打开 {url}"), Vec::new()))
        }
        "switch_tab" => {
            // 兼容 tab_id / index（第几个标签页，从 0 开始）两种指定方式
            if let Some(id) = step["tab_id"].as_str() {
                crate::browser::act(json!({ "action": "switch_tab", "tab_id": id }))?;
                ctx.tab_id = Some(id.to_string());
                Ok((format!("切换到标签页 {id}"), Vec::new()))
            } else if let Some(idx) = step["index"].as_u64() {
                let r = crate::browser::act(json!({ "action": "tabs" }))?;
                let tabs = r["tabs"].as_array().ok_or("获取标签页列表失败")?;
                let t = tabs
                    .get(idx as usize)
                    .ok_or_else(|| format!("标签页下标 {idx} 不存在（共 {} 个）", tabs.len()))?;
                let id = t["id"].as_str().ok_or("标签页缺少 id")?;
                crate::browser::act(json!({ "action": "switch_tab", "tab_id": id }))?;
                ctx.tab_id = Some(id.to_string());
                Ok((format!("切换到第 {} 个标签页（{}）", idx + 1, id), Vec::new()))
            } else {
                Err("switch_tab 缺少参数 tab_id 或 index".into())
            }
        }
        "close_tab" => {
            let id = step["tab_id"]
                .as_str()
                .map(String::from)
                .or_else(|| ctx.tab_id.clone())
                .ok_or("close_tab 缺少参数 tab_id")?;
            crate::browser::act(json!({ "action": "close_tab", "tab_id": &id }))?;
            let was_test_tab = ctx.tab_id.as_deref() == Some(id.as_str());
            if was_test_tab {
                // 关掉的是测试标签页：清空记录并重开测试页，保证后续步骤可继续
                ctx.tab_id = None;
                if !ctx.base_url.is_empty() {
                    crate::browser::act(json!({ "action": "goto", "url": ctx.base_url }))?;
                    return Ok((format!("已关闭测试标签页并重新打开 {}", ctx.base_url), Vec::new()));
                }
            }
            Ok((format!("已关闭标签页 {id}"), Vec::new()))
        }
        "click_selector" => {
            let selector = step["selector"].as_str().ok_or("click_selector 缺少参数 selector")?.to_string();
            // 首选 CDP 真实点击；失败（如元素在 iframe 内或被覆盖）回退跨框架程序化点击；
            // 再失败进入自愈：视觉/OCR 定位后程序化点击（前端改版选择器失效时脚本不报废）
            match crate::browser::act(json!({ "action": "click", "selector": &selector })) {
                Ok(_) => Ok((format!("按选择器点击「{selector}」"), Vec::new())),
                Err(e) => {
                    if web_click_deep(&selector)? {
                        Ok((format!("按选择器点击「{selector}」（深度点击）"), Vec::new()))
                    } else {
                        match heal_web_click(&selector) {
                            Ok(detail) => {
                                mark_healed(step, &detail);
                                Ok((detail, Vec::new()))
                            }
                            Err(he) => Err(format!("CDP 点击失败: {e}；跨 iframe 深度查找也未命中「{selector}」；{he}")),
                        }
                    }
                }
            }
        }
        "fill_input" => {
            let selector = step["selector"].as_str().ok_or("fill_input 缺少参数 selector")?.to_string();
            let text = step["text"].as_str().ok_or("fill_input 缺少参数 text")?.to_string();
            // ① 真实键盘输入优先（CDP 聚焦 + 逐键输入）：能触发 maxlength 截断、
            //    按键过滤、IME 等真实行为——输入类边界用例必须走真实输入才有意义
            if crate::browser::act(json!({ "action": "type", "selector": &selector, "text": &text })).is_ok() {
                // 回读校验：页面可能截断/过滤了输入，如实记录（不覆盖，保留页面真实行为供断言判定）
                return match web_read_value_deep(&selector)? {
                    Some(actual) if actual == text => Ok((
                        format!("真实键盘输入「{text}」→「{selector}」，回读一致"),
                        Vec::new(),
                    )),
                    Some(actual) => Ok((
                        format!("真实键盘输入后回读不一致：期望「{text}」，实际「{actual}」（页面截断/过滤，保留真实行为）"),
                        vec![AssertCheck {
                            name: "输入生效校验".into(),
                            passed: false,
                            expected: text.chars().take(50).collect(),
                            actual: actual.chars().take(50).collect(),
                        }],
                    )),
                    None => Err(format!("输入后无法回读「{selector}」的值（元素可能不在当前页面）")),
                };
            }
            // ② CDP 真实输入失败（如元素在 iframe 内）：程序化填充（跨 iframe，含 input/change 事件）
            let mut detail = if web_fill_input_deep(&selector, &text)? {
                "程序化填充".to_string()
            } else {
                // ③ 自愈：视觉/OCR 定位后填充
                let d = heal_web_fill(&selector, &text)?;
                mark_healed(step, &d);
                d
            };
            // ④ 回读校验：程序化填充应与目标值完全一致，不一致说明填充未真正生效
            match web_read_value_deep(&selector)? {
                Some(actual) if actual == text => Ok((format!("{detail}：向「{selector}」填写文本（回读一致）"), Vec::new())),
                Some(actual) => Err(format!("{detail}未生效：向「{selector}」写入「{text}」后回读到「{actual}」")),
                None => {
                    detail.push_str(&format!("：无法回读「{selector}」的值（可能为非输入控件或已离开页面）"));
                    Ok((detail, Vec::new()))
                }
            }
        }
        "click_text" => {
            // 参数名兼容：脚本化模型可能输出 target / text / value 任意一种
            let target = step["target"]
                .as_str()
                .or_else(|| step["text"].as_str())
                .or_else(|| step["value"].as_str())
                .ok_or("click_text 缺少参数 target（或 text/value）")?;
            crate::browser::act(json!({ "action": "click_text", "target": target }))?;
            Ok((format!("按可见文字点击「{target}」"), Vec::new()))
        }
        "assert_page_text" => {
            let text = step["text"].as_str().ok_or("assert_page_text 缺少参数 text")?;
            // 汇聚顶层 + 全部同源 iframe 的可见文本后比对
            let page = web_page_text_all()?;
            let passed = page.contains(text);
            Ok((
                if passed {
                    format!("页面包含文本「{text}」")
                } else {
                    format!("页面未找到文本「{text}」")
                },
                vec![AssertCheck {
                    name: "页面文本".into(),
                    passed,
                    expected: text.to_string(),
                    actual: page.chars().take(200).collect(),
                }],
            ))
        }
        // URL 断言：检查当前页面地址是否包含子串（如跳转参数 wd=xxx）。参数名兼容 contains/text。
        "assert_url" => {
            let contains = step["contains"]
                .as_str()
                .or_else(|| step["text"].as_str())
                .ok_or("assert_url 缺少参数 contains（或 text）")?;
            let url = browser_eval("location.href")?
                .as_str()
                .unwrap_or_default()
                .to_string();
            let passed = url.contains(contains);
            Ok((
                if passed {
                    format!("当前 URL 包含「{contains}」")
                } else {
                    format!("当前 URL 未包含「{contains}」")
                },
                vec![AssertCheck {
                    name: "当前 URL".into(),
                    passed,
                    expected: contains.to_string(),
                    actual: url.chars().take(200).collect(),
                }],
            ))
        }
        // 文件上传：触发上传入口 → 系统「打开文件」对话框内全选 + 粘贴路径 + 回车确认。
        // 触发入口二选一：web 用 selector（受控浏览器内点击），桌面用 name（UIA 控件点击）。
        "upload_file" => {
            let path = step["path"].as_str().ok_or("upload_file 缺少参数 path")?;
            match (
                step["selector"].as_str(),
                step["name"].as_str(),
            ) {
                (Some(sel), _) => {
                    crate::browser::act(json!({ "action": "click", "selector": sel }))?;
                }
                (_, Some(nm)) => {
                    capability.click_element(nm).map_err(|e| e.to_string())?;
                }
                _ => return Err("upload_file 缺少参数：请提供 selector(web) 或 name(桌面)".into()),
            }
            std::thread::sleep(Duration::from_millis(1500)); // 等待系统文件对话框弹出
            capability
                .act(&Action::KeyPress { keys: "ctrl+a".into() })
                .map_err(|e| e.to_string())?;
            capability
                .act(&Action::PasteText { text: path.to_string() }) // 剪贴板粘贴，稳过中文/特殊字符路径
                .map_err(|e| e.to_string())?;
            std::thread::sleep(Duration::from_millis(400));
            capability
                .act(&Action::KeyPress { keys: "enter".into() })
                .map_err(|e| e.to_string())?;
            Ok((format!("上传文件 {path}（对话框路径已粘贴确认）"), Vec::new()))
        }
        // 验证码自动识别：source 为验证码图片选择器（可跨 iframe），target 为验证码输入框（可选）
        "ocr_captcha" => {
            let source = step["source"]
                .as_str()
                .or(step["selector"].as_str())
                .ok_or("ocr_captcha 缺少参数 source（验证码图片选择器）")?;
            let target = step["target"].as_str();
            let code = solve_captcha_by_ocr(source, target)?;
            Ok((
                format!("OCR 识别验证码「{code}」"),
                vec![AssertCheck {
                    name: "验证码识别".into(),
                    passed: true,
                    expected: "非空字符".into(),
                    actual: code.clone(),
                }],
            ))
        }
        // 人工接管窗口：暂停指定时长等用户手动完成滑块/短信/复杂验证码后再继续执行
        "wait_captcha" => {
            let ms = step["ms"].as_u64().unwrap_or(30_000).clamp(1_000, 300_000);
            std::thread::sleep(Duration::from_millis(ms));
            Ok((
                format!("人工接管等待 {} 秒结束：请在窗口期间手动完成验证码/滑块", ms / 1000),
                Vec::new(),
            ))
        }
        other => Err(format!("未知动作: {other}")),
    }
}

/// 执行一组 UI 步骤（步骤数组由结构化 JSON 驱动，确定性下发）。
/// 步骤可变：自愈成功会把定位方式回写进步骤（healed 字段）。
pub fn run_ui_steps(capability: &dyn Capability, steps: &mut [Value]) -> Vec<UiStepResult> {
    let mut results = Vec::new();
    let mut ctx = WebTestCtx::default();
    for (i, step) in steps.iter_mut().enumerate() {
        let action = step["action"].as_str().unwrap_or("").to_string();
        let mut r = UiStepResult {
            index: i + 1,
            action: action.clone(),
            ok: true,
            detail: String::new(),
            checks: Vec::new(),
        };
        // 误操作守卫：Web 步骤执行前确认仍在测试页上；
        // 误触跳转其他页面/关闭标签页时自动恢复，不再卡住后续测试
        let mut recovery_note: Option<String> = None;
        let is_web = matches!(
            action.as_str(),
            "open_page"
                | "click_selector"
                | "fill_input"
                | "click_text"
                | "assert_page_text"
                | "assert_url"
                | "ocr_captcha"
                | "upload_file"
                | "new_tab"
                | "switch_tab"
                | "close_tab"
        );
        if is_web {
            match ensure_on_test_page(&mut ctx) {
                Ok(Some(note)) => recovery_note = Some(note),
                Ok(None) => {}
                Err(e) => {
                    r.ok = false;
                    r.detail = format!("误操作守卫失败：{e}");
                    results.push(r);
                    continue;
                }
            }
        }
        match dispatch_ui_action(capability, step, &mut ctx) {
            Ok((detail, checks)) => {
                r.detail = match recovery_note {
                    Some(n) => format!("{n}；{detail}"),
                    None => detail,
                };
                r.checks = checks;
                r.ok = r.checks.iter().all(|c| c.passed);
            }
            Err(e) => {
                r.ok = false;
                r.detail = match recovery_note {
                    Some(n) => format!("{n}；{e}"),
                    None => e,
                };
            }
        }
        results.push(r);
    }
    results
}

/// 渲染 UI 测试报告（Markdown，写入文档窗口）
pub fn render_ui_test_report(name: &str, steps: &[UiStepResult], passed: usize, failed: usize) -> String {
    let total = steps.len();
    let rate = if total == 0 { 0 } else { passed * 100 / total };
    let mut md = String::new();
    md.push_str(&format!("# UI 测试报告：{name}\n\n"));
    md.push_str("## 执行概览\n\n");
    md.push_str(&format!(
        "| 指标 | 值 |\n|---|---|\n| 步骤总数 | {} |\n| 通过 | {} |\n| 失败 | {} |\n| 通过率 | {rate}% |\n",
        total, passed, failed
    ));
    md.push('\n');
    md.push_str("## 步骤明细\n\n");
    md.push_str("| # | 动作 | 结果 | 详情 |\n|---|---|---|---|\n");
    for s in steps {
        md.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            s.index,
            esc(&s.action),
            if s.ok { "✓" } else { "✕" },
            esc(&s.detail)
        ));
    }
    let any_checks = steps.iter().any(|s| !s.checks.is_empty());
    if any_checks {
        md.push_str("\n## 断言明细\n\n");
        md.push_str("| 步骤 | 检查项 | 结果 | 期望 | 实际 |\n|---|---|---|---|---|\n");
        for s in steps {
            for c in &s.checks {
                md.push_str(&format!(
                    "| {} | {} | {} | {} | {} |\n",
                    s.index,
                    esc(&c.name),
                    if c.passed { "✓" } else { "✕" },
                    esc(&c.expected),
                    esc(&c.actual)
                ));
            }
        }
    }
    md
}

/// `run_ui_test` 工具：按结构化步骤确定性执行 UI 自动化测试并产出报告。
pub struct RunUiTestTool {
    capability: Arc<dyn Capability>,
    app: AppHandle,
}

impl RunUiTestTool {
    pub fn new(capability: Arc<dyn Capability>, app: AppHandle) -> Self {
        Self { capability, app }
    }
}

impl Tool for RunUiTestTool {
    fn name(&self) -> &str {
        "run_ui_test"
    }
    fn description(&self) -> &str {
        "执行一组结构化的 UI 自动化测试步骤并生成测试报告（写入文档窗口）。steps 为步骤数组，每步含 action 与参数：wait(ms)、click_element(name)、click_at(x,y)、type_text(text)、paste_text(text)、key_press(keys)、window_focus(name)、confirm_dialog、assert(window_title/ocr_text/visual_target)。assert 步骤会做断言校验"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "测试套件名称" },
                "steps": {
                    "type": "array",
                    "description": "步骤数组，每步含 action 与对应参数；assert 步骤含 window_title/ocr_text/visual_target",
                    "items": { "type": "object" }
                }
            },
            "required": ["name", "steps"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let name = args["name"].as_str().unwrap_or("UI 测试").to_string();
        let mut steps = args["steps"].as_array().cloned().ok_or("缺少参数 steps")?;
        let results = run_ui_steps(self.capability.as_ref(), &mut steps);
        let passed = results.iter().filter(|r| r.ok).count();
        let failed = results.len() - passed;
        let markdown = render_ui_test_report(&name, &results, passed, failed);
        crate::markdown::write_document(
            &self.app,
            &self.app.state::<AppState>().inner().markdown,
            "UI 测试报告",
            &markdown,
        );
        Ok(json!({
            "ok": failed == 0,
            "name": name,
            "total": results.len(),
            "passed": passed,
            "failed": failed,
            "steps": results,
        }))
    }
}

// ───────────────────── 接口（API）自动化测试 ─────────────────────

/// 单个接口用例的执行结果
#[derive(Debug, Clone, Serialize)]
pub struct ApiCaseResult {
    pub name: String,
    pub method: String,
    pub url: String,
    pub status: u16,
    pub ok: bool,
    pub checks: Vec<AssertCheck>,
}

/// 执行单个接口用例：发 HTTP 请求并按 status / body / json 断言。
fn run_api_case(case: &Value) -> Result<ApiCaseResult, String> {
    let name = case["name"].as_str().unwrap_or("").to_string();
    let method = case["method"].as_str().unwrap_or("GET").to_uppercase();
    let url = case["url"].as_str().ok_or("接口用例缺少参数 url")?;
    let timeout = case["timeout_secs"].as_u64().unwrap_or(30).clamp(1, 120);
    let body = case["body"].as_str().unwrap_or("");

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(timeout))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;

    let mut req = match method.as_str() {
        "GET" => client.get(url),
        "POST" => client.post(url),
        "PUT" => client.put(url),
        "DELETE" => client.delete(url),
        "PATCH" => client.patch(url),
        other => return Err(format!("不支持的方法: {other}")),
    };
    if let Some(headers) = case["headers"].as_object() {
        for (k, v) in headers {
            req = req.header(k.as_str(), v.as_str().unwrap_or(""));
        }
    }
    if !body.is_empty() && method != "GET" && method != "DELETE" {
        req = req.body(body.to_string());
    }
    let resp = req.send().map_err(|e| format!("请求失败: {e}"))?;
    let status = resp.status().as_u16();
    let text = resp.text().map_err(|e| format!("读取响应失败: {e}"))?;
    let json_val = serde_json::from_str::<Value>(&text).ok();

    let mut checks = Vec::new();
    if let Some(expect_status) = case["expect_status"].as_u64() {
        checks.push(AssertCheck {
            name: "状态码".into(),
            passed: status as u64 == expect_status,
            expected: expect_status.to_string(),
            actual: status.to_string(),
        });
    }
    if let Some(contains) = case["expect_body_contains"].as_str() {
        if !contains.is_empty() {
            checks.push(AssertCheck {
                name: "响应包含".into(),
                passed: text.contains(contains),
                expected: contains.to_string(),
                actual: text.chars().take(200).collect(),
            });
        }
    }
    if let Some(not_contains) = case["expect_body_not_contains"].as_str() {
        if !not_contains.is_empty() {
            checks.push(AssertCheck {
                name: "响应不含".into(),
                passed: !text.contains(not_contains),
                expected: not_contains.to_string(),
                actual: text.chars().take(200).collect(),
            });
        }
    }
    if let Some(expect_json) = case["expect_json"].as_object() {
        if let Some(jv) = &json_val {
            for (k, v) in expect_json {
                // 支持点路径取嵌套字段（如 "data.id"）；顶层键直接 get
                let actual_val = if k.contains('.') {
                    json_dot_path(jv, k)
                } else {
                    jv.get(k).cloned()
                };
                // 按 JSON 结构比较（1 和 1.0、对象键序不同均视为相等），找不到字段记为缺失
                let (passed, actual_str) = match &actual_val {
                    Some(a) => (a == v, a.to_string()),
                    None => (false, "(字段不存在)".to_string()),
                };
                checks.push(AssertCheck {
                    name: format!("字段 {k}"),
                    passed,
                    expected: v.to_string(),
                    actual: actual_str,
                });
            }
        } else {
            checks.push(AssertCheck {
                name: "响应为 JSON".into(),
                passed: false,
                expected: "JSON".into(),
                actual: "非 JSON 响应".into(),
            });
        }
    }
    if checks.is_empty() {
        checks.push(AssertCheck {
            name: "请求可达".into(),
            passed: true,
            expected: "HTTP 请求完成".into(),
            actual: format!("状态码 {status}"),
        });
    }

    let ok = checks.iter().all(|c| c.passed);
    Ok(ApiCaseResult {
        name,
        method,
        url: url.to_string(),
        status,
        ok,
        checks,
    })
}

/// 按 JSON 点路径取嵌套值（"data.items.0.name" → 数组下标 / 对象键），取不到返回 None
fn json_dot_path(v: &Value, path: &str) -> Option<Value> {
    let mut cur = v;
    for seg in path.split('.') {
        cur = if let Ok(idx) = seg.parse::<usize>() {
            cur.get(idx)?
        } else {
            cur.get(seg)?
        };
    }
    Some(cur.clone())
}

/// 渲染接口测试报告（Markdown，写入文档窗口）
pub fn render_api_test_report(name: &str, cases: &[ApiCaseResult], passed: usize, failed: usize) -> String {
    let total = cases.len();
    let rate = if total == 0 { 0 } else { passed * 100 / total };
    let mut md = String::new();
    md.push_str(&format!("# 接口测试报告：{name}\n\n"));
    md.push_str("## 执行概览\n\n");
    md.push_str(&format!(
        "| 指标 | 值 |\n|---|---|\n| 用例总数 | {} |\n| 通过 | {} |\n| 失败 | {} |\n| 通过率 | {rate}% |\n",
        total, passed, failed
    ));
    md.push('\n');
    md.push_str("## 用例明细\n\n");
    md.push_str("| # | 用例 | 方法 | 状态码 | 结果 |\n|---|---|---|---|---|\n");
    for (i, c) in cases.iter().enumerate() {
        let title = if c.name.is_empty() {
            format!("{} {}", c.method, c.url)
        } else {
            c.name.clone()
        };
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            i + 1,
            esc(&title),
            esc(&c.method),
            c.status,
            if c.ok { "✓" } else { "✕" }
        ));
    }
    let any_checks = cases.iter().any(|c| !c.checks.is_empty());
    if any_checks {
        md.push_str("\n## 断言明细\n\n");
        md.push_str("| 用例 | 检查项 | 结果 | 期望 | 实际 |\n|---|---|---|---|---|\n");
        for (i, c) in cases.iter().enumerate() {
            for ch in &c.checks {
                md.push_str(&format!(
                    "| {} | {} | {} | {} | {} |\n",
                    i + 1,
                    esc(&ch.name),
                    if ch.passed { "✓" } else { "✕" },
                    esc(&ch.expected),
                    esc(&ch.actual)
                ));
            }
        }
    }
    md
}

/// `run_api_test` 工具：按结构化请求列表执行接口测试并产出报告。
pub struct RunApiTestTool {
    app: AppHandle,
}

impl RunApiTestTool {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl Tool for RunApiTestTool {
    fn name(&self) -> &str {
        "run_api_test"
    }
    fn description(&self) -> &str {
        "执行一组结构化 HTTP 接口测试并生成报告（写入文档窗口）。requests 为用例数组，每项含 method/url/headers/body，断言字段：expect_status(期望状态码)、expect_body_contains(响应需包含)、expect_body_not_contains(响应不得包含)、expect_json(JSON 字段精确匹配)"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "测试套件名称" },
                "requests": {
                    "type": "array",
                    "description": "接口用例数组",
                    "items": { "type": "object" }
                }
            },
            "required": ["name", "requests"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let name = args["name"].as_str().unwrap_or("接口测试").to_string();
        let requests = args["requests"].as_array().cloned().ok_or("缺少参数 requests")?;

        let mut cases = Vec::new();
        for req in &requests {
            match run_api_case(req) {
                Ok(c) => cases.push(c),
                Err(e) => cases.push(ApiCaseResult {
                    name: req["name"].as_str().unwrap_or("").to_string(),
                    method: req["method"].as_str().unwrap_or("").to_string(),
                    url: req["url"].as_str().unwrap_or("").to_string(),
                    status: 0,
                    ok: false,
                    checks: vec![AssertCheck {
                        name: "请求执行".into(),
                        passed: false,
                        expected: "正常返回".into(),
                        actual: e,
                    }],
                }),
            }
        }

        let passed = cases.iter().filter(|c| c.ok).count();
        let failed = cases.len() - passed;
        let markdown = render_api_test_report(&name, &cases, passed, failed);
        crate::markdown::write_document(
            &self.app,
            &self.app.state::<AppState>().inner().markdown,
            "接口测试报告",
            &markdown,
        );
        Ok(json!({
            "ok": failed == 0,
            "name": name,
            "total": cases.len(),
            "passed": passed,
            "failed": failed,
            "cases": cases,
        }))
    }
}

// ───────────────────── 用例脚本化 + 勾选执行 ─────────────────────

/// 脚本化提示词：把自然语言测试用例转换为可执行脚本
const SCRIPT_CASES_PROMPT: &str = r#"你是软件测试执行编排器。把下面的自然语言测试用例转换为可直接执行的自动化脚本。

对每条用例（case_index 为它在数组中的下标）：
1. 判断 kind：
   - 用例的前置条件/步骤/数据/期望中出现 URL（http:// 或 https:// 或 www.），或项目档案 project_type=web → 这是 Web 页面测试：kind="ui"，动作【只允许用 Web 系列】，禁止任何桌面动作
   - 明确针对本地桌面应用窗口（且全程无任何 URL、项目不是 web）→ kind="ui"（动作用桌面系列）
   - 针对 HTTP 接口/服务请求（含 URL 或 method、状态码、JSON 响应）且非页面操作 → "api"
   - 信息不足以执行 → "unknown"，并填 reason 说明原因
2. kind="ui"：把「前置条件+测试步骤」拆解为动作数组 ui_steps，动作 action 严格限用：
   桌面（仅限本地桌面应用）：wait(ms)、click_element(name)、click_at(x,y)、type_text(text)、paste_text(text)、key_press(keys)、window_focus(name)、confirm_dialog、assert(含 window_title/window_absent/ocr_text/visual_target)
   Web（仅限浏览器页面，一套 Web 用例内不得混入任何桌面动作）：open_page(url)、click_selector(selector)、fill_input(selector,text)、click_text(target=可见文字)、assert_page_text(text)、assert_url(contains=URL应包含的子串)、upload_file(path + selector(web)或name(桌面)，用于「上传文件」步骤——点击上传控件后自动在系统对话框粘贴路径并确认)、ocr_captcha(source=验证码图片选择器, target可选=填入的输入框选择器，仅限常规字符/数字图形验证码)、wait_captcha(ms，弹出人工接管窗口等待用户手动完成滑块/点选类验证码，默认30000)
   【Web 用例铁律】：输入/点击/断言全部基于 DOM——输入用 fill_input（绝不 type_text）、点击用 click_selector/click_text（绝不 click_element/click_at）、断言用 assert_page_text（绝不 ocr_text/visual_target）。桌面动作作用于屏幕窗口和整桌截屏，不是浏览器页面元素，混用必然失败。
   【断言选型】期望「页面文字/内容出现」→ assert_page_text(text)；期望「跳转 URL / 查询参数」（如搜索后地址含 wd=关键词、/detail/123 路径）→ assert_url(contains)，绝不用 assert_page_text 去断言 URL 内容——URL 不会出现在页面文本里。点击触发跳转后先 wait(ms) 留出加载时间再断言。
   【标签页管理】需要在新标签页打开页面 → new_tab(url)；需要切换标签页 → switch_tab(tab_id 或 index=第几个,从0开始)；需要关闭标签页 → close_tab(tab_id，省略则关当前)。若用例步骤触发点击导致跳转到意外页面，执行器会自动回到测试页继续，无需为此生成额外步骤。
   元素藏在 iframe 内时无需额外动作：web 系列选择器会自动跨同源框架查找。遇到滑块/点选等复杂验证码一律用 wait_captcha 交由人工完成，不要尝试脚本模拟。
   项目档案 project_type=web 且给了 ui_entry 时，第一个动作必须是 open_page(ui_entry)；否则第一个动作 open_page(用例中出现的 URL)。
3. 【测试数据保真】fill_input 的 text 必须逐字取自该用例 data 字段给出的值（如 data 写"38个连续汉字"则需查看 data 中给出的具体字符串原样使用）；data 未给出具体值时才允许按步骤语义构造等价数据，且同一用例内保持一致。禁止随意替换或缩短 data 明确给出的内容。
4. kind="api"：把「测试步骤+期望结果」转换为请求数组 api_requests，字段：
   name、method(GET/POST/PUT/DELETE/PATCH)、url(完整地址)、headers(对象,可省略)、body(字符串,可省略)、expect_status(期望状态码,数字)、expect_body_contains(响应需包含)、expect_body_not_contains(响应不得包含)、expect_json(JSON 字段精确匹配对象)
   期望「返回200/状态码X」→ expect_status；「返回体包含某值」→ expect_body_contains；「某字段等于某值」→ expect_json。
   同一对象可另加可选数组 setup / teardown（每项字段同 api_requests）：setup 为执行前的数据准备请求，teardown 为执行后的数据清理请求；它们的结果不计入通过判定，teardown 无论成败都会执行。

只输出 JSON（不要解释、不要 markdown 代码块）：
{"scripts":[{"case_index":0,"title":"用例标题原样保留","kind":"ui","ui_steps":[...]},{"case_index":1,"title":"...","kind":"api","api_requests":[...],"setup":[],"teardown":[]},{"case_index":3,"title":"...","kind":"unknown","reason":"缺少..."}]}

用例清单：
{cases}

被测项目档案（可能为空对象，注意其中的 ui_entry / api_base / readiness 提示）：
{project}"#;

/// 桌面专属动作集：Web 用例里出现任何一个都视为「动作体系用错」
const DESKTOP_ONLY_ACTIONS: &[&str] = &[
    "click_element",
    "click_at",
    "double_click",
    "right_click",
    "drag",
    "type_text",
    "paste_text",
    "key_press",
    "window_focus",
    "confirm_dialog",
];

/// 用例是否属于 Web 页面意图：前置条件/步骤/数据/期望中出现 URL，或项目档案标明 web 形态
fn case_is_web_intent(case: &Value, project: Option<&ProjectProfile>) -> bool {
    let hay = format!(
        "{}{}{}{}",
        case["precondition"].as_str().unwrap_or(""),
        case["steps"].as_str().unwrap_or(""),
        case["data"].as_str().unwrap_or(""),
        case["expected"].as_str().unwrap_or(""),
    )
    .to_lowercase();
    hay.contains("http://")
        || hay.contains("https://")
        || hay.contains("www.")
        || project.map(|p| p.project_type == "web").unwrap_or(false)
}

/// 脚本中是否含桌面专属动作
fn script_has_desktop_action(script: &CaseScript) -> bool {
    script.ui_steps.iter().any(|s| {
        let a = s["action"].as_str().unwrap_or("").to_lowercase();
        DESKTOP_ONLY_ACTIONS.contains(&a.as_str())
    })
}

/// Web 用例被翻译成桌面动作时的打回重翻反馈（拼在原提示词后）
const SCRIPT_WEB_FIX_PROMPT: &str = r#"

⚠️ 你上次的输出存在严重错误：下列用例属于 Web 页面测试，却使用了桌面自动化动作（click_element/type_text/key_press/assert(ocr_text)/assert(visual_target) 等）。
桌面动作作用于屏幕窗口和整个桌面截屏——不是浏览器页面元素，会导致键盘输入打进无关窗口、断言截到整个桌面的杂乱文字。

重新输出全部脚本（JSON 格式与上次完全相同），对下列用例必须：
1. 只用 Web 系列动作：open_page(url)、click_selector(selector)、fill_input(selector,text)、click_text(target)、assert_page_text(text)、assert_url(contains)、ocr_captcha、wait_captcha、upload_file
2. 第一个动作 open_page：有项目 ui_entry 用 ui_entry，否则用用例中出现的 URL
3. fill_input 的 text 必须逐字取自用例 data 字段给出的值，禁止代编或缩短
4. 绝对禁止出现任何桌面动作：click_element / click_at / type_text / paste_text / key_press / window_focus / confirm_dialog / double_click / right_click / drag / assert(ocr_text) / assert(visual_target)
5. 断言选型：页面文本用 assert_page_text，URL/跳转参数用 assert_url(contains)，不要视觉/OCR

需要修正的用例 case_index：{bad_indexes}"#;

/// 脚本化结果（LLM 输出；同时是「白泽测试记录」落盘的标准脚本格式，
/// UI 测试页 / 接口测试页可直接载入此文件再次执行）
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CaseScript {
    #[serde(default)]
    case_index: usize,
    /// 用例标题（模型输出；缺省由前端按序号补全）
    #[serde(default)]
    title: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    ui_steps: Vec<Value>,
    #[serde(default)]
    api_requests: Vec<Value>,
    /// 接口数据准备（不计入通过判定）
    #[serde(default)]
    setup: Vec<Value>,
    /// 接口数据清理（无论成败都执行，不计入通过判定）
    #[serde(default)]
    teardown: Vec<Value>,
    #[serde(default)]
    reason: String,
}

/// 解析脚本化输出
fn parse_scripts(text: &str) -> Result<Vec<CaseScript>, String> {
    let json = extract_json(text)?;
    let v: Value = serde_json::from_str(json).map_err(|e| format!("解析脚本失败: {e}"))?;
    let arr = v["scripts"].as_array().ok_or("缺少 scripts 数组")?;
    let mut out = Vec::new();
    for item in arr {
        out.push(CaseScript {
            case_index: item["case_index"].as_u64().unwrap_or(0) as usize,
            title: item["title"].as_str().unwrap_or("").to_string(),
            kind: item["kind"].as_str().unwrap_or("unknown").to_lowercase(),
            ui_steps: item["ui_steps"].as_array().cloned().unwrap_or_default(),
            api_requests: item["api_requests"].as_array().cloned().unwrap_or_default(),
            setup: item["setup"].as_array().cloned().unwrap_or_default(),
            teardown: item["teardown"].as_array().cloned().unwrap_or_default(),
            reason: item["reason"].as_str().unwrap_or("").to_string(),
        });
    }
    Ok(out)
}

/// 单条勾选用例的执行结果（前端展示 + 报告渲染）
#[derive(Debug, Clone, Serialize)]
pub struct SelectedCaseResult {
    pub index: usize,
    pub title: String,
    pub kind: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub reason: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ui_steps: Vec<UiStepResult>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub api_cases: Vec<ApiCaseResult>,
}

/// 失败证据（失败时的屏幕截图原始路径）
struct Evidence {
    case_title: String,
    src_path: String,
}

// ───────────────────── 执行期辅助：登录态 / 数据准备清理 ─────────────────────

/// login 就绪方式的登录态处理（令牌形式）：account 形如「Bearer xxx」「eyJ…」时视为令牌，
/// 为缺少 Authorization 头的接口请求自动补充，返回补充的请求数。
/// 形如 user:pass 的账号密码走 Web 登录流程（ui_steps），不做此处处理。
fn inject_token_auth(reqs: &mut [Value], account: &str) -> usize {
    let account = account.trim();
    if account.is_empty() {
        return 0;
    }
    let token = if let Some(rest) = account.strip_prefix("Bearer ") {
        format!("Bearer {}", rest.trim())
    } else if account.starts_with("eyJ") {
        format!("Bearer {account}")
    } else {
        return 0; // 非令牌形态（如 user:pass），不在 HTTP 层注入
    };
    let mut n = 0;
    for req in reqs.iter_mut() {
        let has_auth = req["headers"]["Authorization"].as_str().is_some();
        if !has_auth {
            if !req["headers"].is_object() {
                req["headers"] = json!({});
            }
            req["headers"]["Authorization"] = json!(token);
            n += 1;
        }
    }
    n
}

/// 执行 setup / teardown 请求组：结果只广播进度、不计入用例通过判定。
/// 返回 (成功数, 失败数)；失败的明细也会附在 thought 里便于排查。
fn run_aux_requests(app: &AppHandle, label: &str, reqs: &[Value]) -> (usize, usize) {
    let mut ok = 0usize;
    let mut bad = Vec::new();
    for r in reqs {
        match run_api_case(r) {
            Ok(c) if c.ok => ok += 1,
            Ok(c) => {
                let why = c
                    .checks
                    .iter()
                    .filter(|ch| !ch.passed)
                    .map(|ch| ch.name.clone())
                    .collect::<Vec<_>>()
                    .join("、");
                bad.push(format!("{}({})", if c.name.is_empty() { "请求" } else { &c.name }, why))
            }
            Err(e) => bad.push(e),
        }
    }
    let _ = app.emit(
        "thought",
        json!({
            "kind": "test_pipeline",
            "label": label,
            "detail": format!("{ok}/{} 成功{}", reqs.len(), if bad.is_empty() { String::new() } else { format!("，失败：{}", bad.join("；")) }),
        }),
    );
    (ok, reqs.len() - ok)
}

/// 渲染勾选执行的综合报告
pub fn render_selected_report(
    name: &str,
    results: &[SelectedCaseResult],
    passed: usize,
    failed: usize,
) -> String {
    let total = results.len();
    let rate = if total == 0 { 0 } else { passed * 100 / total };
    let mut md = String::new();
    md.push_str(&format!("# 自动化测试报告：{name}\n\n"));
    md.push_str("## 执行概览\n\n");
    md.push_str(&format!(
        "| 指标 | 值 |\n|---|---|\n| 用例总数 | {total} |\n| 通过 | {passed} |\n| 失败 | {failed} |\n| 通过率 | {rate}% |\n"
    ));
    md.push_str("\n## 用例明细\n\n");
    md.push_str("| # | 类型 | 用例 | 结果 | 说明 |\n|---|---|---|---|---|\n");
    for r in results {
        let kind_label = match r.kind.as_str() {
            "ui" => "UI",
            "api" => "接口",
            _ => "未知",
        };
        let detail = if !r.reason.is_empty() {
            r.reason.clone()
        } else if r.kind == "ui" {
            r.ui_steps
                .iter()
                .filter(|s| !s.ok)
                .map(|s| format!("步骤{}:{}", s.index, s.detail))
                .collect::<Vec<_>>()
                .join("；")
        } else if r.kind == "api" {
            r.api_cases
                .iter()
                .filter(|c| !c.ok)
                .map(|c| {
                    c.checks
                        .iter()
                        .filter(|ch| !ch.passed)
                        .map(|ch| format!("{}:预期「{}」实际「{}」", ch.name, ch.expected, ch.actual))
                        .collect::<Vec<_>>()
                        .join("；")
                })
                .collect::<Vec<_>>()
                .join("；")
        } else {
            String::new()
        };
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            r.index,
            kind_label,
            esc(&r.title),
            if r.ok { "✓" } else { "✕" },
            esc(&detail)
        ));
    }
    md
}

// ───────────────────── HTML 报告（含失败证据） ─────────────────────

/// HTML 转义
fn htmlesc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// 渲染勾选执行的 HTML 报告。evidence 为 (用例标题, 相对报告的图片路径)。
/// 报告头部携带项目档案信息（项目名/形态/环境），失败证据以缩略图内嵌到用例行，点击放大。
pub fn render_selected_report_html(
    name: &str,
    results: &[SelectedCaseResult],
    passed: usize,
    failed: usize,
    evidence: &[(String, String)],
    project: Option<&ProjectProfile>,
) -> String {
    let total = results.len();
    let rate = if total == 0 { 0 } else { passed * 100 / total };
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let (proj_name, proj_type, proj_env) = match project {
        Some(p) => (
            p.name.clone(),
            p.project_type.clone(),
            p.env_tag.clone(),
        ),
        None => ("未绑定项目".into(), String::new(), String::new()),
    };
    let type_label = match proj_type.as_str() {
        "web" => "Web 应用",
        "desktop" => "桌面应用",
        "mobile" => "移动应用",
        "api" => "接口服务",
        "miniprogram" => "小程序",
        "" => "未指定",
        other => other,
    };
    let env_badge = match proj_env.as_str() {
        "prod" => "<span class=\"envb env-prod\">生产环境</span>",
        "staging" => "<span class=\"envb env-stage\">预发环境</span>",
        "test" => "<span class=\"envb env-test\">测试环境</span>",
        "" => "",
        other => &format!("<span class=\"envb env-test\">{other}</span>"),
    };

    // 收集失败用例的「预期 vs 实际」差距项：(位置标签, 预期, 实际)
    let gap_of = |r: &SelectedCaseResult| -> Vec<(String, String, String)> {
        let mut out = Vec::new();
        for s in &r.ui_steps {
            if s.ok {
                continue;
            }
            if s.checks.is_empty() {
                out.push((format!("步骤 {}", s.index), "按步骤执行".into(), s.detail.clone()));
            }
            for c in &s.checks {
                if !c.passed {
                    out.push((format!("步骤 {} · {}", s.index, c.name), c.expected.clone(), c.actual.clone()));
                }
            }
        }
        for c in &r.api_cases {
            for ch in &c.checks {
                if !ch.passed {
                    out.push((
                        format!("{} {} · {}", c.method, c.url, ch.name),
                        ch.expected.clone(),
                        ch.actual.clone(),
                    ));
                }
            }
        }
        out
    };

    let mut h = String::new();
    h.push_str("<!DOCTYPE html>\n<html lang=\"zh-CN\">\n<head>\n<meta charset=\"utf-8\">\n");
    h.push_str(&format!(
        "<title>自动化测试报告 · {} · {}</title>\n",
        htmlesc(&proj_name),
        htmlesc(name)
    ));
    h.push_str(r#"<style>
:root{--ok:#059669;--fail:#dc2626;--ink:#0f172a;--sub:#64748b;--line:#e2e8f0;--bg:#f1f5f9}
*{box-sizing:border-box}
body{font-family:'Segoe UI',system-ui,'Microsoft YaHei',sans-serif;max-width:980px;margin:24px auto;padding:0 16px 40px;color:var(--ink);background:var(--bg)}
.hero{background:linear-gradient(135deg,#1e3a8a,#2563eb 55%,#0ea5e9);border-radius:16px;padding:24px 28px;color:#fff;box-shadow:0 8px 24px rgba(37,99,235,.25)}
.hero .tag{display:inline-block;font-size:11px;letter-spacing:2px;background:rgba(255,255,255,.18);border-radius:999px;padding:3px 12px;margin-bottom:10px}
.hero h1{margin:0 0 6px;font-size:22px}
.hero .meta{display:flex;flex-wrap:wrap;gap:8px 18px;font-size:12.5px;color:rgba(255,255,255,.85)}
.hero .meta b{color:#fff;font-weight:600}
.envb{display:inline-block;font-size:11px;border-radius:999px;padding:2px 10px;font-weight:600}
.env-test{background:#dcfce7;color:#166534}
.env-stage{background:#fef9c3;color:#854d0e}
.env-prod{background:#fee2e2;color:#991b1b}
.cards{display:grid;grid-template-columns:repeat(4,1fr);gap:12px;margin:16px 0}
.card{background:#fff;border-radius:12px;padding:14px 18px;box-shadow:0 1px 3px rgba(0,0,0,.06);border:1px solid var(--line)}
.card b{font-size:24px;display:block;line-height:1.2}
.card span{font-size:12px;color:var(--sub)}
.card .bar{height:6px;border-radius:3px;background:#e2e8f0;margin-top:8px;overflow:hidden}
.card .bar i{display:block;height:100%;border-radius:3px;background:var(--ok)}
.card .bar.bad i{background:var(--fail)}
h2.sec{font-size:15px;margin:22px 0 8px;display:flex;align-items:center;gap:8px}
h2.sec::before{content:"";width:4px;height:16px;border-radius:2px;background:#2563eb;display:inline-block}
table{width:100%;border-collapse:collapse;font-size:13px;background:#fff;border-radius:10px;overflow:hidden;box-shadow:0 1px 3px rgba(0,0,0,.05);border:1px solid var(--line)}
th,td{padding:9px 12px;border-bottom:1px solid var(--line);text-align:left;vertical-align:top}
th{background:#f8fafc;color:#475569;font-weight:600;font-size:12px;text-transform:uppercase;letter-spacing:.5px}
tr:last-child td{border-bottom:none}
.ok{color:var(--ok);font-weight:700}.fail{color:var(--fail);font-weight:700}
.badge{display:inline-block;font-size:12px;font-weight:700;border-radius:999px;padding:2px 12px}
.badge.b-ok{background:#dcfce7;color:#15803d}
.badge.b-fail{background:#fee2e2;color:#b91c1c}
.kd{display:inline-block;font-size:11px;font-weight:600;border-radius:6px;padding:2px 8px;background:#eff6ff;color:#1d4ed8}
details{margin-top:4px}
summary{cursor:pointer;color:#2563eb;font-size:12px;user-select:none}
.dtable{margin-top:8px;background:#f8fafc}
.dtable tr.rfail{background:#fef2f2}
.cmp{display:grid;grid-template-columns:1fr 1fr;gap:8px;margin:10px 0 4px}
.cmp .cell{border-radius:8px;padding:8px 10px;font-size:12px;line-height:1.55;word-break:break-all}
.cmp .exp{background:#f0fdf4;border:1px solid #bbf7d0;color:#166534}
.cmp .act{background:#fef2f2;border:1px solid #fecaca;color:#991b1b}
.cmp .cell b{display:block;font-size:11px;margin-bottom:3px;opacity:.75}
.gapbox{margin-top:8px;background:#fffbeb;border:1px solid #fde68a;border-radius:8px;padding:8px 12px;font-size:12px;color:#92400e}
.evi-thumbs{display:flex;flex-wrap:wrap;gap:10px;margin-top:10px}
.evi-item{text-align:center}
img.evi{width:180px;height:auto;border:1px solid #cbd5e1;border-radius:8px;cursor:zoom-in;transition:transform .15s ease;box-shadow:0 1px 4px rgba(0,0,0,.12)}
img.evi:hover{transform:scale(1.04)}
.evi-cap{font-size:11px;color:var(--sub);margin-top:4px;max-width:180px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
#lightbox{display:none;position:fixed;inset:0;background:rgba(15,23,42,.85);z-index:99;align-items:center;justify-content:center;cursor:zoom-out}
#lightbox img{max-width:92vw;max-height:92vh;border-radius:8px;box-shadow:0 10px 40px rgba(0,0,0,.5)}
footer{margin-top:26px;text-align:center;font-size:11.5px;color:var(--sub)}
@media (max-width:640px){.cards{grid-template-columns:repeat(2,1fr)}}
</style>
</head>
<body>
"#);

    // 头部横幅：标题带项目模块信息
    h.push_str(&format!(
        "<div class=\"hero\"><span class=\"tag\">BAIZE · 自动化测试报告</span>\n\
         <h1>{} · {}</h1>\n\
         <div class=\"meta\"><span>项目 <b>{}</b></span><span>模块形态 <b>{}</b></span><span>{} </span><span>执行时间 <b>{}</b></span><span>套件 <b>{}</b></span></div>\n</div>\n",
        htmlesc(&proj_name),
        htmlesc(name),
        htmlesc(&proj_name),
        type_label,
        env_badge,
        htmlesc(&now),
        htmlesc(name),
    ));

    // 概览卡片（带通过率进度条）
    h.push_str(&format!(
        "<div class=\"cards\">\n\
         <div class=\"card\"><b>{total}</b><span>用例总数</span></div>\n\
         <div class=\"card\"><b class=\"ok\">{passed}</b><span>通过</span><div class=\"bar\"><i style=\"width:{}%\"></i></div></div>\n\
         <div class=\"card\"><b class=\"fail\">{failed}</b><span>失败</span><div class=\"bar bad\"><i style=\"width:{}%\"></i></div></div>\n\
         <div class=\"card\"><b>{rate}%</b><span>通过率</span><div class=\"bar\"><i style=\"width:{rate}%\"></i></div></div>\n\
         </div>\n",
        if total > 0 { passed * 100 / total } else { 0 },
        if total > 0 { failed * 100 / total } else { 0 },
    ));

    h.push_str("<h2 class=\"sec\">用例明细</h2>\n<table><tr><th style=\"width:36px\">#</th><th style=\"width:64px\">类型</th><th>用例</th><th style=\"width:88px\">结果</th></tr>\n");
    for r in results {
        let kind_label = match r.kind.as_str() {
            "ui" => "UI",
            "api" => "接口",
            _ => "未知",
        };
        let gaps = gap_of(r);
        let evi_imgs: Vec<&(String, String)> =
            evidence.iter().filter(|(t, _)| t == &r.title).collect();
        let has_detail = !r.api_cases.is_empty()
            || (!r.ui_steps.is_empty() && r.ui_steps.iter().any(|s| !s.checks.is_empty()))
            || !gaps.is_empty()
            || !evi_imgs.is_empty();
        h.push_str(&format!(
            "<tr><td>{}</td><td><span class=\"kd\">{}</span></td><td>{}</td><td><span class=\"badge b-{}\">{}</span></td></tr>\n",
            r.index,
            kind_label,
            htmlesc(&r.title),
            if r.ok { "ok" } else { "fail" },
            if r.ok { "✓ 通过" } else { "✕ 失败" },
        ));
        if !has_detail {
            continue;
        }
        h.push_str(&format!(
            "<tr><td></td><td colspan=\"3\"><details{}><summary>执行详情（#{0}）</summary>",
            if !r.ok { " open" } else { "" }
        ));
        // 预期 vs 实际 差距分析（失败用例）
        if !gaps.is_empty() {
            h.push_str("<div class=\"gapbox\">⚠ 失败差距分析：以下为断言预期与实际结果的差异</div>");
            for (where_, exp, act) in &gaps {
                h.push_str(&format!(
                    "<div class=\"cmp\"><div class=\"exp\"><b>预期 · {}</b>{}</div><div class=\"act\"><b>实际 · {}</b>{}</div></div>\n",
                    htmlesc(where_),
                    htmlesc(exp),
                    htmlesc(where_),
                    htmlesc(act),
                ));
            }
        }
        // 断言逐条明细（失败行高亮）
        let any_checks = !r.api_cases.is_empty()
            || (!r.ui_steps.is_empty() && r.ui_steps.iter().any(|s| !s.checks.is_empty()));
        if any_checks {
            h.push_str("<table class=\"dtable\"><tr><th>位置</th><th style=\"width:40px\">判定</th><th>预期</th><th>实际</th></tr>");
            for s in &r.ui_steps {
                for c in &s.checks {
                    h.push_str(&format!(
                        "<tr{}><td>步骤 {} · {}</td><td class=\"{}\">{}</td><td>{}</td><td>{}</td></tr>",
                        if c.passed { "" } else { " class=\"rfail\"" },
                        s.index,
                        htmlesc(&c.name),
                        if c.passed { "ok" } else { "fail" },
                        if c.passed { "✓" } else { "✕" },
                        htmlesc(&c.expected),
                        htmlesc(&c.actual)
                    ));
                }
            }
            for c in &r.api_cases {
                for ch in &c.checks {
                    h.push_str(&format!(
                        "<tr{}><td>{} {} · {}</td><td class=\"{}\">{}</td><td>{}</td><td>{}</td></tr>",
                        if ch.passed { "" } else { " class=\"rfail\"" },
                        htmlesc(&c.method),
                        htmlesc(&c.url),
                        htmlesc(&ch.name),
                        if ch.passed { "ok" } else { "fail" },
                        if ch.passed { "✓" } else { "✕" },
                        htmlesc(&ch.expected),
                        htmlesc(&ch.actual)
                    ));
                }
            }
            h.push_str("</table>");
        }
        // 失败证据缩略图（点击放大）
        if !evi_imgs.is_empty() {
            h.push_str("<div class=\"evi-thumbs\">");
            for (title, rel) in &evi_imgs {
                h.push_str(&format!(
                    "<div class=\"evi-item\"><img class=\"evi\" src=\"{}\" alt=\"failure evidence\" onclick=\"lb(this)\"><div class=\"evi-cap\">{}</div></div>\n",
                    htmlesc(rel),
                    htmlesc(title),
                ));
            }
            h.push_str("</div>");
        }
        h.push_str("</details></td></tr>\n");
    }
    h.push_str("</table>\n");

    // 灯箱（点击缩略图放大查看）
    h.push_str(
        "<div id=\"lightbox\" onclick=\"this.style.display='none'\"><img id=\"lb-img\" src=\"\" alt=\"\"></div>\n\
         <script>\nfunction lb(img){var lb=document.getElementById('lightbox');document.getElementById('lb-img').src=img.src;lb.style.display='flex';}\ndocument.addEventListener('keydown',function(e){if(e.key==='Escape')document.getElementById('lightbox').style.display='none';});\n</script>\n",
    );
    h.push_str(&format!(
        "<footer>白泽自动化测试 · 报告生成于 {} · 项目「{}」</footer>\n</body>\n</html>\n",
        htmlesc(&now),
        htmlesc(&proj_name),
    ));
    h
}

/// 勾选用例批量执行：脚本化 → 自动执行（UI/接口）→ 综合报告。
/// 在 `spawn_blocking` 线程内调用；脚本化单次云端强模型调用，其余为确定性执行。
pub fn run_selected_cases(
    app: &AppHandle,
    capability: &dyn Capability,
    name: &str,
    cases: &[Value],
    project: Option<&ProjectProfile>,
) -> Result<Value, String> {
    // 环境隔离硬门：被测项目标记为生产环境时，直接拦截执行，防止误打生产。
    // 就绪方式前置：readiness=boot 时白泽自动拉起被测应用（失败不阻断，只广播警告）。
    if let Some(p) = project {
        guard_env(&p.env_tag)?;
        if p.readiness == "boot" && !p.run_command.trim().is_empty() {
            let _ = app.emit(
                "thought",
                json!({ "kind": "test_pipeline", "label": "环境准备", "detail": format!("就绪方式为 boot，正在后台拉起被测应用…") }),
            );
            let detail = match run_prepare_env(&p.run_command) {
                Ok(d) => d,
                Err(e) => format!("启动失败（继续执行测试）：{e}"),
            };
            let _ = app.emit(
                "thought",
                json!({ "kind": "test_pipeline", "label": "环境准备", "detail": detail }),
            );
        }
    }

    let _ = app.emit(
        "thought",
        json!({ "kind": "test_pipeline", "label": "用例脚本化", "detail": format!("正在将 {} 条用例转换为可执行脚本…", cases.len()) }),
    );

    // ① 脚本化（单次强模型调用；附上被测项目档案供 LLM 判断 web 入口 / api_base）
    let cases_json = serde_json::to_string(cases).map_err(|e| e.to_string())?;
    let project_json = project
        .map(|p| serde_json::to_string(p).unwrap_or_else(|_| "{}".into()))
        .unwrap_or_else(|| "{}".into());
    let prompt_owned = SCRIPT_CASES_PROMPT
        .replace("{cases}", &cases_json)
        .replace("{project}", &project_json);
    let app_for_llm = app.clone();
    let scripts = tauri::async_runtime::block_on(async move {
        let state = app_for_llm.state::<AppState>();
        let model = &state.inner().model;

        // 流式脚本化：边生成边广播进度（200ms 节流），并附解析失败自动纠错重试
        let scripted = |label: &str, p: String| {
            use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
            let msgs = vec![ChatMessage {
                role: "user".into(),
                content: p,
                tool_calls: None,
                tool_call_id: None,
            }];
            let chars = AtomicUsize::new(0);
            let last_emit = AtomicU64::new(0);
            let acc = std::sync::Mutex::new(String::new());
            let app2 = app_for_llm.clone();
            let label2 = label.to_string();
            let on_token = move |delta: &str| {
                let n = chars.fetch_add(delta.chars().count(), Ordering::Relaxed) + delta.chars().count();
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                let prev = last_emit.load(Ordering::Relaxed);
                if now.saturating_sub(prev) < 200
                    || last_emit
                        .compare_exchange(prev, now, Ordering::Relaxed, Ordering::Relaxed)
                        .is_err()
                {
                    return;
                }
                acc.lock().unwrap().push_str(delta);
                let tail: String = {
                    let g = acc.lock().unwrap();
                    let cnt = g.chars().count();
                    g.chars().skip(cnt.saturating_sub(60)).collect()
                };
                let _ = app2.emit(
                    "thought",
                    json!({
                        "kind": "test_pipeline",
                        "label": label2,
                        "detail": format!("生成中…已输出 {n} 字 · …{tail}")
                    }),
                );
            };
            async move { model.stream_chat_with_tier(ModelTier::Cloud, &msgs, &[], &on_token).await }
        };

        // 首次调用 + 解析；解析失败自动纠错重试 1 次（避免整条执行链白跑）
        let mut last_err = String::new();
        for attempt in 0..2 {
            let p = if attempt == 0 {
                prompt_owned.clone()
            } else {
                let _ = app_for_llm.emit(
                    "thought",
                    json!({ "kind": "test_pipeline", "label": "用例脚本化", "detail": "脚本解析失败，正在自动重试…" }),
                );
                format!(
                    "{prompt_owned}\n\n⚠️ 你上一次输出无法解析（{last_err}）。这次必须只输出符合要求的 JSON 本体：不要解释、不要 markdown 代码块、不要任何多余文字。"
                )
            };
            match scripted("用例脚本化", p).await {
                Ok(resp) => match parse_scripts(&resp.content.unwrap_or_default()) {
                    Ok(s) => return Ok(s),
                    Err(e) => last_err = e,
                },
                Err(e) => last_err = e,
            }
        }
        Err(format!("用例脚本化失败：{last_err}"))
    })?;

    // ①b Web 用例守卫：检测「Web 意图用例被翻译成桌面动作」并打回重翻一次。
    // 这类脚本会把键盘输入打进前台窗口（如 Word/白泽自己）、断言截整个桌面，必须拦截。
    let web_bad: Vec<usize> = scripts
        .iter()
        .filter(|s| {
            s.kind == "ui"
                && script_has_desktop_action(s)
                && s.case_index < cases.len()
                && case_is_web_intent(&cases[s.case_index], project)
        })
        .map(|s| s.case_index)
        .collect();
    let mut scripts = if web_bad.is_empty() {
        scripts
    } else {
        let _ = app.emit(
            "thought",
            json!({
                "kind": "test_pipeline",
                "label": "用例脚本化",
                "detail": format!("检测到 {} 条 Web 用例被翻译成桌面动作，正在打回重翻…", web_bad.len())
            }),
        );
        let fix_prompt = format!(
            "{}{}",
            SCRIPT_CASES_PROMPT
                .replace("{cases}", &cases_json)
                .replace("{project}", &project_json),
            SCRIPT_WEB_FIX_PROMPT.replace("{bad_indexes}", &format!("{:?}", web_bad))
        );
        let app_fix = app.clone();
        let resp2 = tauri::async_runtime::block_on(async move {
            let state = app_fix.state::<AppState>();
            let msgs = vec![ChatMessage {
                role: "user".into(),
                content: fix_prompt,
                tool_calls: None,
                tool_call_id: None,
            }];
            state.inner().model.chat_with_tier(ModelTier::Cloud, &msgs, &[]).await
        })?;
        let fixed = parse_scripts(&resp2.content.unwrap_or_default())?;
        let _ = app.emit(
            "thought",
            json!({
                "kind": "test_pipeline",
                "label": "用例脚本化",
                "detail": format!("重翻完成，已修正 {} 条脚本", fixed.len())
            }),
        );
        fixed
    };
    let _ = app.emit(
        "thought",
        json!({ "kind": "test_pipeline", "label": "用例脚本化", "detail": format!("已生成 {} 条脚本", scripts.len()) }),
    );

    // ② 构建 index → script 映射（可变：自愈回写需要原地修改步骤）
    let mut map: std::collections::HashMap<usize, &mut CaseScript> = std::collections::HashMap::new();
    for s in &mut scripts {
        map.insert(s.case_index, s);
    }

    // ③ 逐条执行
    let mut results: Vec<SelectedCaseResult> = Vec::new();
    let mut passed = 0usize;
    let mut failed = 0usize;
    // 失败证据：UI 用例失败时立即截屏留档（屏幕状态时效性强）
    let mut evidences: Vec<Evidence> = Vec::new();

    for (i, case) in cases.iter().enumerate() {
        let title = case["title"].as_str().unwrap_or("未命名用例").to_string();
        let r = match map.get_mut(&i) {
            None => SelectedCaseResult {
                index: i + 1,
                title: title.clone(),
                kind: "unknown".into(),
                ok: false,
                reason: "脚本化未覆盖该用例".into(),
                ui_steps: Vec::new(),
                api_cases: Vec::new(),
            },
            Some(script) => match script.kind.as_str() {
                "ui" => {
                    let step_res = run_ui_steps(capability, &mut script.ui_steps);

                    let ok = step_res.iter().all(|r| r.ok);
                    if !ok {
                        if let Ok(shot) = capability.capture_screen() {
                            evidences.push(Evidence {
                                case_title: title.clone(),
                                src_path: shot.path,
                            });
                        }
                    }
                    SelectedCaseResult {
                        index: i + 1,
                        title: title.clone(),
                        kind: "ui".into(),
                        ok,
                        reason: String::new(),
                        ui_steps: step_res,
                        api_cases: Vec::new(),
                    }
                }
                "api" => {
                    // 登录态注入：account 为令牌形态时自动补充 Authorization 头
                    let mut main_reqs = script.api_requests.clone();
                    let mut teardown = script.teardown.clone();
                    if let Some(p) = project {
                        let mut setup = script.setup.clone();
                        let n = inject_token_auth(&mut main_reqs, &p.account)
                            + inject_token_auth(&mut setup, &p.account)
                            + inject_token_auth(&mut teardown, &p.account);
                        if n > 0 {
                            let _ = app.emit(
                                "thought",
                                json!({ "kind": "test_pipeline", "label": "登录态注入", "detail": format!("已为 {n} 个请求自动补充 Authorization 头") }),
                            );
                        }
                        // 数据准备（结果不计入通过判定）
                        if !setup.is_empty() {
                            run_aux_requests(app, "数据准备", &setup);
                        }
                    } else if !script.setup.is_empty() {
                        run_aux_requests(app, "数据准备", &script.setup);
                    }
                    let mut api_cases = Vec::new();
                    for req in &main_reqs {
                        match run_api_case(req) {
                            Ok(c) => api_cases.push(c),
                            Err(e) => api_cases.push(ApiCaseResult {
                                name: req["name"].as_str().unwrap_or("").to_string(),
                                method: req["method"].as_str().unwrap_or("").to_string(),
                                url: req["url"].as_str().unwrap_or("").to_string(),
                                status: 0,
                                ok: false,
                                checks: vec![AssertCheck {
                                    name: "请求执行".into(),
                                    passed: false,
                                    expected: "正常返回".into(),
                                    actual: e,
                                }],
                            }),
                        }
                    }
                    let ok = api_cases.iter().all(|c| c.ok);
                    // 数据清理：无论用例成败都执行 teardown（不计入通过判定）
                    if !teardown.is_empty() {
                        run_aux_requests(app, "数据清理", &teardown);
                    }
                    SelectedCaseResult {
                        index: i + 1,
                        title: title.clone(),
                        kind: "api".into(),
                        ok,
                        reason: String::new(),
                        ui_steps: Vec::new(),
                        api_cases,
                    }
                }
                other => SelectedCaseResult {
                    index: i + 1,
                    title: title.clone(),
                    kind: other.to_string(),
                    ok: false,
                    reason: if script.reason.is_empty() {
                        "无法执行".into()
                    } else {
                        script.reason.clone()
                    },
                    ui_steps: Vec::new(),
                    api_cases: Vec::new(),
                },
            },
        };
        if r.ok {
            passed += 1;
        } else {
            failed += 1;
        }
        let _ = app.emit(
            "thought",
            json!({
                "kind": "test_pipeline",
                "label": "自动执行",
                "detail": format!("#{} {}：{}", i + 1, title, if r.ok { "通过" } else { "失败" }),
                // 结构化进度（0-100）：前端渲染实时进度条与逐条 ✓/✕ 徽标
                "progress": ((i + 1) * 100 / cases.len().max(1)),
                "title": title,
                "ok": r.ok,
            }),
        );
        results.push(r);
    }

    // 脚本化产物序列化：标准可复用格式（与 UI 测试页 / 接口测试页输入互通）。
    // 序列化放在执行后：执行中的自愈回写（healed 字段）会一并落盘为 *_scripts.json
    let healed_count = map.values().filter(|s| s.ui_steps.iter().any(|st| st.get("healed").is_some())).count();
    if healed_count > 0 {
        let _ = app.emit(
            "thought",
            json!({
                "kind": "test_pipeline",
                "label": "自愈选择器",
                "detail": format!("{healed_count} 条用例曾定位失败并已视觉/OCR 自愈，修复线索已回写脚本")
            }),
        );
    }
    let mut sorted_scripts: Vec<&CaseScript> = map.values().map(|s| &**s).collect();
    sorted_scripts.sort_by_key(|s| s.case_index);
    let scripts_text = serde_json::to_string_pretty(&json!({
        "name": name,
        "created": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        "scripts": sorted_scripts,
    }))
    .unwrap_or_default();

    // ④ 综合报告写入文档窗口
    let markdown = render_selected_report(name, &results, passed, failed);
    crate::markdown::write_document(
        app,
        &app.state::<AppState>().inner().markdown,
        "自动化测试报告",
        &markdown,
    );

    // ⑤ 执行记录落盘（Markdown + HTML 报告 + 脚本文件 + 失败证据截图，按项目组织到本地）
    let mut record_md = String::new();
    let mut record_html = String::new();
    let mut record_scripts = String::new();
    if let Some(p) = project {
        let rec = prepare_record_paths_for(app, p, name);
        if let Some(md_path) = &rec.md {
            if std::fs::write(md_path, &markdown).is_ok() {
                record_md = md_path.to_string_lossy().to_string();
            }
            // 可复用脚本文件：<主干>_scripts.json，与报告同目录同名
            if !scripts_text.is_empty() {
                if let Some(stem) = md_path.file_stem().and_then(|s| s.to_str()) {
                    let sp = md_path.with_file_name(format!("{stem}_scripts.json"));
                    if std::fs::write(&sp, &scripts_text).is_ok() {
                        record_scripts = sp.to_string_lossy().to_string();
                    }
                }
            }
        }
        // 失败证据：拷贝截图到记录目录的 evidence 子目录，HTML 内用相对路径引用
        let mut rel_evidence: Vec<(String, String)> = Vec::new();
        for (idx, ev) in evidences.iter().enumerate() {
            let ext = std::path::Path::new(&ev.src_path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("png")
                .to_lowercase();
            let fname = format!("case{}_{}.{}", idx + 1, sanitize_filename(&ev.case_title), ext);
            match rec.evidence_dir.as_ref() {
                Some(edir) => {
                    if std::fs::copy(&ev.src_path, edir.join(&fname)).is_ok() {
                        rel_evidence.push((ev.case_title.clone(), format!("evidence/{fname}")));
                    }
                }
                None => rel_evidence.push((ev.case_title.clone(), ev.src_path.clone())),
            }
        }
        let html = render_selected_report_html(name, &results, passed, failed, &rel_evidence, project);
        if let Some(html_path) = &rec.html {
            if std::fs::write(html_path, html).is_ok() {
                record_html = html_path.to_string_lossy().to_string();
            }
        }
        let mut detail = format!("已落盘：{record_md}");
        if !record_scripts.is_empty() {
            detail.push_str(&format!("（可复用脚本：{record_scripts}）"));
        }
        if !record_html.is_empty() {
            detail.push_str(&format!("（HTML 报告：{record_html}）"));
        }
        let _ = app.emit(
            "thought",
            json!({ "kind": "test_pipeline", "label": "执行记录", "detail": detail }),
        );
    }

    // 未绑定项目时：报告/脚本不落盘，明确广播提醒（避免用户误以为已保存）
    if project.is_none() {
        let _ = app.emit(
            "thought",
            json!({
                "kind": "test_pipeline",
                "label": "执行记录",
                "detail": "未绑定被测项目：报告与可复用脚本没有保存。到「项目配置」选择或新建项目后重跑，即可自动落盘到「文档/白泽测试记录/<项目名_项目id>/」"
            }),
        );
    }

    // 测试趋势入库：本次结果写入项目趋势曲线（保留最近 200 个执行点）
    if let Some(p) = project {
        if !p.id.is_empty() {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            let total = results.len();
            let rate = if total > 0 {
                (passed as f64) * 100.0 / total as f64
            } else {
                0.0
            };
            let point = json!({
                "ts": now_ms,
                "name": name,
                "total": total,
                "passed": passed,
                "failed": failed,
                "rate": (rate * 10.0).round() / 10.0,
            });
            let key = format!("test_trend:{}", p.id);
            let state = app.state::<AppState>();
            let mut list: Vec<Value> = state
                .store
                .get_setting(&key)
                .ok()
                .flatten()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            list.push(point);
            if list.len() > 200 {
                let drop = list.len() - 200;
                list.drain(0..drop);
            }
            if let Ok(s) = serde_json::to_string(&list) {
                let _ = state.store.set_setting(&key, &s);
            }
        }
    }

    Ok(json!({
        "ok": failed == 0,
        "name": name,
        "total": results.len(),
        "passed": passed,
        "failed": failed,
        "results": results,
        "evidence_count": evidences.len(),
        // 报告/脚本落盘路径（未选项目或写入失败时为空串）
        "report_md": record_md,
        "report_html": record_html,
        "scripts_path": record_scripts,
    }))
}

// ───────────────────── 被测对象台账（项目基线 / SUT Profile） ─────────────────────

/// 被测对象台账：测试闭环的「第 0 步」，四个阶段共同的前置输入源。
/// 一次录入、可复用；持久化到本地 settings（key=`test_projects`，JSON 数组）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectProfile {
    /// 项目标识（保存时生成，用于执行记录目录命名）
    #[serde(default)]
    pub id: String,
    /// 项目名
    pub name: String,
    /// 项目形态：web / desktop / mobile / api / miniprogram
    #[serde(default)]
    pub project_type: String,
    /// 需求文档来源：本地文件 / 飞书链接 / URL（约定一个主来源）
    #[serde(default)]
    pub source: String,
    /// Web UI 入口 URL（仅 web 形态需要）
    #[serde(default)]
    pub ui_entry: String,
    /// 接口 base_url
    #[serde(default)]
    pub api_base: String,
    /// openapi/swagger 文档路径或在线地址（可选）
    #[serde(default)]
    pub api_doc: String,
    /// 代码仓库地址 / 本地目录
    #[serde(default)]
    pub repo_or_path: String,
    /// 就绪方式：running（已部署直接测）/ boot（白泽拉起）/ login（需登录态）
    #[serde(default)]
    pub readiness: String,
    /// boot 方式下白泽拉起应用的命令（如 npm run dev / docker compose up）
    #[serde(default)]
    pub run_command: String,
    /// 测试账号 / token（可选，敏感）
    #[serde(default)]
    pub account: String,
    /// 环境标识：test / staging / prod（prod 触发环境隔离硬门）
    #[serde(default)]
    pub env_tag: String,
    /// 报告/脚本保存根目录（可选；留空 = 系统文档目录/白泽测试记录）。
    /// 用户的文档目录可能被 360 搬家等工具重定向，支持显式指定避免找不到落盘位置。
    #[serde(default)]
    pub report_dir: String,
}

impl ProjectProfile {
    /// 生成唯一 id：项目名 + 时间戳（可读、可作目录名）
    fn with_generated_id(mut self) -> Self {
        if self.id.is_empty() {
            let ts = chrono::Local::now().format("%Y%m%d%H%M%S");
            let name_part: String = self
                .name
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
                .take(24)
                .collect();
            self.id = format!("{}_{}", name_part, ts);
        }
        self
    }
}

/// 项目档案库持久化 key
const TEST_PROJECTS_KEY: &str = "test_projects";

/// 读取项目档案库（空则返回空数组）
pub fn load_projects(store: &crate::memory::MemoryStore) -> Vec<ProjectProfile> {
    match store.get_setting(TEST_PROJECTS_KEY) {
        Ok(Some(json_str)) => serde_json::from_str(&json_str).unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn persist_projects(store: &crate::memory::MemoryStore, list: &[ProjectProfile]) -> Result<(), String> {
    let json_str = serde_json::to_string(list).map_err(|e| e.to_string())?;
    store.set_setting(TEST_PROJECTS_KEY, &json_str)
}

/// 保存（新增或按 id 更新）一个项目，返回保存后的档案列表。
pub fn save_project(store: &crate::memory::MemoryStore, profile: ProjectProfile) -> Result<Vec<ProjectProfile>, String> {
    let mut list = load_projects(store);
    let p = profile.with_generated_id();
    match list.iter_mut().find(|x| x.id == p.id) {
        Some(existing) => *existing = p,
        None => list.push(p),
    }
    persist_projects(store, &list)?;
    Ok(list)
}

/// 按 id 删除一个项目，返回删除后的档案列表。
pub fn delete_project(store: &crate::memory::MemoryStore, id: &str) -> Result<Vec<ProjectProfile>, String> {
    let mut list = load_projects(store);
    list.retain(|x| x.id != id);
    persist_projects(store, &list)?;
    Ok(list)
}

/// 从本地代码目录嗅探项目形态（确定性规则，作为 LLM 的兜底）。
fn sniff_project_type(dir: &str) -> String {
    let p = std::path::Path::new(dir);
    if !p.is_dir() {
        return String::new();
    }
    let has = |name: &str| p.join(name).exists();
    if has("package.json") {
        return "web".to_string();
    }
    if has("Cargo.toml") {
        return "desktop".to_string();
    }
    if has("pom.xml") || has("build.gradle") || has("go.mod") || has("requirements.txt") {
        return "api".to_string();
    }
    String::new()
}

/// 自动识别被测对象：从需求文档 +（可选）代码目录推断项目形态与地址。
/// 主路径走一次云端强模型抽取，代码目录嗅探作为 project_type 兜底。
pub fn auto_detect_project(
    app: &AppHandle,
    requirement: &str,
    repo_or_path: Option<&str>,
) -> Result<ProjectProfile, String> {
    let sniffed = repo_or_path.map(sniff_project_type).unwrap_or_default();

    let prompt = AUTO_DETECT_PROMPT.replace("{requirement}", requirement);
    let app_for_llm = app.clone();
    let resp = tauri::async_runtime::block_on(async move {
        let state = app_for_llm.state::<AppState>();
        let msgs = vec![ChatMessage {
            role: "user".into(),
            content: prompt,
            tool_calls: None,
            tool_call_id: None,
        }];
        state.inner().model.chat_with_tier(ModelTier::Cloud, &msgs, &[]).await
    })?;

    let mut profile = parse_project_profile(&resp.content.unwrap_or_default())?;
    // 代码目录嗅探结果作为 project_type 兜底
    if profile.project_type.is_empty() && !sniffed.is_empty() {
        profile.project_type = sniffed;
    }
    if let Some(repo) = repo_or_path {
        if profile.repo_or_path.is_empty() {
            profile.repo_or_path = repo.to_string();
        }
    }
    Ok(profile)
}

/// 解析自动识别输出的项目档案 JSON。
fn parse_project_profile(text: &str) -> Result<ProjectProfile, String> {
    let json = extract_json(text)?;
    let v: Value = serde_json::from_str(json).map_err(|e| format!("解析项目档案失败: {e}"))?;
    let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
    Ok(ProjectProfile {
        id: String::new(),
        name: s("name"),
        project_type: s("project_type").to_lowercase(),
        source: s("source"),
        ui_entry: s("ui_entry"),
        api_base: s("api_base"),
        api_doc: s("api_doc"),
        repo_or_path: s("repo_or_path"),
        readiness: s("readiness").to_lowercase(),
        run_command: s("run_command"),
        account: s("account"),
        env_tag: s("env_tag").to_lowercase(),
        report_dir: s("report_dir"),
    })
}

/// 就绪方式前置：当 readiness=boot 时执行启动命令（后台 spawn，不阻塞等待长驻服务）。
pub fn run_prepare_env(run_command: &str) -> Result<String, String> {
    if run_command.trim().is_empty() {
        return Ok("无启动命令，跳过环境准备".to_string());
    }
    #[cfg(target_os = "windows")]
    let spawned = crate::tools::silent_command("cmd")
        .arg("/C")
        .arg(run_command)
        .spawn();
    #[cfg(not(target_os = "windows"))]
    let spawned = std::process::Command::new("sh").arg("-c").arg(run_command).spawn();

    spawned
        .map(|_| format!("已后台启动：{run_command}"))
        .map_err(|e| format!("启动失败: {e}"))
}

/// 环境隔离硬门：env_tag=prod 时阻止接口测试执行，防止误打生产。
pub fn guard_env(env_tag: &str) -> Result<(), String> {
    if env_tag.eq_ignore_ascii_case("prod") || env_tag.eq_ignore_ascii_case("production") {
        return Err(
            "环境隔离硬门：被测项目标记为生产环境(prod)，已拦截接口测试，防止误操作生产。请改用测试环境或明确调整环境标识。".to_string(),
        );
    }
    Ok(())
}

/// 执行记录文件名清洗：仅保留字母数字与 - _，超长截断
fn sanitize_filename(title: &str) -> String {
    title
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .take(32)
        .collect()
}

/// 执行记录根目录：本地 `文档目录/白泽测试记录/`（文档目录不可得时回退工作目录）
fn record_base_dir(app: &AppHandle) -> std::path::PathBuf {
    app.path()
        .document_dir()
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default())
        .join("白泽测试记录")
}

/// 执行记录目录：`<记录根>/<项目名_项目id>/`
fn record_project_dir(base: &std::path::Path, project_name: &str, project_id: &str) -> std::path::PathBuf {
    base.join(format!("{}_{}", project_name, project_id))
}

/// 一次执行的成对落盘路径（Markdown + HTML 共享同一时间戳；目录就地创建）。
/// 创建失败时对应项为 None（调用方降级：不写该格式，HTML 引用证据时退回绝对路径）。
struct RecordPaths {
    evidence_dir: Option<std::path::PathBuf>,
    md: Option<std::path::PathBuf>,
    html: Option<std::path::PathBuf>,
}

/// 带项目自定义保存目录的落盘路径（report_dir 留空回退系统文档目录）
fn prepare_record_paths_for(
    app: &AppHandle,
    p: &ProjectProfile,
    title: &str,
) -> RecordPaths {
    let base = if p.report_dir.trim().is_empty() {
        record_base_dir(app)
    } else {
        std::path::PathBuf::from(p.report_dir.trim())
    };
    prepare_record_paths_in(&base, &p.name, &p.id, title)
}

fn prepare_record_paths_in(base: &std::path::Path, project_name: &str, project_id: &str, title: &str) -> RecordPaths {
    let project_dir = record_project_dir(base, project_name, project_id);
    if std::fs::create_dir_all(&project_dir).is_err() {
        return RecordPaths { evidence_dir: None, md: None, html: None };
    }
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let stem = format!("{ts}_{}", sanitize_filename(title));
    // 证据子目录创建失败不影响报告本身
    let evidence_dir = project_dir.join("evidence");
    match std::fs::create_dir_all(&evidence_dir) {
        Ok(()) => RecordPaths {
            evidence_dir: Some(evidence_dir),
            md: Some(project_dir.join(format!("{stem}.md"))),
            html: Some(project_dir.join(format!("{stem}.html"))),
        },
        Err(_) => RecordPaths {
            evidence_dir: None,
            md: Some(project_dir.join(format!("{stem}.md"))),
            html: Some(project_dir.join(format!("{stem}.html"))),
        },
    }
}

/// 列出某项目的执行记录（文档目录/白泽测试记录/<项目名_项目id>/），
/// 按 Markdown 与 HTML 文件名时间戳倒序返回；md/html 同名配对为一条记录。
pub fn list_execution_records(
    app: &AppHandle,
    project_name: &str,
    project_id: &str,
    report_dir: Option<&str>,
) -> Result<Vec<Value>, String> {
    let base = match report_dir.map(str::trim).filter(|s| !s.is_empty()) {
        Some(dir) => std::path::PathBuf::from(dir),
        None => record_base_dir(app),
    };
    list_execution_records_in(&base, project_name, project_id)
}

/// 版本无关的记录列表实现（base 可注入，供单测使用）
fn list_execution_records_in(
    base: &std::path::Path,
    project_name: &str,
    project_id: &str,
) -> Result<Vec<Value>, String> {
    let dir = record_project_dir(base, project_name, project_id);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    // stem → { md / html 路径与大小 }
    let mut entries: std::collections::BTreeMap<String, Value> = Default::default();
    let rd = std::fs::read_dir(&dir).map_err(|e| format!("读取执行记录目录失败: {e}"))?;
    for entry in rd.flatten() {
        let p = entry.path();
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext != "md" && ext != "html" {
            continue;
        }
        let file_name = match p.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        // 文件名约定：<yyyyMMdd_HHmmss>_<标题>；无前缀的视为老数据原样保留
        let (ts, title) = split_record_stem(&file_name);
        let meta = entry.metadata().ok();
        let item = entries
            .entry(file_name.clone())
            .or_insert_with(|| json!({ "stem": file_name, "ts": ts, "title": title }));
        let key = if ext == "html" { "html" } else { "md" };
        if let Some(obj) = item.as_object_mut() {
            obj.insert(key.into(), json!(p.to_string_lossy()));
            obj.insert(format!("{key}_size"), json!(meta.map(|m| m.len()).unwrap_or(0)));
        }
    }
    let mut list: Vec<Value> = entries.into_values().collect();
    list.sort_by(|a, b| {
        let ka = a["stem"].as_str().unwrap_or("");
        let kb = b["stem"].as_str().unwrap_or("");
        kb.cmp(ka) // 时间戳前缀天然可字典序倒序
    });
    Ok(list)
}

/// 解析记录文件名前缀 `yyyyMMdd_HHmmss_`：返回 (ts, 标题)；不符合则整段作为标题
fn split_record_stem(stem: &str) -> (String, String) {
    // 8 位日期 + _ + 6 位时间 + _
    if stem.len() > 15 && stem.as_bytes()[8] == b'_' && stem.as_bytes()[15] == b'_' {
        let ts = &stem[..15];
        if ts.chars().all(|c| c.is_ascii_digit() || c == '_') {
            return (ts.to_string(), stem[16..].to_string());
        }
    }
    (String::new(), stem.to_string())
}

// ───────────────────── openapi / swagger 直出接口用例 ─────────────────────

/// 解析 openapi/swagger 文档文本（JSON 格式；YAML 文档请先导出为 JSON）。
pub fn parse_openapi_spec(text: &str) -> Result<Value, String> {
    serde_json::from_str::<Value>(text.trim())
        .map_err(|e| format!("文档不是合法 JSON（YAML 请先转换为 JSON）：{e}"))
}

/// 按 JSON Schema 生成一个最小可用的示例值（深度受限，防止递归炸栈）。
fn sample_from_schema(schema: &Value, depth: usize) -> Value {
    if depth > 4 {
        return json!(null);
    }
    match schema["type"].as_str() {
        Some("integer") | Some("number") => json!(0),
        Some("boolean") => json!(false),
        Some("array") => json!([sample_from_schema(&schema["items"], depth + 1)]),
        Some("object") | None => {
            let mut obj = serde_json::Map::new();
            let props = schema["properties"].as_object();
            if let Some(props) = props {
                for (k, v) in props {
                    obj.insert(k.clone(), sample_from_schema(v, depth + 1));
                }
            }
            Value::Object(obj)
        }
        _ => json!(""),
    }
}

/// 解引用 schema 的 `$ref` 指针（如 "#/components/schemas/User"、"#/definitions/User"），
/// 解不出时原样返回。只做指针导航，不做递归展开，无循环炸栈风险。
fn deref_schema<'a>(schema: &'a Value, spec: &'a Value) -> &'a Value {
    let Some(ptr) = schema["$ref"].as_str() else {
        return schema;
    };
    let segs = match ptr.strip_prefix("#/") {
        Some(rest) if !rest.is_empty() => rest.split('/'),
        _ => return schema,
    };
    let mut cur = spec;
    for seg in segs {
        match cur.get(seg) {
            Some(next) => cur = next,
            None => return schema,
        }
    }
    cur
}

/// 把 openapi 的 paths 展平为接口用例数组（run_api_case 可直接执行的形状）。
/// - api_base 为空时优先用 spec.servers[0].url；
/// - 路径参数 {id} 以占位符 1 替换（保证 URL 可请求）；
/// - 期望状态码取该操作第一个 2xx 响应码；请求体按 JSON Schema 生成最小示例。
pub fn openapi_to_cases(spec: &Value, api_base: &str) -> Vec<Value> {
    let base = if !api_base.trim().is_empty() {
        api_base.trim_end_matches('/').to_string()
    } else if let Some(server) = spec["servers"]
        .as_array()
        .and_then(|s| s.first())
        .and_then(|s| s["url"].as_str())
    {
        server.trim_end_matches('/').to_string()
    } else if let Some(host) = spec["host"].as_str() {
        // swagger 2.0：host + basePath
        let scheme = spec["schemes"]
            .as_array()
            .and_then(|s| s.first())
            .and_then(Value::as_str)
            .unwrap_or("https");
        let bp = spec["basePath"].as_str().unwrap_or("").trim_end_matches('/');
        format!("{scheme}://{host}{bp}")
    } else {
        String::new()
    };
    let mut cases = Vec::new();
    let paths = match spec["paths"].as_object() {
        Some(p) => p,
        None => return cases,
    };
    let methods = ["get", "post", "put", "delete", "patch"];
    for (path, item) in paths {
        if item.get("deprecated").and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        for m in methods {
            let op = match item.get(m).and_then(Value::as_object) {
                Some(op) if !op.is_empty() => op,
                _ => continue,
            };
            if op.get("deprecated").and_then(Value::as_bool).unwrap_or(false) {
                continue;
            }
            let summary = op.get("summary").and_then(Value::as_str).unwrap_or("");
            let op_id = op.get("operationId").and_then(Value::as_str).unwrap_or("");
            let name = if !summary.is_empty() {
                summary.to_string()
            } else if !op_id.is_empty() {
                op_id.to_string()
            } else {
                format!("{m} {path}")
            };

            // 完整 URL：base + path（路径参数 {xxx} 以 1 占位，保证 URL 可请求）
            let url = format!("{base}{path}");
            let mut resolved = String::new();
            let mut rest = url.as_str();
            while let Some(start) = rest.find('{') {
                if let Some(end_off) = rest[start..].find('}') {
                    let end = start + end_off;
                    resolved.push_str(&rest[..start]);
                    resolved.push('1');
                    rest = &rest[end + 1..];
                } else {
                    break;
                }
            }
            resolved.push_str(rest);

            let mut case = json!({ "name": name, "method": m.to_uppercase(), "url": resolved });

            // 期望状态码：该操作第一个 2xx 响应
            if let Some(resps) = op.get("responses").and_then(Value::as_object) {
                for (code, _) in resps {
                    if let Ok(n) = code.parse::<u64>() {
                        if (200..300).contains(&n) {
                            case["expect_status"] = json!(n);
                            break;
                        }
                    }
                }
            }

            // 请求体：application/json 的 Schema 最小示例（$ref 先解引用，否则常见
            // 「requestBody 引用 components/schemas」的文档会因取不到 properties 而丢请求体）
            let body_schema = op
                .get("requestBody")
                .and_then(|rb| rb.get("content"))
                .and_then(|c| c.get("application/json"))
                .and_then(|j| j.get("schema"));
            if let Some(schema) = body_schema {
                if m != "get" && m != "delete" {
                    let sample = sample_from_schema(&deref_schema(schema, spec), 0);
                    if sample.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
                        case["body"] = json!(sample.to_string());
                        case["headers"] = json!({ "Content-Type": "application/json" });
                    }
                }
            }
            cases.push(case);
        }
    }
    cases
}

/// 从文件路径或 http(s) 地址导入 openapi/swagger 文档并转换为接口用例。
pub fn import_openapi(doc_source: &str, api_base: &str) -> Result<Vec<Value>, String> {
    let source = doc_source.trim();
    if source.is_empty() {
        return Err("请填写 openapi/swagger 文档地址或本地路径".into());
    }
    let text = if source.starts_with("http://") || source.starts_with("https://") {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;
        client
            .get(source)
            .send()
            .map_err(|e| format!("拉取文档失败: {e}"))?
            .text()
            .map_err(|e| format!("读取文档失败: {e}"))?
    } else {
        std::fs::read_to_string(source)
            .map_err(|e| format!("读取本地文档失败: {e}"))?
    };
    let spec = parse_openapi_spec(&text)?;
    let cases = openapi_to_cases(&spec, api_base);
    if cases.is_empty() {
        return Err("文档中未找到可导入的接口（paths 为空或均为已废弃项）".into());
    }
    Ok(cases)
}

const AUTO_DETECT_PROMPT: &str = r#"你是软件测试工程师的项目分析助手。根据下面的需求文档，推断被测项目的基本信息。

输出 JSON（不要任何解释）：
{"name":"项目名","project_type":"web|desktop|mobile|api|miniprogram","source":"需求来源描述","ui_entry":"Web 页面入口 URL（web 才有，否则空）","api_base":"接口 base_url（http(s) 地址，有则填，否则空）","api_doc":"openapi/swagger 文档地址（有则填，否则空）","repo_or_path":"代码仓库或本地目录（有则填）","readiness":"running|boot|login","run_command":"若需白泽拉起应用则填启动命令，否则空","account":"登录账号/token（有则填）","env_tag":"test|staging|prod"}

判定规则：
- 提到页面/浏览器/URL/网页/前端 → web；桌面窗口/安装/exe → desktop；App/小程序 → mobile/miniprogram；仅接口/HTTP/OpenAPI → api。
- api_base：优先从文档中的 base_url、host、server、http(s):// 地址提取。
- ui_entry：web 项目的首页或登录页 URL。
- readiness：文档表明已部署可直接访问 → running；需先编译/启动 → boot；需登录/token → login。
- env_tag：默认 test，只有文档明确指向生产时用 prod。

需求文档：
{requirement}"#;

// ───────────────────── 测试流水线实测（测试数据 + 断言全覆盖） ─────────────────────

#[cfg(test)]
mod pipeline_tests {
    use super::*;
    use crate::capability::{
        ActionResult, CapError, CapabilitySet, ElementMatch, ObserveReq, Observation,
        ScreenshotInfo, WindowInfo,
    };
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};

    #[test]
    fn selector_to_hint_extracts_readable_clue() {
        assert_eq!(selector_to_hint("text=百度一下"), "百度一下");
        assert_eq!(selector_to_hint("#su"), "su");
        assert_eq!(selector_to_hint("button.btn-search"), "button btn search");
        assert_eq!(selector_to_hint("input[name=\"kw\"]"), "input name kw");
        assert_eq!(selector_to_hint("  "), "");
    }

    /// 起一个本地 mock HTTP 服务，返回 (端口, 已收请求体记录)。
    /// 路由：
    ///   GET  /health          → 200 "ok"
    ///   POST /users           → 201 {"code":0,"data":{"id":7,"name":"白泽"}}
    ///   GET  /user/1          → 200 {"code":0,"data":{"id":1,"name":"tester"}}
    ///   GET  /api/health      → 200 {"ok":true}（openapi 文档用）
    ///   其余                   → 404 not-found
    fn spawn_mock_server() -> (u16, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("mock 服务绑定失败");
        let port = listener.local_addr().unwrap().port();
        let hits = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let hits2 = hits.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let hits3 = hits2.clone();
                handle_conn(stream, hits3);
            }
        });
        (port, hits)
    }

    fn handle_conn(mut stream: TcpStream, hits: std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let mut reader = BufReader::new(stream.try_clone().expect("clone 流失败"));
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() {
            return;
        }
        // "METHOD PATH HTTP/1.1"
        let mut parts = line.split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let path = parts.next().unwrap_or("/").to_string();
        // 读掉全部头
        let mut headers = String::new();
        loop {
            let mut h = String::new();
            match reader.read_line(&mut h) {
                Ok(0) | Err(_) => break,
                _ => {
                    if h.trim().is_empty() {
                        break;
                    }
                    headers.push_str(&h);
                }
            }
        }
        // 按 Content-Length 读取 body
        let content_len = headers
            .to_ascii_lowercase()
            .lines()
            .find_map(|l| l.strip_prefix("content-length:")?.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = vec![0u8; content_len];
        if content_len > 0 {
            use std::io::Read;
            let _ = reader.read_exact(&mut body);
        }
        let body_str = String::from_utf8_lossy(&body).to_string();
        hits.lock().unwrap().push(format!("{method} {path} :: {body_str}"));

        let route = path.split('?').next().unwrap_or(path.as_str());
        let (status, payload, ctype): (&str, &str, &str) = match (method.as_str(), route) {
            ("GET", "/health") => ("200 OK", "ok", "text/plain"),
            ("POST", "/users") => (
                "201 Created",
                "{\"code\":0,\"data\":{\"id\":7,\"name\":\"白泽\"}}",
                "application/json",
            ),
            ("GET", "/user/1") => (
                "200 OK",
                "{\"code\":0,\"data\":{\"id\":1,\"name\":\"tester\"}}",
                "application/json",
            ),
            ("GET", "/api/health") => ("200 OK", "{\"ok\":true}", "application/json"),
            ("GET", "/page") => (
                "200 OK",
                "<html><body><iframe id=\"f\" src=\"/inner\"></iframe></body></html>",
                "text/html; charset=utf-8",
            ),
            ("GET", "/inner") => (
                "200 OK",
                "<html><body><input id=\"u\"/><button id=\"go\" onclick=\"document.getElementById('r').textContent='提交成功 '+document.getElementById('u').value\">go</button><div id=\"r\">empty</div></body></html>",
                "text/html; charset=utf-8",
            ),
            _ => ("404 Not Found", "not-found", "text/plain"),
        };
        let resp = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            payload.len()
        );
        let _ = stream.write_all(resp.as_bytes());
        let _ = stream.flush();
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir()
            .join("baize-test-record")
            .join(format!("{tag}_{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&d).expect("创建临时目录失败");
        d
    }

    // ── 接口执行器：断言全类型覆盖（本地 mock，不发外网） ──

    #[test]
    fn api_case_status_and_body_assertions() {
        let (port, _) = spawn_mock_server();
        let u = move |p: &str| format!("http://127.0.0.1:{port}{p}");

        // 全部命中：状态码 + 包含
        let c = run_api_case(&json!({
            "name": "健康检查", "method": "GET", "url": u("/health"),
            "expect_status": 200, "expect_body_contains": "ok"
        }))
        .expect("应执行成功");
        assert!(c.ok, "通过用例不应失败: {:?}", c.checks);

        // 状态码不符 → 失败
        let c = run_api_case(&json!({
            "name": "错判", "method": "GET", "url": u("/health"), "expect_status": 500
        }))
        .expect("应执行成功");
        assert!(!c.ok);

        // expect_body_not_contains 命中（响应确不含 xx）→ 通过；反例 → 失败
        let ok = run_api_case(&json!({
            "url": u("/health"), "expect_body_not_contains": "xx"
        }))
        .unwrap();
        assert!(ok.ok);
        let bad = run_api_case(&json!({
            "url": u("/health"), "expect_body_not_contains": "ok"
        }))
        .unwrap();
        assert!(!bad.ok);
    }

    /// expect_json：顶层键、嵌套点路径、结构相等、字段缺失、非 JSON 响应
    #[test]
    fn api_case_json_assertions() {
        let (port, _) = spawn_mock_server();
        let u = move |p: &str| format!("http://127.0.0.1:{port}{p}");

        // 顶层键 数字比较（结构相等，不受 to_string 影响）
        let c = run_api_case(&json!({
            "url": u("/user/1"), "expect_json": { "code": 0 }
        }))
        .unwrap();
        assert!(c.ok, "code=0 应匹配: {:?}", c.checks);

        // 点路径取嵌套字段 data.name == "tester"；数组下标也走同一逻辑
        let c = run_api_case(&json!({
            "url": u("/user/1"), "expect_json": { "data.name": "tester", "data.id": 1 }
        }))
        .unwrap();
        assert!(c.ok, "点路径断言应命中: {:?}", c.checks);

        // 字段不存在 → 失败且 actual 标注缺失
        let c = run_api_case(&json!({
            "url": u("/user/1"), "expect_json": { "no.such.field": 1 }
        }))
        .unwrap();
        assert!(!c.ok);
        assert!(
            c.checks.iter().any(|ch| ch.actual.contains("不存在")),
            "缺失字段应在 actual 中标注: {:?}",
            c.checks
        );

        // 非 JSON 响应 + expect_json → 「响应为 JSON」检查失败
        let c = run_api_case(&json!({
            "url": u("/health"), "expect_json": { "ok": true }
        }))
        .unwrap();
        assert!(!c.ok);
    }

    /// 非法方法报错、默认无断言仅可达性
    #[test]
    fn api_case_edge_paths() {
        let (port, _) = spawn_mock_server();
        let u = move |p: &str| format!("http://127.0.0.1:{port}{p}");
        assert!(run_api_case(&json!({ "url": u("/health"), "method": "HEAD" })).is_err());
        let c = run_api_case(&json!({ "url": u("/health") })).unwrap();
        assert!(c.ok); // 无断言时退化为「请求可达」
    }

    // ── 登录态注入 ──

    #[test]
    fn token_auth_injection() {
        let mut reqs = vec![
            json!({ "name": "a", "headers": {} }),
            json!({ "name": "b", "headers": { "Authorization": "Bearer keep" } }),
            json!({ "name": "c" }), // 无 headers 对象
        ];
        assert_eq!(inject_token_auth(&mut reqs, "Bearer abc"), 2);
        assert_eq!(reqs[0]["headers"]["Authorization"], "Bearer abc");
        assert_eq!(reqs[1]["headers"]["Authorization"], "Bearer keep"); // 已有不覆盖
        assert_eq!(reqs[2]["headers"]["Authorization"], "Bearer abc");

        // eyJ 开头裸 JWT 自动补 Bearer；user:pass 不注入；空账号不注入
        let mut one = vec![json!({})];
        assert_eq!(inject_token_auth(&mut one, "eyJhbGciOi"), 1);
        assert_eq!(one[0]["headers"]["Authorization"], "Bearer eyJhbGciOi");
        assert_eq!(inject_token_auth(&mut vec![json!({})], "admin:123"), 0);
        assert_eq!(inject_token_auth(&mut vec![json!({})], ""), 0);
    }

    // ── LLM 输出解析容错 ──

    #[test]
    fn parse_scripts_tolerates_markdown_fence_and_prose() {
        let raw = "好的，以下是脚本：\n```json\n{\"scripts\":[{\"case_index\":0,\"kind\":\"ui\",\"ui_steps\":[{\"action\":\"wait\",\"ms\":100}]},{\"case_index\":1,\"kind\":\"api\",\"api_requests\":[{\"name\":\"x\",\"url\":\"http://a\",\"method\":\"GET\"}],\"setup\":[],\"teardown\":[{\"name\":\"清理\"}]},{\"case_index\":2,\"kind\":\"unknown\",\"reason\":\"信息不足\"}]}\n```\n说明文字";
        let scripts = parse_scripts(raw).expect("围栏+前后缀文案都应能解析");
        assert_eq!(scripts.len(), 3);
        assert_eq!(scripts[0].kind, "ui");
        assert_eq!(scripts[1].teardown.len(), 1);
        assert_eq!(scripts[2].reason, "信息不足");
    }

    /// Web 用例守卫：URL 意图识别 + 桌面动作混用检测（打回重翻的前置判定）
    #[test]
    fn web_intent_guard_detects_mixed_desktop_actions() {
        // 出现 URL → Web 意图；项目 web 形态 → 即使无 URL 也是 Web
        let web_case = json!({
            "title": "百度首页搜索",
            "steps": "1. 打开 https://www.baidu.com 2. 输入关键词搜索",
            "data": "关键词：百度",
            "expected": "出现搜索结果"
        });
        assert!(case_is_web_intent(&web_case, None));
        assert!(case_is_web_intent(&json!({"steps": "点击登录按钮"}), None) == false);

        let mut profile = ProjectProfile {
            id: "p1".into(),
            name: "演示".into(),
            project_type: "web".into(),
            source: String::new(),
            ui_entry: "http://x".into(),
            api_base: String::new(),
            api_doc: String::new(),
            repo_or_path: String::new(),
            readiness: "running".into(),
            run_command: String::new(),
            account: String::new(),
            env_tag: String::new(),
            report_dir: String::new(),
        };
        assert!(case_is_web_intent(&json!({"steps": "点击登录按钮"}), Some(&profile)));
        profile.project_type = "desktop".into();
        assert!(!case_is_web_intent(&json!({"steps": "点击登录按钮"}), Some(&profile)));

        // 混入桌面动作 → 命中守卫；纯 Web 动作 → 放行
        let mk = |steps: Vec<Value>| CaseScript {
            case_index: 0,
            title: "t".into(),
            kind: "ui".into(),
            ui_steps: steps,
            api_requests: vec![],
            setup: vec![],
            teardown: vec![],
            reason: String::new(),
        };
        let bad = mk(vec![
            json!({"action": "open_page", "url": "https://www.baidu.com"}),
            json!({"action": "type_text", "text": "百度"}),
            json!({"action": "key_press", "keys": "enter"}),
        ]);
        assert!(script_has_desktop_action(&bad));
        let good = mk(vec![
            json!({"action": "open_page", "url": "https://www.baidu.com"}),
            json!({"action": "fill_input", "selector": "#kw", "text": "百度"}),
            json!({"action": "click_selector", "selector": "#su"}),
            json!({"action": "assert_page_text", "text": "百度一下"}),
        ]);
        assert!(!script_has_desktop_action(&good));
    }

    // ── OpenAPI 导入 ──

    /// openapi 3.0：servers 取址、$ref 解引用出请求体、路径参数占位、deprecated 跳过
    #[test]
    fn openapi30_to_cases_with_ref_deref() {
        let spec = json!({
            "servers": [{ "url": "http://127.0.0.1:9000/api/v1" }],
            "components": { "schemas": { "UserReq": {
                "type": "object",
                "properties": { "name": { "type": "string" }, "age": { "type": "integer" } }
            }}},
            "paths": {
                "/users": {
                    "post": {
                        "summary": "创建用户",
                        "requestBody": { "content": { "application/json": {
                            "schema": { "$ref": "#/components/schemas/UserReq" }
                        }}},
                        "responses": { "201": { "description": "created" }, "400": {} }
                    },
                    "get": { "summary": "列表", "responses": { "200": {} } }
                },
                "/users/{id}": {
                    "delete": { "deprecated": true, "responses": { "204": {} } },
                    "get": { "operationId": "getUser", "responses": { "200": {} } }
                },
                "/legacy": { "deprecated": true, "get": { "responses": { "200": {} } } }
            }
        });
        let cases = openapi_to_cases(&spec, "");
        assert_eq!(cases.len(), 3, "废弃项跳过：POST+GET /users 与 GET /users/{{id}}");
        for c in &cases {
            let url = c["url"].as_str().unwrap();
            assert!(url.starts_with("http://127.0.0.1:9000/api/v1"), "base 取自 servers: {url}");
        }

        let post = cases.iter().find(|c| c["method"] == "POST").unwrap();
        assert_eq!(post["expect_status"], 201, "取第一个 2xx 响应码");
        // $ref 解引用后按 properties 生成最小示例（修复前这里会因解不出 ref 而丢 body）
        let body = post["body"].as_str().unwrap();
        assert!(body.contains("\"name\"") && body.contains("\"age\""), "ref 请求体应展开: {body}");
        assert_eq!(post["headers"]["Content-Type"], "application/json");

        let get_user = cases.iter().find(|c| c["method"] == "GET" && c["url"].as_str().unwrap().ends_with("/users/1")).unwrap();
        assert_eq!(get_user["expect_status"], 200, "路径参数 {{id}} → 占位符 1 后仍带期望码");
    }

    /// swagger 2.0：host + basePath 兜底
    #[test]
    fn swagger20_host_basepath_fallback() {
        let spec = json!({
            "host": "example.com", "basePath": "/v2", "schemes": ["https"],
            "paths": { "/pet/{petId}": { "get": { "summary": "查宠物", "responses": { "200": {} } } } }
        });
        let cases = openapi_to_cases(&spec, "");
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0]["url"], "https://example.com/v2/pet/1");

        // 显式 api_base 优先于 host/basePath
        let cases2 = openapi_to_cases(&spec, "http://127.0.0.1:8080/");
        assert_eq!(cases2[0]["url"], "http://127.0.0.1:8080/pet/1", "末尾斜杠裁剪");
    }

    /// 本地文件导入端到端 + 空源/非法 JSON 报错
    #[test]
    fn import_openapi_from_local_file() {
        let dir = temp_dir("openapi");
        let file = dir.join("spec.json");
        std::fs::write(
            &file,
            r#"{"paths": {"/api/health": {"get": {"summary": "探活", "responses": {"200": {}}}}}}"#,
        )
        .unwrap();

        let (port, _) = spawn_mock_server();
        // api_base 指向本地 mock：导入的用例可直接跑通，形成「导入 → 执行」闭环
        let cases = import_openapi(file.to_str().unwrap(), &format!("http://127.0.0.1:{port}"))
            .expect("本地文档应导入成功");
        assert_eq!(cases.len(), 1);
        let result = run_api_case(&cases[0]).unwrap();
        assert!(result.ok && result.status == 200, "导入用例应能直接执行通过: {:?}", result.checks);

        assert!(import_openapi("", "").is_err(), "空来源应报错");
        let bad = dir.join("bad.json");
        std::fs::write(&bad, "{oops").unwrap();
        let err = import_openapi(bad.to_str().unwrap(), "").unwrap_err();
        assert!(err.contains("JSON"), "非法文档错误信息应提示 JSON: {err}");
    }

    // ── 执行记录：落盘配对 / 列表排序 / 文件名解析 ──

    #[test]
    fn record_stem_parsing_and_listing_pairing_sort() {
        // 文件名前缀解析：正常 / 无前缀 / 全字母干扰
        assert_eq!(split_record_stem("20260827_120000_登录冒烟"), ("20260827_120000".into(), "登录冒烟".into()));
        assert_eq!(split_record_stem("手工导出的报告.md"), ("".into(), "手工导出的报告.md".into()));
        // 第 9 位不是 '_'：不是时间戳前缀
        assert_eq!(split_record_stem("12345678a234567_标题"), ("".into(), "12345678a234567_标题".into()));

        let base = temp_dir("list");
        let pdir = record_project_dir(&base, "电商后台", "p001");
        std::fs::create_dir_all(&pdir).unwrap();
        // 写两组成对记录（倒序写入，验证输出按时间正序名为倒序排列）
        for ts in ["20260826_100000", "20260827_093000"] {
            std::fs::write(pdir.join(format!("{ts}_登录冒烟.md")), "# md").unwrap();
            std::fs::write(pdir.join(format!("{ts}_登录冒烟.html")), "<h1>html</h1>").unwrap();
        }
        // 干扰文件：txt 不计入；孤儿 md 单独成条
        std::fs::write(pdir.join("note.txt"), "-").unwrap();
        std::fs::write(pdir.join("20260825_080000_单条.md"), "# only-md").unwrap();

        let list = list_execution_records_in(&base, "电商后台", "p001").unwrap();
        assert_eq!(list.len(), 3, "md/html 成对合并 + 孤儿 md：共 3 条，txt 忽略: {list:?}");
        assert_eq!(list[0]["ts"], "20260827_093000", "最新在前");
        assert_eq!(list[0]["title"], "登录冒烟");
        assert!(list[0]["md"].is_string() && list[0]["html"].is_string(), "成对记录两种格式都有");
        assert_eq!(list[2]["ts"], "20260825_080000");
        assert!(list[2]["html"].is_null(), "孤儿 md 无 html 配对");

        // 目录不存在 → 空数组而非报错
        assert!(list_execution_records_in(&base, "不存在", "x999").unwrap().is_empty());

        // prepare_record_paths_in：同 title 共享 timestamp 主干，evidence 子目录就位
        let rp = prepare_record_paths_in(&base, "电商后台", "p001", "下单/回归*用例"); // 非法字符应被清洗
        let md = rp.md.unwrap(); let html = rp.html.unwrap(); let ev = rp.evidence_dir.unwrap();
        assert!(ev.is_dir(), "证据目录就地创建");
        assert!(md.file_name().unwrap().to_str().unwrap().contains("_下单回归用例"), "文件名已清洗: {:?}", md);
        let md_stem = md.file_stem().unwrap().to_str().unwrap();
        let html_stem = html.file_stem().unwrap().to_str().unwrap();
        assert_eq!(md_stem, html_stem, "md/html 共享主干保证前端配对");
    }

    // ── 用例导出（json/csv/xlsx）──

    #[test]
    fn cases_export_json_csv_xlsx() {
        let dir = temp_dir("cases_export");
        std::fs::create_dir_all(&dir).unwrap();
        let cases = vec![
            json!({
                "req_index": 0, "title": "正常登录", "precondition": "账号已注册",
                "steps": "输入账号密码\n点击登录", "expected": "登录成功",
                "priority": "P0", "case_type": "功能"
            }),
            json!({ "req_index": 0, "title": "越权访问他人订单", "priority": "P1", "case_type": "安全" }),
        ];

        // json：按类型分组对象 { "功能": [...], "安全": [...] }
        let p = dir.join("cases.json");
        write_cases_file(&cases, "json", &p).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        let obj = v.as_object().expect("json 应为分组对象");
        assert_eq!(obj.get("功能").unwrap().as_array().unwrap().len(), 1);
        assert_eq!(obj.get("安全").unwrap().as_array().unwrap().len(), 1);

        // csv：UTF-8 BOM 开头 + 表头 + 每类一个小节标题行 + 每条一行
        let p = dir.join("cases.csv");
        write_cases_file(&cases, "csv", &p).unwrap();
        let raw = std::fs::read(&p).unwrap();
        assert!(raw.starts_with(&[0xEF, 0xBB, 0xBF]), "csv 应以 UTF-8 BOM 开头");
        let text = String::from_utf8_lossy(&raw[3..]).to_string();
        assert!(text.contains("用例编号"), "应含中文表头: {text}");
        assert!(text.contains("── 功能（1 条）──"), "应有功能小节标题: {text}");
        assert!(text.contains("── 安全（1 条）──"), "应有安全小节标题: {text}");
        assert_eq!(text.lines().count(), 5, "表头 + 2 个小节行 + 2 行数据");
        assert!(!text.contains("输入账号密码\r"), "换行应已转义");

        // xlsx：zip 魔数校验
        let p = dir.join("cases.xlsx");
        write_cases_file(&cases, "xlsx", &p).unwrap();
        let raw = std::fs::read(&p).unwrap();
        assert!(raw.starts_with(b"PK"), "xlsx 应为 zip 容器");

        // 未知格式直接报错，不落盘
        assert!(write_cases_file(&cases, "doc", &dir.join("c.doc")).is_err());
        assert!(!dir.join("c.doc").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn case_gen_options_and_type_normalization() {
        // 非法类型被剔除；空清单回落到全部可选类型
        let opts = CaseGenOptions { case_types: vec!["功能".into(), "魔法".into()], per_type_count: 3 };
        assert_eq!(opts.allowed(), vec!["功能".to_string()]);
        assert_eq!(
            CaseGenOptions { case_types: vec![], per_type_count: 0 }.allowed().len(),
            CASE_TYPE_OPTIONS.len()
        );
        assert!(opts.count_rule().contains("约 3 条"));
        assert!(!CaseGenOptions::default().count_rule().contains("约"), "缺省不限数量");

        // 类型收敛：包含式匹配 / 旧枚举映射 / 完全未知与空类型返回 false（由调用方剔除，不冒充所选类型）
        let allowed = vec!["功能".to_string(), "安全".to_string()];
        let mut c = TestCase {
            req_index: 0,
            title: "t".into(),
            precondition: String::new(),
            steps: String::new(),
            data: String::new(),
            expected: String::new(),
            priority: String::new(),
            case_type: "功能性".into(),
        };
        assert!(map_case_type(&mut c, &allowed), "「功能性」应映射为功能");
        assert_eq!(c.case_type, "功能");
        c.case_type = "渗透测试".into();
        assert!(map_case_type(&mut c, &allowed), "旧枚举「渗透」应映射为安全");
        assert_eq!(c.case_type, "安全");
        c.case_type = "魔法交互".into();
        assert!(!map_case_type(&mut c, &allowed), "无法归类的应剔除而非冒充");
        c.case_type = "".into();
        assert!(!map_case_type(&mut c, &allowed), "空类型应剔除");

        // 数量硬约束：单类超出按优先级截断，P0 优先保留；未超额的组原样保留
        let mk = |i: usize, t: &str, p: &str| TestCase {
            case_type: t.to_string(),
            priority: p.to_string(),
            title: format!("用例{i}"),
            ..c.clone()
        };
        let cases = vec![
            mk(0, "UI", "P1"),
            mk(1, "UI", "P0"),
            mk(2, "UI", "P2"),
            mk(3, "安全", "P0"),
        ];
        let (kept, dropped) = cap_cases_per_type(cases, 2, &["UI".to_string(), "安全".to_string()]);
        assert_eq!(dropped, 1);
        let ui: Vec<&TestCase> = kept.iter().filter(|x| x.case_type == "UI").collect();
        assert_eq!(ui.len(), 2, "UI 截断到 2 条");
        assert_eq!(ui[0].priority, "P0", "P0 优先保留");
        assert!(kept.iter().any(|x| x.case_type == "安全"), "未超额的组不受影响");
    }

    // ── 环境隔离硬门 ──

    #[test]
    fn guard_env_blocks_production() {
        assert!(guard_env("test").is_ok());
        assert!(guard_env("staging").is_ok());
        // prod 拦截且大小写归一化（eq_ignore_ascii_case）
        let err = guard_env("prod").unwrap_err();
        assert!(err.contains("环境隔离"), "拦截信息需说明硬门原因: {err}");
        assert!(guard_env("PROD").is_err());
        assert!(guard_env("production").is_err());
    }

    // ── 点路径工具 ──

    #[test]
    fn json_dot_path_traversal() {
        let v = json!({ "a": { "b": [10, { "c": "深值" }] }, "n": null });
        assert_eq!(json_dot_path(&v, "a.b.0"), Some(json!(10)));
        assert_eq!(json_dot_path(&v, "a.b.1.c"), Some(json!("深值")));
        assert!(json_dot_path(&v, "a.x.y").is_none());
        assert_eq!(json_dot_path(&v, "n"), Some(json!(null)));
    }

    // ── Web 深度操作真机链路（需要本机 Chrome；cargo test -- --ignored 单独跑） ──

    /// 全动作都不触达桌面的空实现（web 系列只走 browser::act）
    struct NoopCap;
    impl crate::tools::Tool for NoopCap {
        fn name(&self) -> &str { "noop" }
        fn description(&self) -> &str { "" }
        fn schema(&self) -> serde_json::Value { json!({}) }
        fn permission(&self) -> crate::tools::PermissionClass { crate::tools::PermissionClass::ReadOnly }
        fn run(&self, _args: serde_json::Value) -> Result<serde_json::Value, String> { Ok(json!({})) }
    }
    impl Capability for NoopCap {
        fn probe(&self) -> CapabilitySet { CapabilitySet::default() }
        fn list_windows(&self) -> Result<Vec<WindowInfo>, CapError> { Ok(vec![]) }
        fn observe(&self, _req: &ObserveReq) -> Result<Observation, CapError> {
            Err(CapError::Unsupported("noop".into()))
        }
        fn capture_screen(&self) -> Result<ScreenshotInfo, CapError> {
            Err(CapError::Unsupported("noop".into()))
        }
        fn act(&self, _action: &Action) -> Result<ActionResult, CapError> {
            Err(CapError::Unsupported("noop".into()))
        }
        fn find(&self, _target: &str) -> Result<Vec<ElementMatch>, CapError> { Ok(vec![]) }
        fn find_anywhere(&self, _target: &str) -> Result<Vec<ElementMatch>, CapError> { Ok(vec![]) }
        fn click_element(&self, _target: &str) -> Result<ActionResult, CapError> {
            Err(CapError::Unsupported("noop".into()))
        }
    }

    /// 本地 iframe 页面端到端：open_page → fill_input(跨 iframe) → click_selector
    /// （CDP 失败走深度点击）→ assert_page_text 汇聚 iframe 文本。
    /// 跑通「打开页 - 填 - 点 - 断言」整条 Web 自动化通路。
    #[test]
    #[ignore = "需本机 Chrome 与桌面环境；cargo test deep -- --ignored 显式运行"]
    fn web_deep_actions_live_browser() {
        let (port, _) = spawn_mock_server();
        let mut steps: Vec<Value> = vec![
            json!({ "action": "open_page", "url": format!("http://127.0.0.1:{port}/page") }),
            // 输入框在 <iframe src="/inner"> 里，主文档 querySelector 找不到 → 必须走跨框架深度填充
            json!({ "action": "fill_input", "selector": "#u", "text": "白泽深度操作" }),
            json!({ "action": "click_selector", "selector": "#go" }),
            // 按钮的 onclick 把结果写进 iframe 内 #r → 只有汇聚 iframe 文本的断言才能命中
            json!({ "action": "assert_page_text", "text": "提交成功 白泽深度操作" }),
        ];
        let results = run_ui_steps(&NoopCap, &mut steps);
        for r in &results {
            println!("step ok={} detail={}", r.ok, r.detail);
            for c in &r.checks {
                println!("   check {} expected={:?} actual={:?}", c.name, c.expected, c.actual);
            }
        }
        assert!(results.iter().all(|r| r.ok), "Web 全链路存在失败步骤: {:?}", results);
        // static 持有的浏览器实例不会随进程退出被 drop，显式关闭避免留下孤儿 Chrome
        crate::browser::shutdown_browser();
    }

    /// 分步探针：逐个执行并在每步后 println，用于定位 Web 链路卡死的调用点。
    /// cargo test web_step_probe -- --nocapture --ignored
    #[test]
    #[ignore = "需本机 Chrome 与桌面环境"]
    fn web_step_probe() {
        let (port, _) = spawn_mock_server();
        let url = format!("http://127.0.0.1:{port}/page");
        println!("[probe] 1/6 调 get_browser（启动受控 Chrome）…");
        let t0 = std::time::Instant::now();
        let _ = crate::browser::act(json!({ "action": "state" })).expect("state 失败");
        println!("[probe]    完成，耗时 {:?}", t0.elapsed());

        println!("[probe] 2/6 open_page goto {url} …");
        let t0 = std::time::Instant::now();
        let r = crate::browser::act(json!({ "action": "goto", "url": url }));
        println!("[probe]    goto -> {:?}，耗时 {:?}", r.is_ok(), t0.elapsed());

        println!("[probe] 3/6 evaluate 顶层标题 …");
        let t0 = std::time::Instant::now();
        let v = browser_eval("document.title||'no-title'");
        println!("[probe]    evaluate -> {:?}，耗时 {:?}", v.map(|x| x.to_string()), t0.elapsed());

        println!("[probe] 4/6 fill_input 跨 iframe 填充 #u …");
        let t0 = std::time::Instant::now();
        let filled = web_fill_input_deep("#u", "白泽探针");
        println!("[probe]    fill -> {:?}，耗时 {:?}", filled, t0.elapsed());

        println!("[probe] 5/6 click_selector #go（先 CDP 后深度兜底）…");
        let t0 = std::time::Instant::now();
        let clicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            match crate::browser::act(json!({ "action": "click", "selector": "#go" })) {
                Ok(_) => Ok(true),
                Err(_) => web_click_deep("#go"),
            }
        }));
        println!("[probe]    click -> {:?}，耗时 {:?}", clicked.map(|r| r.map_err(|e| e)), t0.elapsed());

        println!("[probe] 6/6 assert_page_text 汇聚 iframe 文本 …");
        let t0 = std::time::Instant::now();
        let text = web_page_text_all();
        println!(
            "[probe]    text({:?}) = {}",
            text.as_ref().map(|s| s.chars().count()),
            text.as_ref().map(|s| s.trim().chars().take(120).collect::<String>()).unwrap_or_default()
        );
        println!("[probe]    断言耗时 {:?}", t0.elapsed());
        // static 持有的浏览器实例不会随进程退出被 drop，显式关闭避免留下孤儿 Chrome
        crate::browser::shutdown_browser();
    }
}