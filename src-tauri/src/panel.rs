//! 面板控制工具 —— 把主界面顶栏各功能面板注册成 agent 可感知的工具。
//!
//! 后端 emit `panel-control` 事件，前端 App.tsx 监听后调用 openPanel/closePanel。
//! 工具描述里内嵌每个面板的用途清单，模型据此在用户需求出现时自主决定打开哪个面板
//! （例：聊天里甩来需求文档并要求测试 → 打开 test 测试面板）。

use std::sync::OnceLock;

use regex::Regex;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::tools::{PermissionClass, Tool};

static APP_HANDLE: OnceLock<AppHandle> = OnceLock::new();

/// 在应用 setup 中注入句柄（工具 run 时无需 State 参数）
pub fn init_handle(app: AppHandle) {
    let _ = APP_HANDLE.set(app);
}

/// 面板清单：(id, 中文名 + 用途说明)。新增顶栏页面时同步维护这里。
const PANELS: &[(&str, &str)] = &[
    ("butler", "软件管家（安装/卸载/管理电脑软件、软件安装进度）"),
    ("schedule", "计划（定时任务与日程提醒的编排管理）"),
    ("workflow", "工作流（查看/运行多步骤自动化工作流）"),
    ("plaza", "任务广场（技能、工具、工作流的聚合广场）"),
    ("imlog", "IM 消息总线（IM 消息收发与总线记录）"),
    ("meeting", "会议室（多人协作与会议记录）"),
    ("chrome", "浏览器（受控 Chrome 浏览器窗口）"),
    (
        "test",
        "测试面板（自动化测试全流程：项目配置 → 需求生成用例 → 执行用例 → 测试报告；含 UI 测试与接口测试）",
    ),
    ("settings", "设置（模型配置、API 密钥、工作模式、运行时模型）"),
];

fn panel_catalog() -> String {
    PANELS
        .iter()
        .map(|(id, desc)| format!("{id}: {desc}"))
        .collect::<Vec<_>>()
        .join("；")
}

/// 聊天消息 → 面板 的正则意图表：(面板 id, 触发正则)。
/// 用户消息命中即直接打开对应面板——比等 LLM 调 panel_control 更快且零 token；
/// LLM 自主决策仍保留（处理正则覆盖不到的复杂意图）。
const TRIGGERS: &[(&str, &str)] = &[
    // 测试面板：用例/测试/报告相关（仅测试工程师模式下触发，与顶栏入口显隐一致）
    (
        "test",
        r"生成用例|测试用例|用例设计|执行用例|跑.{0,6}测试|接口测试|[Uu][Ii]\s*测试|自动化测试|测试报告|覆盖检查",
    ),
    // 软件管家：安装/卸载「软件/应用」（「安装依赖」这类开发操作不触发）
    ("butler", r"(安装|卸载).{0,16}(软件|应用)|(打开|显示).{0,4}软件管家"),
    // 计划：定时提醒 / 日程
    ("schedule", r"(定时|到点|X分钟|几分钟).{0,6}提醒|提醒我|日程|闹钟|计划任务|(打开|显示).{0,4}计划"),
    // 工作流
    ("workflow", r"工作流|自动化流程|(打开|显示|查看).{0,4}流程"),
    // 任务广场
    ("plaza", r"任务广场|技能广场"),
    // IM 消息总线
    ("imlog", r"消息总线|IM\s*记录|消息记录|(打开|显示).{0,4}消息总线"),
    // 会议室
    ("meeting", r"会议室|会议记录|多方协作|(打开|进入).{0,4}会议室"),
    // 浏览器面板（受控 Chrome；「打开网页」由 browser_open 工具处理，不在此列）
    ("chrome", r"(打开|显示).{0,6}(内置)?浏览器|浏览器面板"),
    // 设置（避免「设置环境变量」误触，要求明确的设置面板语义）
    ("settings", r"API\s*Key|API密钥|密钥配置|模型配置|打开设置|设置面板|系统设置"),
];

/// 编译后的正则缓存（首次使用时编译一次）
fn trigger_regexes() -> &'static Vec<(String, Regex)> {
    static RE: OnceLock<Vec<(String, Regex)>> = OnceLock::new();
    RE.get_or_init(|| {
        TRIGGERS
            .iter()
            .filter_map(|(id, pat)| Regex::new(pat).ok().map(|re| (id.to_string(), re)))
            .collect()
    })
}

/// 纯匹配逻辑：返回命中的面板 id（不 emit 事件，便于单元测试）
fn match_trigger(message: &str, is_qa: bool) -> Option<String> {
    for (id, re) in trigger_regexes() {
        if id == "test" && !is_qa {
            continue;
        }
        if re.is_match(message) {
            return Some(id.clone());
        }
    }
    None
}

/// 对用户消息做正则意图匹配，命中则 emit `panel-control` 直接打开对应面板。
/// 返回命中的面板 id（供调用方写执行流日志）；未命中返回 None。
pub fn detect_intent(app: &AppHandle, message: &str) -> Option<String> {
    // 测试面板仅在「软件测试工程师」模式下触发（与顶栏入口显隐规则一致）
    let is_qa = {
        let state = app.state::<crate::AppState>();
        state.workmodes.current_id().as_deref() == Some("qa-engineer")
    };
    let matched = match_trigger(message, is_qa);
    if let Some(id) = &matched {
        let _ = app.emit("panel-control", json!({ "action": "open", "panel": id }));
    }
    matched
}

/// panel_control 工具：open / close 指定面板
pub struct PanelControlTool;

impl Tool for PanelControlTool {
    fn name(&self) -> &str {
        "panel_control"
    }

    fn description(&self) -> &str {
        // 描述里直接携带面板目录，模型看 tools 列表就能「知道」每个页面的存在与用途；
        // 拼接结果缓存一次（description 会被每轮对话的 schemas() 反复调用，不能重复分配）
        static DESC: OnceLock<String> = OnceLock::new();
        DESC.get_or_init(|| {
            "打开或关闭白泽主界面的功能面板。可用面板：软件管家(butler)、计划(schedule)、工作流(workflow)、任务广场(plaza)、IM消息总线(imlog)、会议室(meeting)、浏览器(chrome)、测试面板(test)、设置(settings)。\n\
             使用时机举例：\n\
             1. 用户给出需求/需求文档并要求测试、生成用例、跑接口/UI 测试、看测试报告 → 打开 test 面板，再继续用测试相关工具完成全流程；\n\
             2. 用户要求安装/管理软件 → 打开 butler；用户要求定时提醒 → 打开 schedule；用户想看/编辑自动化流程 → 打开 workflow；\n\
             3. 需要用户手动操作某个页面（如填 API Key、选项目）时，先打开对应面板再提示；\n\
             4. 完成操作后若面板不再需要，可 close 收回界面。\n\
             全部面板：__CATALOG__"
                .replace("__CATALOG__", &panel_catalog())
        })
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["open", "close"],
                    "description": "open 打开面板 / close 关闭面板（回到对话视图）"
                },
                "panel": {
                    "type": "string",
                    "enum": PANELS.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
                    "description": "面板 id（close 也可省略 panel 关闭当前面板）"
                }
            },
            "required": ["action"]
        })
    }

    fn permission(&self) -> PermissionClass {
        // 纯界面导航：只读级别自动放行
        PermissionClass::ReadOnly
    }

    fn run(&self, args: Value) -> Result<Value, String> {
        let action = args["action"]
            .as_str()
            .ok_or("panel_control 缺少 action（open/close）")?;
        let panel = args["panel"].as_str().unwrap_or("");
        if action == "open" {
            if panel.is_empty() || !PANELS.iter().any(|(id, _)| *id == panel) {
                return Err(format!(
                    "未知面板「{panel}」，可用：{}",
                    PANELS.iter().map(|(id, _)| *id).collect::<Vec<_>>().join("/")
                ));
            }
        }
        let app = APP_HANDLE
            .get()
            .ok_or("面板控制不可用（应用尚未初始化完成）")?;
        app.emit(
            "panel-control",
            json!({ "action": action, "panel": panel }),
        )
        .map_err(|e| format!("发送面板控制事件失败: {e}"))?;
        let verb = if action == "open" { "已打开" } else { "已关闭" };
        let label = PANELS
            .iter()
            .find(|(id, _)| *id == panel)
            .map(|(_, d)| d.split("（").next().unwrap_or(panel))
            .unwrap_or("当前面板");
        Ok(json!({ "ok": true, "message": format!("{verb}{label}") }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_control_rejects_unknown_panel_on_open() {
        let t = PanelControlTool;
        let err = t
            .run(json!({ "action": "open", "panel": "nope" }))
            .unwrap_err();
        assert!(err.contains("未知面板"));
    }

    #[test]
    fn panel_control_close_allows_empty_panel() {
        // close 缺省 panel 表示关闭当前面板（前端处理），不校验
        let t = PanelControlTool;
        // 未初始化 APP_HANDLE 时应报「不可用」而非「未知面板」——证明校验只针对 open
        let err = t.run(json!({ "action": "close" })).unwrap_err();
        assert!(err.contains("不可用"));
    }

    #[test]
    fn description_contains_all_panels() {
        let t = PanelControlTool;
        let d = t.description();
        assert!(d.contains("test") && d.contains("butler") && d.contains("settings"));
    }

    #[test]
    fn trigger_test_panel_requires_qa_mode() {
        assert_eq!(
            match_trigger("帮我根据这份需求生成用例并跑一遍测试", true).as_deref(),
            Some("test")
        );
        // 非测试工程师模式不触发测试面板
        assert_eq!(
            match_trigger("帮我根据这份需求生成用例并跑一遍测试", false),
            None
        );
    }

    #[test]
    fn trigger_butler_ignores_dependency_install() {
        assert_eq!(
            match_trigger("帮我安装一下 7-Zip 这个软件", true).as_deref(),
            Some("butler")
        );
        // 「安装依赖」是开发操作，不应打开软件管家
        assert_eq!(match_trigger("帮我安装依赖", true), None);
        assert_eq!(match_trigger("在这个项目里执行 npm install", true), None);
    }

    #[test]
    fn trigger_schedule_and_workflow() {
        assert_eq!(match_trigger("10 分钟后提醒我喝水", true).as_deref(), Some("schedule"));
        assert_eq!(match_trigger("帮我看一下工作流列表", true).as_deref(), Some("workflow"));
    }

    #[test]
    fn trigger_settings_avoids_env_set_confusion() {
        // 「设置环境变量」不应打开设置面板
        assert_eq!(match_trigger("帮我设置环境变量 JAVA_HOME", true), None);
        assert_eq!(match_trigger("我要配置 API Key", true).as_deref(), Some("settings"));
    }

    #[test]
    fn trigger_plain_chat_no_match() {
        assert_eq!(match_trigger("今天天气怎么样？", true), None);
        assert_eq!(match_trigger("写一个快排函数", true), None);
    }
}
