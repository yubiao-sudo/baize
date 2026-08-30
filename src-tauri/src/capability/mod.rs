//! Computer Use 能力抽象 —— 见《ComputerUse 接口设计》§2/§3
//!
//! M2 只读：observe()（无障碍树感知）、list_windows()（窗口枚举）与 read_screen/read_window 工具。
//! M4 起补充 act()（输入注入）与 ground()（三级接地）。

#[cfg(windows)]
pub(crate) mod windows;
#[cfg(not(windows))]
mod stub;

use std::sync::{Arc, OnceLock};
use tauri::AppHandle;

use serde_json::{json, Value};

use crate::tools::{PermissionClass, Tool};

/// 平台能力抽象：业务层无平台分支，三端各自实现
pub trait Capability: Send + Sync {
    fn probe(&self) -> CapabilitySet;
    fn list_windows(&self) -> Result<Vec<WindowInfo>, CapError>;
    fn observe(&self, req: &ObserveReq) -> Result<Observation, CapError>;
    fn capture_screen(&self) -> Result<ScreenshotInfo, CapError>;
    fn act(&self, action: &Action) -> Result<ActionResult, CapError>;
    /// 三级接地一级：按名称模糊查找控件，返回匹配项（名称 + 位置）
    fn find(&self, target: &str) -> Result<Vec<ElementMatch>, CapError>;
    /// 跨所有顶层窗口查找控件（而非仅前台窗口），用于定位弹出对话框里的按钮
    fn find_anywhere(&self, target: &str) -> Result<Vec<ElementMatch>, CapError>;
    /// 三级接地一级：按名称查找并语义点击控件（无需坐标）
    fn click_element(&self, target: &str) -> Result<ActionResult, CapError>;
    /// 可交互元素地图：枚举目标窗口的可交互控件（名称/类型/中心坐标），
    /// 供「先分析应用结构 → 一轮批量派发操作」的两阶段 GUI 模式使用
    fn interactive_map(&self, _window: Option<String>) -> Result<Value, CapError> {
        Err(CapError::Unsupported("当前平台不支持 ui_analyze"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObserveMode {
    TreeOnly,
    Mixed,
}

#[derive(Debug, Clone)]
pub enum WindowTarget {
    ByName(String),
}

#[derive(Debug, Clone)]
pub struct ObserveReq {
    pub mode: ObserveMode,
    pub max_depth: usize,
    pub max_nodes: usize,
    /// None = 前台窗口；Some = 指定窗口
    pub window: Option<WindowTarget>,
}

impl Default for ObserveReq {
    fn default() -> Self {
        Self {
            mode: ObserveMode::TreeOnly,
            max_depth: 8,
            max_nodes: 120,
            window: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub name: String,
    pub role: String,
    /// Windows 窗口类名（如 "#32770" 对话框、空标题弹窗据此识别）
    pub class: String,
    pub bbox: Option<Rect>,
}

#[derive(Debug, Clone)]
pub struct ScreenshotInfo {
    pub path: String,
    pub width: u32,
    pub height: u32,
    /// 该屏左上角在虚拟桌面坐标系中的物理偏移（多显示器标定用）
    pub offset_x: i32,
    pub offset_y: i32,
}

#[derive(Debug, Clone)]
pub struct ElementMatch {
    pub name: String,
    pub bbox: Option<Rect>,
    /// 与目标的匹配得分（0-120，由 match_score 评分；越高越可信）
    pub score: u32,
}

#[derive(Debug, Clone)]
pub struct Observation {
    pub source: String,
    pub tree: Option<A11yTree>,
}

#[derive(Debug, Clone)]
pub struct A11yTree {
    pub root: A11yNode,
    pub node_count: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone)]
pub struct A11yNode {
    pub role: String,
    pub name: String,
    pub value: Option<String>,
    pub bbox: Option<Rect>,
    pub enabled: bool,
    pub focused: bool,
    pub children: Vec<A11yNode>,
}

#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone)]
pub enum Action {
    ReadOnly,
    /// 中键单击（关闭标签页 / 新标签页打开链接等场景）
    MiddleClick { x: f64, y: f64 },
    /// 纯悬停移动（触发 hover 菜单 / tooltip，不点击）
    Hover { x: f64, y: f64 },
    /// 滚轮滚动（clicks 正=上/右、负=下/左；horizontal=true 时为水平滚动）
    WheelScroll { clicks: i32, horizontal: bool },
    /// 在屏幕坐标点击鼠标左键
    ClickAt { x: f64, y: f64 },
    /// 双击鼠标左键
    DoubleClick { x: f64, y: f64 },
    /// 右键点击
    RightClick { x: f64, y: f64 },
    /// 拖拽（左键按下 → 移动 → 释放）
    Drag {
        from_x: f64,
        from_y: f64,
        to_x: f64,
        to_y: f64,
    },
    /// 键盘输入文本
    TypeText { text: String },
    /// 键盘组合键（如 "ctrl+s"、"alt+tab"）
    KeyPress { keys: String },
    /// 按住某个键不放（需配合 KeyUp 抬起；支持修饰键/字母/数字/F键/方向键/小键盘/媒体键）
    KeyDown { key: String },
    /// 抬起某个键（配合 KeyDown 使用）
    KeyUp { key: String },
    /// 通过系统剪贴板 + Ctrl+V 粘贴文本（适配中文/emoji/大段文本）
    PasteText { text: String },
    /// 最小化所有顶层窗口（except 为标题关键词列表，命中的窗口保持不被最小化）
    WindowMinimizeAll { except: Vec<String> },
    /// 将指定名称窗口置顶 / 取消置顶
    WindowSetTopmost { name: String, topmost: bool },
    /// 聚焦并前置指定名称窗口（模糊匹配标题，最小化则先还原）
    WindowFocus { name: String },
    /// 一键清屏准备：聚焦目标窗口（验证前台切换）+ 按需置顶（验证）+ 最小化其余无关窗口
    WindowPrepare { name: String, topmost: bool },
}

#[derive(Debug, Clone)]
pub struct ActionResult {
    pub ok: bool,
    pub description: String,
}

#[derive(Debug, Clone, Default)]
pub struct CapabilitySet {
    pub a11y: bool,
    pub screenshot: bool,
    pub input: bool,
}

#[derive(Debug)]
pub enum CapError {
    Unsupported(&'static str),
    InvalidState(String),
    NotFound(String),
}

impl std::fmt::Display for CapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CapError::Unsupported(s) => write!(f, "不支持: {s}"),
            CapError::InvalidState(s) => write!(f, "{s}"),
            CapError::NotFound(s) => write!(f, "{s}"),
        }
    }
}
impl std::error::Error for CapError {}

impl Observation {
    /// 把无障碍树转成模型可读的文本（缩进层级）
    pub fn to_text(&self) -> String {
        match &self.tree {
            Some(t) => format!(
                "[source={}] 无障碍树（{} 节点{}）:\n{}",
                self.source,
                t.node_count,
                if t.truncated { "，已截断" } else { "" },
                node_text(&t.root, 0)
            ),
            None => format!("[source={}] 未获取到无障碍树", self.source),
        }
    }
}

fn node_text(node: &A11yNode, depth: usize) -> String {
    let mut s = String::new();
    let indent = "  ".repeat(depth);
    let name = if node.name.is_empty() {
        String::new()
    } else {
        format!(" name='{}'", node.name)
    };
    let value = node
        .value
        .as_ref()
        .map(|v| format!(" value='{}'", v))
        .unwrap_or_default();
    let bbox = node
        .bbox
        .map(|r| {
            format!(
                " bbox=({},{},{},{})",
                r.x as i32, r.y as i32, r.width as i32, r.height as i32
            )
        })
        .unwrap_or_default();
    let state = if !node.enabled { " [禁用]" } else { "" };
    let focus = if node.focused { " [聚焦]" } else { "" };
    s.push_str(&format!(
        "{}[{}]{}{}{}{}{}\n",
        indent, node.role, name, value, bbox, state, focus
    ));
    for c in &node.children {
        s.push_str(&node_text(c, depth + 1));
    }
    s
}

/// 三级接地编排：语义目标 → 可点击坐标。
/// 一级：a11y 无障碍树定位；二级：截图 + 本地 OCR 文字定位；
/// 三级：Set-of-Marks 视觉标注选号；四级：截图 + 视觉模型猜坐标（仅无文字时走）。
/// 所有截图内坐标统一加上所在显示器的物理偏移，保证与 SendInput 绝对坐标对齐。
pub fn ground(capability: &dyn Capability, target: &str) -> Result<Rect, CapError> {
    let info = capability.capture_screen()?;
    ground_on_screenshot(capability, target, &info)
}

/// 在已截取的截图上接地（供点击者的前后截图复用，避免重复截屏）；
/// `info` 携带截图路径与显示器物理偏移。
///
/// 跨级评分仲裁：a11y 与 OCR 各自产出「带分候选」，只有高分一级才短路，
/// 低分 a11y 命中（如仅字符重叠的模糊命中）会被更精确的 OCR 命中取代。
pub fn ground_on_screenshot(
    capability: &dyn Capability,
    target: &str,
    info: &ScreenshotInfo,
) -> Result<Rect, CapError> {
    // 一级：a11y 语义查找（无需截图；find 已按分数降序）
    let mut a11y_best: Option<(u32, Rect)> = None;
    if let Ok(matches) = capability.find(target) {
        for m in matches {
            if let Some(bbox) = m.bbox {
                a11y_best = Some((m.score, bbox));
                break;
            }
        }
    }
    // a11y 高置信命中（精确/前缀/包含 + 角色加权）直接采用，省一次 OCR
    if let Some((s, r)) = &a11y_best {
        if *s >= 80 {
            return Ok(*r);
        }
    }

    let (ox, oy) = (info.offset_x as f64, info.offset_y as f64);
    let words = crate::ocr::ocr_detect_gui(&info.path)
        .map(|(_t, w)| w)
        .unwrap_or_default();

    // 二级：OCR 文字定位——逐词评分取最优（不再首个包含即命中）
    let mut ocr_best: Option<(u32, Rect)> = None;
    for w in &words {
        let s = w["text"].as_str().unwrap_or("").trim();
        if s.is_empty() {
            continue;
        }
        let sc = match_score(s, "", target);
        if sc == 0 {
            continue;
        }
        let r = Rect {
            x: w["x"].as_f64().unwrap_or(0.0) + ox,
            y: w["y"].as_f64().unwrap_or(0.0) + oy,
            width: w["w"].as_f64().unwrap_or(0.0),
            height: w["h"].as_f64().unwrap_or(0.0),
        };
        if ocr_best.as_ref().map_or(true, |(b, _)| sc > *b) {
            ocr_best = Some((sc, r));
        }
    }

    // 仲裁：两边都有候选时取分高者（同分偏向 a11y，语义来源更稳）
    let arbitrated = match (a11y_best, ocr_best) {
        (Some((a, ar)), Some((o, or))) => Some(if o > a + 10 { or } else { ar }),
        (Some((_, ar)), None) => Some(ar),
        (None, Some((_, or))) => Some(or),
        (None, None) => None,
    };
    if let Some(r) = arbitrated {
        return Ok(r);
    }

    if !words.is_empty() {
        // 三级：Set-of-Marks 视觉标注选号（有文字候选时唯一走视觉模型的路径）
        let candidates: Vec<(i32, i32, i32, i32)> = words
            .iter()
            .filter_map(|w| {
                let x = w["x"].as_i64()? as i32;
                let y = w["y"].as_i64()? as i32;
                let ww = w["w"].as_i64()? as i32;
                let hh = w["h"].as_i64()? as i32;
                if ww > 2 && hh > 2 {
                    Some((x, y, ww, hh))
                } else {
                    None
                }
            })
            .take(30)
            .collect();
        if let Ok((ann_path, centers)) = crate::som::annotate(&info.path, &candidates) {
            if let Some(idx) = crate::som::som_select(&ann_path, target, centers.len()) {
                if let Some(&(cx, cy)) = centers.get(idx) {
                    return Ok(Rect {
                        x: cx + ox,
                        y: cy + oy,
                        width: 0.0,
                        height: 0.0,
                    });
                }
            }
        }
        // SOM 未命中不短路：视觉模型（Ollama）可能未部署/选号模型分辨率不足导致失败，
        // 落入下方 visual_locate 统一兜底，尽量提高命中率而非直接放弃。
    }

    // 兜底：视觉模型直接定位（无文字界面，或 SOM 选号未命中时均走此处）
    if let Ok((cx, cy)) = crate::visual_grounding::visual_locate(&info.path, target) {
        return Ok(Rect {
            x: cx + ox,
            y: cy + oy,
            width: 0.0,
            height: 0.0,
        });
    }

    Err(CapError::NotFound(format!("未找到目标: {target}")))
}

// ───────────── 语义目标 ↔ 候选文本 匹配评分（GUI 定位准确性核心） ─────────────
//
// 设计目标：把「目标描述 vs 控件名/OCR 词」从朴素 contains 升级为可比较的分数：
//   - 归一化：忽略大小写、空白、下划线、连字符（英文按钮常见「Search »」「OK_2」等形态）
//   - 类型后缀剥离：「搜索按钮/输入框」剥掉类型词后与「搜索」精确相等（OCR 词通常不带类型词）
//   - 角色加权：目标里写明「按钮/输入框/下拉」等类型词时，命中对应控件角色加分
//   - 字符重叠率兜底：以上都未命中时，按目标字符在候选中的命中率给模糊分（阈值 70%）

/// 归一化：小写 + 去空白/下划线/连字符
pub fn normalize_target(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && *c != '_' && *c != '-')
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// 剥离目标描述中的控件类型词（「搜索按钮」→「搜索」）
fn strip_kind_words(t: &str) -> String {
    let mut s = t.to_string();
    for w in [
        "输入框", "下拉框", "复选框", "单选框", "搜索框", "文本框", "编辑框", "选项卡", "按钮",
        "菜单", "链接", "列表", "标签", "图标", "选项", "框", "键",
    ] {
        s = s.replace(w, "");
    }
    s
}

/// 目标中的类型词 → 控件角色加分（UIA ControlType 的 Debug 名，如 Button/Edit）
fn role_bonus(t: &str, role: &str) -> u32 {
    let r = role.to_lowercase();
    let hinted_roles: &[(&str, &[&str])] = &[
        ("按钮", &["button"]),
        ("下拉", &["combobox"]),
        ("输入", &["edit", "combobox", "spinner", "document"]),
        ("链接", &["hyperlink"]),
        ("勾选", &["checkbox"]),
        ("复选", &["checkbox"]),
        ("单选", &["radiobutton"]),
        ("菜单", &["menu"]),
        ("标签", &["tabitem", "tab"]),
        ("选项卡", &["tabitem", "tab"]),
        ("列表", &["list", "listitem"]),
        ("滑块", &["slider"]),
        ("进度", &["progressbar"]),
    ];
    let mut bonus = 0;
    for (kw, roles) in hinted_roles {
        if t.contains(kw) && roles.iter().any(|x| r.contains(x)) {
            bonus += 10;
        }
    }
    bonus
}

/// 语义目标 ↔ 候选名称 匹配评分（0-120）。0 = 不匹配。
/// `role` 传控件角色（UIA ControlType Debug 名）；OCR 词等无角色场景传 ""
pub fn match_score(name: &str, role: &str, target: &str) -> u32 {
    let n = normalize_target(name);
    let t = normalize_target(target);
    if n.is_empty() || t.is_empty() {
        return 0;
    }
    let t_bare = strip_kind_words(&t);
    let mut score = if n == t {
        100
    } else if n.starts_with(&t) {
        92
    } else if n.contains(&t) {
        84
    } else if !t_bare.is_empty() && (n == t_bare || n.contains(&t_bare)) {
        72
    } else {
        // 字符重叠率模糊匹配：目标字符在候选名中的命中率 ≥70% 才给分
        let total = t.chars().count() as u32;
        let hits = t.chars().filter(|c| n.contains(*c)).count() as u32;
        let ratio = hits * 100 / total.max(1);
        if ratio >= 70 {
            40 + ratio / 5
        } else {
            0
        }
    };
    if !role.is_empty() {
        score += role_bonus(&t, role);
    }
    score.min(120)
}

/// 平台工厂：返回当前平台的 Capability 实现

/// Capability 可用的全局 AppHandle（setup 时注入；光圈事件需要它发事件）
static CAP_APP: OnceLock<AppHandle> = OnceLock::new();

#[cfg(test)]
mod score_tests {
    use super::match_score;

    #[test]
    fn 精确与归一化匹配() {
        assert!(match_score("设置", "", "设置") >= 100);
        // 大小写/空白/符号归一化
        assert!(match_score("Search »", "Button", "search") >= 90);
        assert!(match_score("OK_2", "", "ok") >= 90);
    }

    #[test]
    fn 类型后缀剥离() {
        // 「搜索按钮」剥掉「按钮」后与 OCR 词「搜索」精确相等
        assert!(match_score("搜索", "", "搜索按钮") >= 70);
        // 「用户名输入框」剥掉「输入框」后包含于「用户名：」场景
        assert!(match_score("用户名", "Edit", "用户名输入框") >= 70);
    }

    #[test]
    fn 角色加权() {
        // 同名候选，目标写明「按钮」时 Button 角色得分更高
        let btn = match_score("发送", "Button", "发送按钮");
        let edit = match_score("发送", "Edit", "发送按钮");
        assert!(btn > edit);
    }

    #[test]
    fn 模糊兜底与拒绝() {
        // 字符重叠率达标给模糊分
        assert!(match_score("开始使用向导", "", "使用向导") > 0);
        // 重叠率不足直接判不匹配
        assert_eq!(match_score("取消", "", "打开文件"), 0);
    }
}

/// setup 阶段注入 AppHandle（在 create_capability 之后调用亦生效）
pub fn init_capability_app(app: AppHandle) {
    let _ = CAP_APP.set(app);
}

#[cfg(windows)]
pub fn create_capability() -> Arc<dyn Capability> {
    Arc::new(windows::WindowsCapability::new(CAP_APP.get().cloned()))
}

#[cfg(not(windows))]
pub fn create_capability() -> Arc<dyn Capability> {
    Arc::new(stub::StubCapability)
}

// ---------------- 只读 Computer Use 工具 ----------------

/// 只读「看屏」工具：读取前台窗口无障碍树

/// ui_analyze：一次性枚举目标应用的可交互元素（按钮/输入框/链接/菜单…名称+坐标）。
/// 「先分析 → 一轮批量派发」两阶段 GUI 模式的分析步，显著减少逐步观察的模型往返。

/// gui_undo：逆操作映射回退（回退原则第 3 级）。逆映射表：
///   - 文本输入（type_text/paste_text）→ 全选+删除清空
///   - 点击（click_element/click_at/mouse_click，非破坏性目标）→ 再点一次（开关类）
///   - 导航/按键 → 发送 Alt+Left 返回上一界面
///   - 文件操作（write_file/edit_file/move_file）→ 字节级快照还原（tools::undo_last_step）
/// 不可逆操作（删除/发送类、无对应逆映射的）如实标注，不假装回滚。
pub struct GuiUndoTool {
    capability: Arc<dyn Capability>,
    click: ClickElementTool,
}

impl GuiUndoTool {
    pub fn new(capability: Arc<dyn Capability>) -> Self {
        Self {
            click: ClickElementTool::new(capability.clone()),
            capability,
        }
    }
}

/// 破坏性关键词：逆操作同样不得触碰不可逆目标
const GUI_UNDO_DESTRUCTIVE: &[&str] = &[
    "删除", "清空", "格式化", "卸载", "发送", "注销", "关机", "移除", "抹掉", "重置",
];

impl Tool for GuiUndoTool {
    fn name(&self) -> &str {
        "gui_undo"
    }
    fn description(&self) -> &str {
        "回退最近的 GUI 操作（逆操作映射）。action 可选：list=查看最近操作记录；last=回退最近一次文本输入（全选删除清空，默认）；toggle=把最近一次点击再点一次（适用于开关/复选/展开收起类）；back=发送 Alt+Left 返回上一界面（撤销导航类操作）；file=撤销最近一次文件写/改/移动（字节级快照还原）；steps=按逆序回退最近 n 步中所有可逆操作（文本输入清空，其余标注跳过）。删除/发送等不可逆操作无法回退，会如实列出操作记录并标注"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["last", "list", "toggle", "back", "file", "steps"], "description": "last=清空最近文本输入（默认）；list=查看记录；toggle=重击最近一次点击（开关类）；back=Alt+Left 返回；file=撤销文件操作；steps=逆序回退最近 n 步可逆操作" },
                "n": { "type": "integer", "description": "steps 模式的回退步数（1-10，默认 3）" }
            }
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let action = args["action"].as_str().unwrap_or("last").to_string();
        let ops = crate::replay::last_gui_ops(20);
        let journal: Vec<Value> = ops
            .iter()
            .map(|(t, d)| json!({ "tool": t, "detail": d }))
            .collect();

        if action == "list" {
            return Ok(json!({ "ok": true, "journal": journal }));
        }

        if action == "back" {
            // 导航类逆操作：Alt+Left（资源管理器/浏览器/多数应用通用「返回」）
            self.capability
                .act(&Action::KeyPress {
                    keys: "alt+left".into(),
                })
                .map_err(|e| format!("发送返回快捷键失败: {e}"))?;
            return Ok(json!({
                "ok": true,
                "undone": "导航",
                "note": "已发送 Alt+Left 返回上一界面。请截屏确认是否回到了预期位置"
            }));
        }

        if action == "file" {
            // 文件类逆操作：复用字节级快照撤销栈
            match crate::tools::undo_last_step() {
                Ok(desc) => return Ok(json!({ "ok": true, "undone": desc })),
                Err(e) => {
                    return Ok(json!({
                        "ok": false,
                        "message": format!("文件撤销失败: {e}"),
                        "journal": journal
                    }))
                }
            }
        }

        if action == "toggle" {
            // 开关类逆操作：把最近一次点击再点一次（ops 最新在前，取第一条命中的）
            let last_click = ops.iter().find(|(t, _)| {
                matches!(t.as_str(), "click_element" | "click_at" | "mouse_click")
            });
            let Some((tool, detail)) = last_click else {
                return Ok(json!({
                    "ok": false, "journal": journal,
                    "message": "操作记录里没有可重击的点击操作"
                }));
            };
            let args_text = detail.to_string();
            if GUI_UNDO_DESTRUCTIVE.iter().any(|w| args_text.contains(w)) {
                return Ok(json!({
                    "ok": false, "journal": journal,
                    "message": "最近一次点击目标含破坏性关键词（删除/清空/发送等），重击不可逆且危险，已拒绝。请用应用自身功能恢复"
                }));
            }
            let parsed: Value = serde_json::from_str(&args_text).unwrap_or(json!({}));
            let output = match tool.as_str() {
                "click_element" => self.click.run(parsed)?,
                "click_at" | "mouse_click" => {
                    let x = parsed["x"].as_f64();
                    let y = parsed["y"].as_f64();
                    let (Some(x), Some(y)) = (x, y) else {
                        return Ok(json!({
                            "ok": false, "journal": journal,
                            "message": "最近一次坐标点击缺少 x/y 参数，无法重击"
                        }));
                    };
                    self.capability
                        .act(&Action::ClickAt { x, y })
                        .map_err(|e| format!("重击点击失败: {e}"))?;
                    json!({ "ok": true })
                }
                _ => json!({ "error": "该操作类型不支持重击" }),
            };
            let ok = output.get("error").is_none()
                && output.get("ok").and_then(|v| v.as_bool()).unwrap_or(true);
            return Ok(json!({
                "ok": ok,
                "undone": { "tool": tool, "detail": detail, "via": "toggle-重新点击" },
                "note": "已对同一目标再次点击（适用于开关/复选/展开收起类）。请截屏确认状态已翻转",
                "result": output
            }));
        }

        if action == "steps" {
            // 逆序回退最近 n 步中所有可逆操作（ops 最新在前：从最新开始往旧回退）
            let n = args["n"].as_u64().unwrap_or(3).clamp(1, 10) as usize;
            let target: Vec<&(String, String)> = ops.iter().take(n).collect();
            let mut undone: Vec<Value> = Vec::new();
            let mut skipped: Vec<Value> = Vec::new();
            for (t, d) in target {
                if matches!(t.as_str(), "type_text" | "paste_text") {
                    self.capability
                        .act(&Action::KeyPress {
                            keys: "ctrl+a".into(),
                        })
                        .map_err(|e| format!("发送全选快捷键失败: {e}"))?;
                    self.capability
                        .act(&Action::KeyPress {
                            keys: "delete".into(),
                        })
                        .or_else(|_| self.capability.act(&Action::KeyPress { keys: "backspace".into() }))
                        .map_err(|e| format!("发送删除键失败: {e}"))?;
                    undone.push(json!({ "tool": t, "detail": d }));
                } else if matches!(
                    t.as_str(),
                    "click_element" | "click_at" | "mouse_click"
                ) && !GUI_UNDO_DESTRUCTIVE.iter().any(|w| d.contains(w))
                {
                    undone.push(json!({ "tool": t, "detail": d, "via": "toggle-重新点击" }));
                    let parsed: Value = serde_json::from_str(d).unwrap_or(json!({}));
                    match t.as_str() {
                        "click_element" => {
                            let _ = self.click.run(parsed)?;
                        }
                        _ => {
                            if let (Some(x), Some(y)) =
                                (parsed["x"].as_f64(), parsed["y"].as_f64())
                            {
                                let _ = self.capability.act(&Action::ClickAt { x, y });
                            }
                        }
                    }
                } else {
                    skipped.push(json!({ "tool": t, "detail": d, "reason": "不可逆或无逆映射" }));
                }
            }
            return Ok(json!({
                "ok": true,
                "undone": undone,
                "skipped": skipped,
                "note": "回退完成。文本输入已清空、可重击的点击已翻转；跳过项请用应用自身功能（Ctrl+Z/返回）恢复"
            }));
        }

        // action=last：回退最近一次文本输入
        let last_text = ops
            .iter()
            .find(|(t, _)| t == "type_text" || t == "paste_text");
        let Some((tool, detail)) = last_text else {
            return Ok(json!({
                "ok": false,
                "irreversible": true,
                "journal": journal,
                "message": "最近的操作不含可自动回退的文本输入。可用 action=toggle 重击最近点击、action=back 返回上一界面、action=file 撤销文件操作，或用应用自身的撤销（Ctrl+Z）恢复"
            }));
        };

        self.capability
            .act(&Action::KeyPress { keys: "ctrl+a".into() })
            .map_err(|e| format!("发送全选快捷键失败: {e}"))?;
        let del = self
            .capability
            .act(&Action::KeyPress { keys: "delete".into() })
            .or_else(|_| self.capability.act(&Action::KeyPress { keys: "backspace".into() }));
        del.map_err(|e| format!("发送删除键失败: {e}"))?;

        Ok(json!({
            "ok": true,
            "undone": { "tool": tool, "detail": detail },
            "note": "已对当前聚焦输入框执行全选+删除。若焦点已不在原输入框，请立即检查该窗口内容"
        }))
    }
}


pub struct UiAnalyzeTool {
    capability: Arc<dyn Capability>,
}

impl UiAnalyzeTool {
    pub fn new(capability: Arc<dyn Capability>) -> Self {
        Self { capability }
    }
}

impl Tool for UiAnalyzeTool {
    fn name(&self) -> &str {
        "ui_analyze"
    }
    fn description(&self) -> &str {
        "一次性获取目标应用的可交互元素地图（按钮/输入框/链接/菜单的名称、类型与屏幕坐标）。用于 GUI 操作前分析应用结构：先调用本工具拿到元素清单，再把整套操作（click_element/type_text/close_popup…）作为多个工具调用在同一轮内按顺序批量派发，最后放一次 capture_screen 验证——比每步单独观察快数倍。window 传目标窗口标题关键词（缺省为当前焦点应用窗口）。只读，无需授权"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "window": { "type": "string", "description": "目标窗口标题关键词（可选，缺省为当前焦点应用）" }
            }
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let window = args["window"]
            .as_str()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        self.capability
            .interactive_map(window)
            .map_err(|e| e.to_string())
    }
}


pub struct ReadScreenTool {
    capability: Arc<dyn Capability>,
}

impl ReadScreenTool {
    pub fn new(capability: Arc<dyn Capability>) -> Self {
        Self { capability }
    }
}

impl Tool for ReadScreenTool {
    fn name(&self) -> &str {
        "read_screen"
    }
    fn description(&self) -> &str {
        "读取当前前台窗口的无障碍树，了解界面上有哪些控件及其名称/角色/位置（只读，不进行任何操作）"
    }
    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, _args: Value) -> Result<Value, String> {
        let obs = self
            .capability
            .observe(&ObserveReq::default())
            .map_err(|e| e.to_string())?;
        Ok(json!({ "text": obs.to_text() }))
    }
}

/// 窗口枚举工具
pub struct ListWindowsTool {
    capability: Arc<dyn Capability>,
}

impl ListWindowsTool {
    pub fn new(capability: Arc<dyn Capability>) -> Self {
        Self { capability }
    }
}

impl Tool for ListWindowsTool {
    fn name(&self) -> &str {
        "list_windows"
    }
    fn description(&self) -> &str {
        "枚举当前桌面上的顶层窗口（名称/角色/位置），了解打开了哪些应用（只读）。对自渲染窗口（Electron/DirectUI/Qt/自绘安装器等）UIA 读不到内部控件，可传 with_preview=true 对每个窗口做一次截屏 OCR，把窗口内文字/按钮塞进 preview 字段，一眼看清弹窗里有什么可点"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "with_preview": {
                    "type": "boolean",
                    "description": "是否对每个窗口做 OCR 内景预览（默认 false）。开启后会对屏幕截一次并识别文字，按窗口位置归并到各窗口的 preview 字段"
                }
            }
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let with_preview = args["with_preview"].as_bool() == Some(true);
        let wins = self.capability.list_windows().map_err(|e| e.to_string())?;

        // 可选内景预览：复用一次全屏截图 + OCR，把文字按「中心点是否落在窗口矩形内」
        // 归并到对应窗口，让自渲染窗口（无障碍树为空）也能看清有哪些按钮/文本。
        let mut ocr_words: Vec<(f64, f64, f64, f64, String)> = Vec::new();
        let mut offset = (0.0f64, 0.0f64);
        if with_preview {
            if let Ok(info) = self.capability.capture_screen() {
                offset = (info.offset_x as f64, info.offset_y as f64);
                if let Ok((_, words)) = crate::ocr::ocr_detect_gui(&info.path) {
                    for w in words {
                        let text = w["text"].as_str().unwrap_or("").trim().to_string();
                        if text.is_empty() {
                            continue;
                        }
                        let x = w["x"].as_f64().unwrap_or(0.0);
                        let y = w["y"].as_f64().unwrap_or(0.0);
                        let ww = w["w"].as_f64().unwrap_or(0.0);
                        let hh = w["h"].as_f64().unwrap_or(0.0);
                        ocr_words.push((x, y, ww, hh, text));
                    }
                }
            }
        }

        let arr: Vec<Value> = wins
            .into_iter()
            .map(|win| {
                let mut obj = json!({
                    "name": win.name,
                    "role": win.role,
                    "class": win.class,
                    "bbox": win.bbox.map(|r| format!("{},{},{},{}",
                        r.x as i32, r.y as i32, r.width as i32, r.height as i32)),
                });
                if with_preview {
                    let mut preview = String::new();
                    if let Some(b) = &win.bbox {
                        let mut texts: Vec<String> = Vec::new();
                        for (x, y, ww, hh, text) in &ocr_words {
                            let cx = x + ww / 2.0 + offset.0;
                            let cy = y + hh / 2.0 + offset.1;
                            if cx >= b.x
                                && cx <= b.x + b.width
                                && cy >= b.y
                                && cy <= b.y + b.height
                            {
                                texts.push(text.clone());
                            }
                        }
                        preview = texts.join(" | ");
                        // 控制长度，避免超长 OCR 噪音撑爆上下文
                        if preview.chars().count() > 400 {
                            preview = preview.chars().take(400).collect::<String>() + "…";
                        }
                    }
                    obj["preview"] = json!(preview);
                }
                obj
            })
            .collect();
        Ok(json!(arr))
    }
}

/// 读指定窗口工具
pub struct ReadWindowTool {
    capability: Arc<dyn Capability>,
}

impl ReadWindowTool {
    pub fn new(capability: Arc<dyn Capability>) -> Self {
        Self { capability }
    }
}

impl Tool for ReadWindowTool {
    fn name(&self) -> &str {
        "read_window"
    }
    fn description(&self) -> &str {
        "读取指定名称窗口的无障碍树（先用 list_windows 获取窗口名，支持模糊匹配）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "窗口名（模糊匹配）" }
            },
            "required": ["name"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let name = args["name"].as_str().ok_or("缺少参数 name")?;
        let req = ObserveReq {
            window: Some(WindowTarget::ByName(name.to_string())),
            ..ObserveReq::default()
        };
        let obs = self.capability.observe(&req).map_err(|e| e.to_string())?;
        Ok(json!({ "text": obs.to_text() }))
    }
}

/// 截屏工具（像素感知通道，为视觉 grounding 打基础）
pub struct CaptureScreenTool {
    capability: Arc<dyn Capability>,
}

impl CaptureScreenTool {
    pub fn new(capability: Arc<dyn Capability>) -> Self {
        Self { capability }
    }
}

impl Tool for CaptureScreenTool {
    fn name(&self) -> &str {
        "capture_screen"
    }
    fn description(&self) -> &str {
        "截取当前屏幕并保存为 PNG，返回路径与尺寸；可选 ocr=true 同步提取画面文字（本地 Tesseract），或用 question 让本地视觉模型描述/回答截图内容——均不入网"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "ocr": { "type": "boolean", "description": "是否同时做 OCR 文字识别（默认 false）；true 时返回 ocr_text 与 ocr_words 坐标" },
                "lang": { "type": "string", "description": "OCR 语言，默认 chi_sim+eng" },
                "question": { "type": "string", "description": "可选：用本地视觉模型回答关于截图内容的问题（如「截图里有什么按钮」），返回 vision 字段" }
            }
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let info = self.capability.capture_screen().map_err(|e| e.to_string())?;
        let path = info.path;
        let (width, height) = (info.width, info.height);

        let mut out = json!({
            "path": path,
            "width": width,
            "height": height,
        });

        if args["ocr"].as_bool().unwrap_or(false) {
            // 双引擎快速 OCR（Windows.Media.Ocr 优先，~0.5s；旧 Tesseract 路径 8-11s 已弃用为回退）
            match crate::ocr::ocr_detect_gui(&path) {
                Ok((text, words)) => {
                    out["ocr_text"] = Value::String(text);
                    out["ocr_words"] = Value::Array(words);
                }
                Err(e) => {
                    out["ocr_error"] = Value::String(e);
                }
            }
        }

        if let Some(q) = args["question"].as_str() {
            if !q.trim().is_empty() {
                match crate::visual_grounding::describe_image(&path, q.trim()) {
                    Ok(desc) => {
                        out["vision"] = Value::String(desc);
                    }
                    Err(e) => {
                        out["vision_error"] = Value::String(e);
                    }
                }
            }
        }

        Ok(out)
    }
}

/// 全屏元素标注工具：OCR 文字行 + UIA 可交互控件一次性汇总，供批量规划 GUI 操作
pub struct ScreenElementsTool {
    capability: Arc<dyn Capability>,
}

impl ScreenElementsTool {
    pub fn new(capability: Arc<dyn Capability>) -> Self {
        Self { capability }
    }
}

impl Tool for ScreenElementsTool {
    fn name(&self) -> &str {
        "screen_elements"
    }
    fn description(&self) -> &str {
        "一键提取当前屏幕的全部可操作元素并返回坐标（只读）：UIA 可交互控件（按钮/输入框/列表项，含名称与类型）+ OCR 文字行（自动把相邻词合并成完整文本行，含中心坐标）。坐标为屏幕物理像素，可直接喂给 mouse_click / click_at 批量派发操作，无需逐个 find_element。规划 GUI 步骤时优先用它一张截图看清全局面板"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "window": { "type": "string", "description": "可选：只分析指定标题关键词窗口的 UIA 控件（OCR 仍为全屏）；留空取前台窗口" },
                "max": { "type": "number", "description": "最多返回元素数，默认 80" }
            }
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let max = args["max"].as_u64().unwrap_or(80).clamp(10, 200) as usize;
        let info = self.capability.capture_screen().map_err(|e| e.to_string())?;
        let (ox, oy) = (info.offset_x as f64, info.offset_y as f64);
        let mut elements: Vec<Value> = Vec::new();

        // 一级：UIA 可交互控件（名称/类型/中心坐标）——有树的应用直接给结构化元素
        let win = args["window"].as_str().map(|s| s.to_string());
        if let Ok(map) = self.capability.interactive_map(win) {
            if let Some(arr) = map["elements"].as_array() {
                for e in arr {
                    let (x, y) = (
                        e["x"].as_f64().unwrap_or(0.0) + ox,
                        e["y"].as_f64().unwrap_or(0.0) + oy,
                    );
                    if x <= ox && y <= oy {
                        continue;
                    }
                    elements.push(json!({
                        "source": "uia",
                        "text": e["name"],
                        "type": e["type"],
                        "x": x as i32, "y": y as i32,
                        "cx": x as i32, "cy": y as i32,
                    }));
                }
            }
        }

        // 二级：OCR 文字行（双引擎快速 OCR，词合并成行）
        if let Ok((_, words)) = crate::ocr::ocr_detect_gui(&info.path) {
            for line in cluster_ocr_lines(&words) {
                elements.push(json!({
                    "source": "ocr",
                    "text": line.text,
                    "x": line.x + ox as i32,
                    "y": line.y + oy as i32,
                    "w": line.w,
                    "h": line.h,
                    "cx": line.x + line.w / 2 + ox as i32,
                    "cy": line.y + line.h / 2 + oy as i32,
                }));
            }
        }

        // 去重：OCR 行与 UIA 控件文本相同且中心距离 < 24px 时只留 UIA（结构化信息更准）
        let uia_refs: Vec<(String, i64, i64)> = elements
            .iter()
            .filter(|e| e["source"] == "uia")
            .map(|e| {
                (
                    e["text"].as_str().unwrap_or("").to_string(),
                    e["cx"].as_i64().unwrap_or(9999),
                    e["cy"].as_i64().unwrap_or(9999),
                )
            })
            .collect();
        elements.retain(|e| {
            if e["source"] != "ocr" {
                return true;
            }
            let (ex, ey) = (e["cx"].as_i64().unwrap_or(0), e["cy"].as_i64().unwrap_or(0));
            let text = e["text"].as_str().unwrap_or("");
            !uia_refs.iter().any(|(t, ux, uy)| {
                t.contains(text) && (*ux - ex).abs() < 24 && (*uy - ey).abs() < 24
            })
        });
        // 自上而下排序，方便按界面顺序规划步骤
        elements.sort_by_key(|e| (e["cy"].as_i64().unwrap_or(0), e["cx"].as_i64().unwrap_or(0)));
        elements.truncate(max);

        Ok(json!({
            "path": info.path,
            "count": elements.len(),
            "elements": elements,
        }))
    }
}

/// OCR 词框聚类成文本行：y 重叠且水平间距近的词合并（兼容 Windows OCR 短语框与 Tesseract 词框）
fn cluster_ocr_lines(words: &[Value]) -> Vec<OcrLine> {
    #[derive(Clone)]
    struct W {
        text: String,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
    }
    let mut ws: Vec<W> = words
        .iter()
        .filter_map(|w| {
            let text = w["text"].as_str()?.trim().to_string();
            if text.is_empty() {
                return None;
            }
            Some(W {
                text,
                x: w["x"].as_i64()? as i32,
                y: w["y"].as_i64()? as i32,
                w: w["w"].as_i64()? as i32,
                h: w["h"].as_i64()? as i32,
            })
        })
        .filter(|w| w.w > 2 && w.h > 4)
        .collect();
    ws.sort_by_key(|w| (w.y, w.x));

    let mut lines: Vec<OcrLine> = Vec::new();
    for w in ws {
        // 寻找可容纳该词的行：词垂直中点落在行范围内，且水平间距小于行高 2 倍
        let cy = w.y + w.h / 2;
        let mut target: Option<usize> = None;
        for (i, ln) in lines.iter().enumerate() {
            let lcy = ln.y + ln.h / 2;
            let gap = (w.x - (ln.x + ln.w)).max(0);
            if (cy - lcy).abs() * 2 < ln.h.max(w.h) && gap < ln.h.max(w.h) * 2 {
                target = Some(i);
                break;
            }
        }
        match target {
            Some(i) => {
                let ln = &mut lines[i];
                let nx = ln.x.min(w.x);
                let ny = ln.y.min(w.y);
                let ne = (ln.x + ln.w).max(w.x + w.w);
                let nb = (ln.y + ln.h).max(w.y + w.h);
                ln.text.push_str(&w.text);
                ln.x = nx;
                ln.y = ny;
                ln.w = ne - nx;
                ln.h = nb - ny;
            }
            None => lines.push(OcrLine {
                text: w.text,
                x: w.x,
                y: w.y,
                w: w.w,
                h: w.h,
            }),
        }
    }
    lines.retain(|l| !l.text.is_empty());
    lines
}

/// 聚合后的 OCR 文本行（图片物理像素坐标）
struct OcrLine {
    text: String,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

/// 点击工具（写操作，需人工审批）
pub struct ClickAtTool {
    capability: Arc<dyn Capability>,
}

impl ClickAtTool {
    pub fn new(capability: Arc<dyn Capability>) -> Self {
        Self { capability }
    }
}

impl Tool for ClickAtTool {
    fn name(&self) -> &str {
        "click_at"
    }
    fn description(&self) -> &str {
        "在屏幕坐标 (x,y) 处点击鼠标左键（写操作，会请求授权）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "x": { "type": "number", "description": "屏幕 X 坐标" },
                "y": { "type": "number", "description": "屏幕 Y 坐标" }
            },
            "required": ["x", "y"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let x = args["x"].as_f64().ok_or("缺少参数 x")?;
        let y = args["y"].as_f64().ok_or("缺少参数 y")?;
        let res = self
            .capability
            .act(&Action::ClickAt { x, y })
            .map_err(|e| e.to_string())?;
        Ok(json!({ "ok": res.ok, "description": res.description }))
    }
}

/// 输入文本工具（写操作，需人工审批）
pub struct TypeTextTool {
    capability: Arc<dyn Capability>,
}

impl TypeTextTool {
    pub fn new(capability: Arc<dyn Capability>) -> Self {
        Self { capability }
    }
}

impl Tool for TypeTextTool {
    fn name(&self) -> &str {
        "type_text"
    }
    fn description(&self) -> &str {
        "向当前焦点输入文本（写操作，会请求授权）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "要输入的文本" }
            },
            "required": ["text"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let text = args["text"].as_str().ok_or("缺少参数 text")?;
        let res = self
            .capability
            .act(&Action::TypeText { text: text.to_string() })
            .map_err(|e| e.to_string())?;
        Ok(json!({ "ok": res.ok, "description": res.description }))
    }
}

/// 查找控件工具（只读）：按名称模糊查找，返回名称+位置
pub struct FindElementTool {
    capability: Arc<dyn Capability>,
}

impl FindElementTool {
    pub fn new(capability: Arc<dyn Capability>) -> Self {
        Self { capability }
    }
}

impl Tool for FindElementTool {
    fn name(&self) -> &str {
        "find_element"
    }
    fn description(&self) -> &str {
        "按名称模糊查找当前窗口中的控件，返回匹配项的名称和位置（只读）。注意：页面动画/加载期间坐标会漂移，界面刚变化时先等 1-2 秒再调用；同一目标多次调用结果不一致说明界面还在变化"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "控件名称（模糊匹配）" }
            },
            "required": ["name"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let name = args["name"].as_str().ok_or("缺少参数 name")?;
        let matches = self.capability.find(name).map_err(|e| e.to_string())?;
        let arr: Vec<Value> = matches
            .into_iter()
            .map(|m| {
                json!({
                    "name": m.name,
                    "bbox": m.bbox.map(|r| format!("{},{},{},{}",
                        r.x as i32, r.y as i32, r.width as i32, r.height as i32)),
                })
            })
            .collect();
        Ok(json!(arr))
    }
}

/// 语义点击工具（写操作）：按名称查找并点击控件
pub struct ClickElementTool {
    capability: Arc<dyn Capability>,
}

impl ClickElementTool {
    pub fn new(capability: Arc<dyn Capability>) -> Self {
        Self { capability }
    }
}

impl Tool for ClickElementTool {
    fn name(&self) -> &str {
        "click_element"
    }
    fn description(&self) -> &str {
        "按名称查找并点击当前窗口中的控件（写操作，会请求授权）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "控件名称（模糊匹配）" }
            },
            "required": ["name"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let name = args["name"].as_str().ok_or("缺少参数 name")?;
        let res = self.capability.click_element(name).map_err(|e| e.to_string())?;
        Ok(json!({ "ok": res.ok, "description": res.description }))
    }
}

/// 接地工具（只读）：语义目标 → 中心坐标（供 click_at 使用）
pub struct GroundElementTool {
    capability: Arc<dyn Capability>,
}

impl GroundElementTool {
    pub fn new(capability: Arc<dyn Capability>) -> Self {
        Self { capability }
    }
}

impl Tool for GroundElementTool {
    fn name(&self) -> &str {
        "ground_element"
    }
    fn description(&self) -> &str {
        "按名称定位控件并返回其中心坐标（三级接地一级，只读；配合 click_at 使用）。注意：页面动画/加载期间坐标会漂移，界面刚变化时先等 1-2 秒再调用"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "控件名称（模糊匹配）" }
            },
            "required": ["name"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let name = args["name"].as_str().ok_or("缺少参数 name")?;
        let rect = ground(self.capability.as_ref(), name).map_err(|e| e.to_string())?;
        let cx = rect.x + rect.width / 2.0;
        let cy = rect.y + rect.height / 2.0;
        Ok(json!({
            "x": cx,
            "y": cy,
            "bbox": format!(
                "{},{},{},{}",
                rect.x as i32, rect.y as i32, rect.width as i32, rect.height as i32
            ),
        }))
    }
}

// ---------------- P0「手」：鼠标/键盘写操作工具 ----------------

/// 统一鼠标点击工具（左键单击/双击、右键）
pub struct MouseClickTool {
    capability: Arc<dyn Capability>,
}

impl MouseClickTool {
    pub fn new(capability: Arc<dyn Capability>) -> Self {
        Self { capability }
    }
}

impl Tool for MouseClickTool {
    fn name(&self) -> &str {
        "mouse_click"
    }
    fn description(&self) -> &str {
        "在屏幕坐标 (x,y) 处点击鼠标（button: left/middle/right，count: 1 单击 / 2 双击；写操作，会请求授权）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "x": { "type": "number", "description": "屏幕 X 坐标" },
                "y": { "type": "number", "description": "屏幕 Y 坐标" },
                "button": { "type": "string", "enum": ["left", "middle", "right"], "description": "鼠标按键：left 单击/双击，middle 关闭标签页/新标签打开链接，right 上下文菜单，默认 left" },
                "count": { "type": "integer", "enum": [1, 2], "description": "点击次数（1 单击 / 2 双击，仅 left 有效），默认 1" }
            },
            "required": ["x", "y"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let x = args["x"].as_f64().ok_or("缺少参数 x")?;
        let y = args["y"].as_f64().ok_or("缺少参数 y")?;
        let button = args["button"].as_str().unwrap_or("left");
        let count = args["count"].as_u64().unwrap_or(1);

        let action = match (button, count) {
            ("left", 1) => Action::ClickAt { x, y },
            ("left", 2) => Action::DoubleClick { x, y },
            ("right", _) => Action::RightClick { x, y },
            ("middle", _) => Action::MiddleClick { x, y },
            _ => {
                return Err(
                    "不支持的组合（button 仅 left/middle/right，count 仅 1/2）".to_string()
                )
            }
        };
        let res = self.capability.act(&action).map_err(|e| e.to_string())?;
        Ok(json!({ "ok": res.ok, "description": res.description }))
    }
}

/// 滚轮滚动工具：垂直/水平滚动页面或列表（长页面导航的正规途径）
pub struct WheelScrollTool {
    capability: Arc<dyn Capability>,
}

impl WheelScrollTool {
    pub fn new(capability: Arc<dyn Capability>) -> Self {
        Self { capability }
    }
}

impl Tool for WheelScrollTool {
    fn name(&self) -> &str {
        "wheel_scroll"
    }
    fn description(&self) -> &str {
        "滚动鼠标滚轮（写操作，会请求授权）。clicks 为齿感格数：正数向上/向右、负数向下/向左，一格约为页面 1/3 屏；可先传 x/y 把光标移到目标区域再滚（部分应用只响应光标下的滚轮）。horizontal=true 时水平滚动"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "clicks": { "type": "integer", "description": "滚动格数：正=上/右，负=下/左，默认 3" },
                "horizontal": { "type": "boolean", "description": "true=水平滚动（左右），默认 false（垂直）" },
                "x": { "type": "number", "description": "可选：滚动前先把光标移动到该坐标（目标区域中心）" },
                "y": { "type": "number", "description": "可选：同上，与 x 成对使用" }
            }
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let clicks = args["clicks"].as_i64().unwrap_or(3).clamp(-50, 50) as i32;
        let horizontal = args["horizontal"].as_bool().unwrap_or(false);
        // 可选的预移动：滚轮事件只发给光标下的窗口
        if let (Some(x), Some(y)) = (args["x"].as_f64(), args["y"].as_f64()) {
            self.capability
                .act(&Action::Hover { x, y })
                .map_err(|e| e.to_string())?;
        }
        let res = self
            .capability
            .act(&Action::WheelScroll { clicks, horizontal })
            .map_err(|e| e.to_string())?;
        Ok(json!({ "ok": res.ok, "description": res.description }))
    }
}

/// 中键单击工具：关闭标签页 / 后台新标签打开链接
pub struct MiddleClickTool {
    capability: Arc<dyn Capability>,
}

impl MiddleClickTool {
    pub fn new(capability: Arc<dyn Capability>) -> Self {
        Self { capability }
    }
}

impl Tool for MiddleClickTool {
    fn name(&self) -> &str {
        "middle_click"
    }
    fn description(&self) -> &str {
        "在屏幕坐标 (x,y) 处点击鼠标中键（写操作，会请求授权）。典型用途：浏览器/编辑器里关闭标签页、后台新标签页打开链接"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "x": { "type": "number", "description": "屏幕 X 坐标" },
                "y": { "type": "number", "description": "屏幕 Y 坐标" }
            },
            "required": ["x", "y"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let x = args["x"].as_f64().ok_or("缺少参数 x")?;
        let y = args["y"].as_f64().ok_or("缺少参数 y")?;
        let res = self
            .capability
            .act(&Action::MiddleClick { x, y })
            .map_err(|e| e.to_string())?;
        Ok(json!({ "ok": res.ok, "description": res.description }))
    }
}

/// 悬停移动工具：把光标移到目标位置但不点击（展开悬停菜单、显示 tooltip）
pub struct HoverTool {
    capability: Arc<dyn Capability>,
}

impl HoverTool {
    pub fn new(capability: Arc<dyn Capability>) -> Self {
        Self { capability }
    }
}

impl Tool for HoverTool {
    fn name(&self) -> &str {
        "hover"
    }
    fn description(&self) -> &str {
        "把鼠标光标移动到屏幕坐标 (x,y) 并悬停（写操作，会请求授权）。用途：展开悬停才出现的菜单/子菜单、查看 tooltip、聚焦悬停热区。配合等待（如 key_press 无操作或截屏确认）让悬停效果渲染完成"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "x": { "type": "number", "description": "屏幕 X 坐标" },
                "y": { "type": "number", "description": "屏幕 Y 坐标" }
            },
            "required": ["x", "y"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let x = args["x"].as_f64().ok_or("缺少参数 x")?;
        let y = args["y"].as_f64().ok_or("缺少参数 y")?;
        let res = self
            .capability
            .act(&Action::Hover { x, y })
            .map_err(|e| e.to_string())?;
        Ok(json!({ "ok": res.ok, "description": res.description }))
    }
}

/// 鼠标拖拽工具（左键按下 → 移动 → 释放）
pub struct MouseDragTool {
    capability: Arc<dyn Capability>,
}

impl MouseDragTool {
    pub fn new(capability: Arc<dyn Capability>) -> Self {
        Self { capability }
    }
}

impl Tool for MouseDragTool {
    fn name(&self) -> &str {
        "mouse_drag"
    }
    fn description(&self) -> &str {
        "从 (from_x,from_y) 拖拽到 (to_x,to_y)（左键按住移动后释放，写操作，会请求授权）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "from_x": { "type": "number", "description": "起点屏幕 X 坐标" },
                "from_y": { "type": "number", "description": "起点屏幕 Y 坐标" },
                "to_x": { "type": "number", "description": "终点屏幕 X 坐标" },
                "to_y": { "type": "number", "description": "终点屏幕 Y 坐标" }
            },
            "required": ["from_x", "from_y", "to_x", "to_y"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let from_x = args["from_x"].as_f64().ok_or("缺少参数 from_x")?;
        let from_y = args["from_y"].as_f64().ok_or("缺少参数 from_y")?;
        let to_x = args["to_x"].as_f64().ok_or("缺少参数 to_x")?;
        let to_y = args["to_y"].as_f64().ok_or("缺少参数 to_y")?;
        let res = self
            .capability
            .act(&Action::Drag {
                from_x,
                from_y,
                to_x,
                to_y,
            })
            .map_err(|e| e.to_string())?;
        Ok(json!({ "ok": res.ok, "description": res.description }))
    }
}

/// 键盘组合键工具
pub struct KeyPressTool {
    capability: Arc<dyn Capability>,
}

impl KeyPressTool {
    pub fn new(capability: Arc<dyn Capability>) -> Self {
        Self { capability }
    }
}

impl Tool for KeyPressTool {
    fn name(&self) -> &str {
        "key_press"
    }
    fn description(&self) -> &str {
        "按下键盘组合键或单个键，如 ctrl+s、alt+tab、enter（写操作，会请求授权）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "keys": { "type": "string", "description": "组合键，用 + 连接，如 ctrl+s、alt+tab、ctrl+shift+esc" }
            },
            "required": ["keys"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let keys = args["keys"].as_str().ok_or("缺少参数 keys")?;
        let res = self
            .capability
            .act(&Action::KeyPress {
                keys: keys.to_string(),
            })
            .map_err(|e| e.to_string())?;
        Ok(json!({ "ok": res.ok, "description": res.description }))
    }
}

/// 按住某键工具（不抬起，配合 key_up 使用）
pub struct KeyDownTool {
    capability: Arc<dyn Capability>,
}

impl KeyDownTool {
    pub fn new(capability: Arc<dyn Capability>) -> Self {
        Self { capability }
    }
}

impl Tool for KeyDownTool {
    fn name(&self) -> &str {
        "key_down"
    }
    fn description(&self) -> &str {
        "按住某个键不放，需配合 key_up 抬起。支持修饰键(ctrl/shift/alt/win)、字母(a-z)、数字(0-9)、F1-F24、方向键(up/down/left/right)、home/end/page_up/page_down/insert/delete/enter/tab/esc、小键盘(numpad0-9/numpad_add等)、媒体键(volume_up/volume_down/volume_mute/media_play_pause等)。用于长按、按住修饰键配合鼠标多选等精细操作（写操作，会请求授权）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "key": { "type": "string", "description": "键名，如 ctrl、shift、alt、win、up、down、f5、volume_up、numpad1、a、enter" }
            },
            "required": ["key"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let key = args["key"].as_str().ok_or("缺少参数 key")?;
        let res = self
            .capability
            .act(&Action::KeyDown {
                key: key.to_string(),
            })
            .map_err(|e| e.to_string())?;
        Ok(json!({ "ok": res.ok, "description": res.description }))
    }
}

/// 抬起某键工具
pub struct KeyUpTool {
    capability: Arc<dyn Capability>,
}

impl KeyUpTool {
    pub fn new(capability: Arc<dyn Capability>) -> Self {
        Self { capability }
    }
}

impl Tool for KeyUpTool {
    fn name(&self) -> &str {
        "key_up"
    }
    fn description(&self) -> &str {
        "抬起之前用 key_down 按住的键。键名规则同 key_down（写操作，会请求授权）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "key": { "type": "string", "description": "键名，同 key_down" }
            },
            "required": ["key"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let key = args["key"].as_str().ok_or("缺少参数 key")?;
        let res = self
            .capability
            .act(&Action::KeyUp {
                key: key.to_string(),
            })
            .map_err(|e| e.to_string())?;
        Ok(json!({ "ok": res.ok, "description": res.description }))
    }
}

/// 剪贴板粘贴工具
pub struct PasteTextTool {
    capability: Arc<dyn Capability>,
}

impl PasteTextTool {
    pub fn new(capability: Arc<dyn Capability>) -> Self {
        Self { capability }
    }
}

impl Tool for PasteTextTool {
    fn name(&self) -> &str {
        "paste_text"
    }
    fn description(&self) -> &str {
        "把文本通过系统剪贴板粘贴到当前焦点输入框（Ctrl+V 方式）。适配中文、emoji、大段文本等 type_text 的按键扫描码方式可能失效的场景，兼容性更强（写操作，会请求授权）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "要粘贴的文本" }
            },
            "required": ["text"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let text = args["text"].as_str().ok_or("缺少参数 text")?;
        let res = self
            .capability
            .act(&Action::PasteText {
                text: text.to_string(),
            })
            .map_err(|e| e.to_string())?;
        Ok(json!({ "ok": res.ok, "description": res.description }))
    }
}

/// 最小化所有窗口工具（支持按标题关键词豁免）
pub struct WindowMinimizeAllTool {
    capability: Arc<dyn Capability>,
}

impl WindowMinimizeAllTool {
    pub fn new(capability: Arc<dyn Capability>) -> Self {
        Self { capability }
    }
}

impl Tool for WindowMinimizeAllTool {
    fn name(&self) -> &str {
        "window_minimize_all"
    }
    fn description(&self) -> &str {
        "最小化桌面上的所有顶层窗口，从而清空屏幕、避免目标窗口被其他窗口遮挡。可用 except 传入标题关键词列表（如 [\"资源管理器\"]），命中的窗口保持不变、不被最小化。配合 GUI 自动化操作前调用，能让目标窗口稳定可见（写操作，会请求授权）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "except": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "不要最小化的窗口标题关键词列表，如 [\"资源管理器\", \"Explorer\"]。留空则最小化所有窗口（含白泽自身）"
                }
            }
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let except: Vec<String> = match &args["except"] {
            Value::Array(a) => a
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
            _ => Vec::new(),
        };
        let res = self
            .capability
            .act(&Action::WindowMinimizeAll { except })
            .map_err(|e| e.to_string())?;
        Ok(json!({ "ok": res.ok, "description": res.description }))
    }
}

/// 置顶/取消置顶窗口工具
pub struct WindowSetTopmostTool {
    capability: Arc<dyn Capability>,
}

impl WindowSetTopmostTool {
    pub fn new(capability: Arc<dyn Capability>) -> Self {
        Self { capability }
    }
}

impl Tool for WindowSetTopmostTool {
    fn name(&self) -> &str {
        "window_set_topmost"
    }
    fn description(&self) -> &str {
        "把指定名称的窗口置顶（始终显示在最上层）或取消置顶。GUI 自动化操作目标窗口前调用，可防止目标被其他窗口遮挡（写操作，会请求授权）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "窗口标题关键词（模糊匹配），如 \"资源管理器\" 或 \"Explorer\"" },
                "topmost": { "type": "boolean", "description": "true=置顶，false=取消置顶，默认 true" }
            },
            "required": ["name"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let name = args["name"].as_str().ok_or("缺少参数 name")?;
        let topmost = args["topmost"].as_bool().unwrap_or(true);
        let res = self
            .capability
            .act(&Action::WindowSetTopmost {
                name: name.to_string(),
                topmost,
            })
            .map_err(|e| e.to_string())?;
        Ok(json!({ "ok": res.ok, "description": res.description }))
    }
}

/// 一键清屏准备工具：聚焦 + 置顶 + 最小化其余窗口 + 验证（一次调用完成）
pub struct WindowPrepareTool {
    capability: Arc<dyn Capability>,
}

impl WindowPrepareTool {
    pub fn new(capability: Arc<dyn Capability>) -> Self {
        Self { capability }
    }
}

impl Tool for WindowPrepareTool {
    fn name(&self) -> &str {
        "window_prepare"
    }
    fn description(&self) -> &str {
        "一键清屏准备（GUI 操作前的首选，一次调用完成全部准备）：聚焦指定名称的窗口并验证前台切换是否成功（失败自动重试）、按需置顶并验证、最小化其余所有无关窗口，返回验证结果（前台切换/置顶/最小化数量）。等价于 list_windows+window_minimize_all+window_focus+window_set_topmost 的组合，GUI 任务开始时用一次即可（写操作，会请求授权）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "目标窗口标题关键词（模糊匹配），如 \"汽水音乐\"" },
                "topmost": { "type": "boolean", "description": "是否置顶目标窗口，默认 true" }
            },
            "required": ["name"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let name = args["name"].as_str().ok_or("缺少参数 name")?;
        let topmost = args["topmost"].as_bool().unwrap_or(true);
        let res = self
            .capability
            .act(&Action::WindowPrepare {
                name: name.to_string(),
                topmost,
            })
            .map_err(|e| e.to_string())?;
        Ok(json!({ "ok": res.ok, "description": res.description }))
    }
}

/// 聚焦/前置窗口工具
pub struct WindowFocusTool {
    capability: Arc<dyn Capability>,
}

impl WindowFocusTool {
    pub fn new(capability: Arc<dyn Capability>) -> Self {
        Self { capability }
    }
}

impl Tool for WindowFocusTool {
    fn name(&self) -> &str {
        "window_focus"
    }
    fn description(&self) -> &str {
        "把指定名称的窗口调到前台并聚焦（若已最小化则先还原）。GUI 自动化操作前调用，确保目标窗口拿到焦点、不被遮挡（写操作，会请求授权）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "窗口标题关键词（模糊匹配），如 \"资源管理器\"" }
            },
            "required": ["name"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let name = args["name"].as_str().ok_or("缺少参数 name")?;
        let res = self
            .capability
            .act(&Action::WindowFocus {
                name: name.to_string(),
            })
            .map_err(|e| e.to_string())?;
        Ok(json!({ "ok": res.ok, "description": res.description }))
    }
}
