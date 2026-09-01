use std::collections::{HashMap, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::model::{ChatMessage, ChatResponse};
use crate::security::{AuditEntry, PermissionDecision, SecurityManager};
use crate::AppState;

const APPROVAL_TIMEOUT: Duration = Duration::from_secs(900); // 15 分钟总超时（配合升级系统）
const CANCELLED: &str = "__CANCELLED__";

/// 循环检测参数：连续 N 轮出现完全相同的工具调用指纹即判定陷入循环
const STREAK_THRESHOLD: usize = 3;
/// 滑动窗口大小（用于检测周期性重复）
const LOOP_WINDOW: usize = 12;
/// 滑动窗口内同一指纹出现次数达到该值即判定周期性循环
const WINDOW_THRESHOLD: usize = 5;

/// 观察类工具（只读、每次结果都可能不同）：截图/读树/列窗口/查找/OCR 等。
/// GUI 自动化中反复调用它们是「观察→决策」的正常节奏（屏幕会随操作变化），
/// 不应计入循环指纹，否则「分别打开多个应用并逐个操作」这类任务会被误判为死循环。
const OBSERVATIONAL_TOOLS: &[&str] = &[
    "capture_screen",
    "read_screen",
    "read_window",
    "list_windows",
    "find_element",
    "ground_element",
    "ocr_image",
    "screen_elements",
    "wait_ui_stable",
    "region_ocr",
    "board_diff",
];

/// 屏幕接管守卫：在 scope 结束时（无论正常/停止/报错）自动解除接管，恢复主窗口与外界输入。
struct TakeoverGuard<'g>(&'g AppHandle);

impl Drop for TakeoverGuard<'_> {
    fn drop(&mut self) {
        crate::takeover::disengage_screen(self.0);
    }
}

/// 循环检测器：记录每轮「工具调用序列指纹」，出现以下任一情形即判定陷入循环（重复无效操作）：
/// 1) 连续 STREAK_THRESHOLD 轮指纹完全相同；
/// 2) 最近 LOOP_WINDOW 轮内同一指纹累计达到 WINDOW_THRESHOLD 次。
/// 命中即自动终止以免空转；否则持续执行直到任务完成。
struct LoopDetector {
    history: VecDeque<String>,
}

impl LoopDetector {
    fn new() -> Self {
        Self {
            history: VecDeque::new(),
        }
    }

    /// 记录本轮指纹，若判定陷入循环返回 true。
    fn record(&mut self, fingerprint: String) -> bool {
        self.history.push_back(fingerprint.clone());
        while self.history.len() > LOOP_WINDOW {
            self.history.pop_front();
        }

        // 1) 连续重复：最近若干轮指纹完全相同。
        let n = self.history.len();
        let mut streak = 1usize;
        for i in (0..n.saturating_sub(1)).rev() {
            if self.history[i] == fingerprint {
                streak += 1;
            } else {
                break;
            }
        }
        if streak >= STREAK_THRESHOLD {
            return true;
        }

        // 2) 滑动窗口内同一指纹高频出现（周期性重复）。
        if n >= LOOP_WINDOW {
            let mut counts: HashMap<&str, usize> = HashMap::new();
            for item in self.history.iter() {
                *counts.entry(item.as_str()).or_insert(0) += 1;
            }
            if counts.values().any(|&c| c >= WINDOW_THRESHOLD) {
                return true;
            }
        }
        false
    }
}

/// 判定是否为「桌面 GUI 自动化」任务（鼠标/键盘/拖拽/选中等界面操作）。
/// 破坏性点击目标关键词：命中即强制高危审批 + 暂停本轮批量（回退原则第 2 级）
const DESTRUCTIVE_WORDS: &[&str] = &[
    "删除", "清空", "格式化", "卸载", "发送", "注销", "关机", "移除", "抹掉", "重置",
];

// ─────────────── 回退原则第 1 级：预期状态校验 ───────────────
//
// 批量派发前模型必须先调用 set_expected_state 写明「执行完界面应该是什么样」；
// 批量执行完毕后系统自动截屏 + 取元素树，交给模型比对「是否符合预期」——
// 把验证从「界面变了」升级为「变成了预期的样子」，不符立即叫停。

static EXPECTED_STATE: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn set_expected_state(desc: &str) {
    let slot = EXPECTED_STATE.get_or_init(|| Mutex::new(None));
    if let Ok(mut g) = slot.lock() {
        let brief: String = desc.chars().take(400).collect();
        *g = Some(brief);
    }
}

fn take_expected_state() -> Option<String> {
    let slot = EXPECTED_STATE.get_or_init(|| Mutex::new(None));
    slot.lock().ok().and_then(|mut g| g.take())
}

/// set_expected_state：批量派发前的预期状态声明（回退原则第 1 级）
pub struct ExpectedStateTool;

impl crate::tools::Tool for ExpectedStateTool {
    fn name(&self) -> &str {
        "set_expected_state"
    }
    fn description(&self) -> &str {
        "声明本批 GUI 操作执行完成后的「预期界面状态」（一句话描述）。必须在批量派发的同一轮里作为第一个工具调用，例如：set_expected_state(\"记事本标题变为 todo.txt，正文含三行文字\")。批量执行完毕后系统会自动截屏并核对此预期，不符将立即叫停——这是防止「点了但没点对」的关键校验"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "description": { "type": "string", "description": "执行完成后界面应呈现的状态，一句话，要具体可核验（标题/选中项/弹窗消失/文字内容等）" }
            },
            "required": ["description"]
        })
    }
    fn permission(&self) -> crate::tools::PermissionClass {
        crate::tools::PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let desc = args["description"]
            .as_str()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .ok_or("缺少 description 参数")?;
        set_expected_state(desc);
        Ok(json!({ "ok": true, "note": "预期状态已登记，批量执行完毕后将自动核验" }))
    }
}

fn is_gui_task(message: &str) -> bool {
    const GUI_KEYWORDS: &[&str] = &[
        "点击", "双击", "右键", "拖拽", "按住", "选中", "框选", "鼠标", "shift", "ctrl",
        "键盘操作", "屏幕操作", "界面操作", "桌面操作",
    ];
    let m = message.to_lowercase();
    GUI_KEYWORDS.iter().any(|k| m.contains(k))
}

// ─────────────── 系统提示词分段（分级注入：按任务语义拼装，纯聊天任务省 2/3 token） ───────────────
// 原则：宁可多注入不错漏——误命中只多花几百 token，漏段会让模型不知道对应预案而瞎操作。

/// CORE：身份 + 通用工具纪律，恒定注入
const PROMPT_CORE: &str = r#"你是白泽，一个本地优先的桌面助手。请用中文简洁回答；需要读取文件或目录时调用工具。
重要：当用户要求「写文档 / 写报告 / 写总结 / 写教程 / 生成 Markdown」时，必须调用 markdown_set 工具把完整内容写入右侧文档窗口，而不是只在对话里粘贴 Markdown 文本；写完后再用一句话告知用户已写入。
调研类任务：用 browser_search 搜索 1 次。无论搜索结果是否为空，都直接基于结果或你自己的知识整理并调用 markdown_set 写文档，绝对不要反复重试搜索或导航网页。
执行多步骤任务时，用 todo_update 工具更新任务进度（把步骤标记为 in_progress 或 completed）。
回复中包含适合可视化展示的结构化信息（天气/日程安排/比分/行情/系统状态/多维度对比等）时，调用 chat_card 工具推送一张精美卡片嵌入聊天框：html 用内联样式自由排版（渐变背景/圆角卡片/flex 布局/emoji 图标/表格/https 图片），width 可调（如 340px）；排版美观克制、信息分层、深色背景友好。普通问答不要滥用卡片。
当用户要求「定时提醒 / X 分钟后提醒 / X 小时后提醒 / 到点提醒」时，调用 set_reminder 工具（delay_seconds 请换算成秒）。
当指令来自微信/飞书消息且回复需要附截图或图片时，调用 wechat_send_image 工具（path 传图片本地路径即可真实发出图片消息，不要把文件路径当文字回复）；回复文本中的截图路径系统也会自动转成图片消息。wechat_send_image 是白泽的内部工具，只能作为工具调用，严禁把它当命令行程序丢给 run_command/ps_exec 执行。
你运行在 Windows 系统上。执行命令行操作、运行脚本、构建项目、安装依赖、查看进程等任务时，优先使用 terminal_send 工具（在「白泽终端」窗口里真实执行，用户能实时看到命令与滚动输出，体验最佳；首次调用会自动打开终端窗口；返回终端输出文本供你判断成败，失败就分析纠正重试；长命令把 timeout_ms 调大，如构建/安装传 60000~120000；同一终端会话状态延续，cd/环境变量设置对后续命令持续生效）。ps_exec 作为后备：需要结构化 stdout/stderr/exit_code、无需展示的后台快速命令时用它。两者不要对同一条命令混用。
执行命令后检查输出中的错误信息（如 error、failed、not found、cannot），失败则分析原因并纠正后重试。
回答中可自行对关键的结论、数字、术语、文件路径或需要用户注意的内容用 ==文字== 包裹做高亮强调（例如 ==重点==），其余保持普通文本，不要过度使用。
当用户请求不清晰、缺少关键参数或存在歧义时，先向用户提出一个简短的澄清问题（例如「你指的是哪个文件？」「要写到哪里？」），等用户回复后再继续，不要擅自假设或直接执行。"#;

/// GUI：屏幕接管 / 稳定性感知 / 应用操作预案 / 工具清单与交互纪律 / 批量派发 / 撤销
const PROMPT_GUI: &str = r#"凡涉及操作桌面上的应用程序或窗口的任务（打开/关闭/切换应用与程序窗口、在应用界面里点击输入、管理任务栏等），若发现屏幕尚未接管，必须先调用 takeover 工具接管屏幕再执行 GUI 操作，不要在未接管的情况下直接点击。
【界面稳定性感知（务必遵守，提速关键）】窗口刚打开、点击后界面跳转、页面加载完成后的 1-3 秒内动画/转场尚未结束，此时 find_element/坐标会漂移——用 wait_ui_stable 工具等待界面安定（像素级检测，安定即返回，通常 0.5-2s）再定位，替代拍脑袋 sleep 3 秒：又准又不浪费轮次。规则：launch_app/explorer_open 返回窗口后、批量步骤之间遇到界面跳转、capture_screen(ocr=true) 找不到预期文字时，先 wait_ui_stable 再操作。
【常见应用操作预案】① 启动应用一律用 launch_app(name)——一次完成开始菜单/UWP 匹配启动并等待窗口出现返回窗口信息，禁止 GUI 点开始菜单搜索、禁止 ps_exec Start-Process；② 浏览器类：Ctrl+L 聚焦地址栏直接输入完整 URL 或搜索词回车，不要从首页一层层点导航；③ 资源管理器：explorer_open(path) 一步打开并定位到文件夹/文件（等待窗口出现），不要逐层双击；也可 Ctrl+L 直接输入完整路径回车；④ 创建文档/写文件内容一律用 write_file 工具（一步原子完成、自动快照可撤销），严禁 GUI 绕记事本多步——记事本/编辑器 GUI 只留给用户明确要求「打开应用操作界面」的场景；确实需要在应用内保存时：Ctrl+S 对已存在文件通常静默保存，弹出「另存为」对话框时用 save_dialog(path) 一步填入完整路径保存（自动处理替换确认弹窗），不要手动定位文件名输入框；编辑器内输入用 paste_text 不要逐字 type_text；⑤ 设置/控制面板类：直接用应用内搜索框搜功能名；⑥ 聊天类（微信/QQ 等）：Ctrl+F 或 Ctrl+K 搜联系人后回车；⑦ 自渲染应用（Electron/Qt）：UIA 树可能为空或不全，优先 screen_elements 的 OCR 行坐标，失败再走键盘导航兜底；⑧ 自绘应用窗口查找加强：list_windows 会列出最小化的后台窗口（minimized=true，聚焦时自动还原）并给出每窗口的 process（进程 exe 名）——窗口标题不含中文关键词时按 process 列匹配（如找「汽水」看 QishuiMusic.exe），聚焦失败先 list_windows 查 process 再用窗口真名/进程名重试。
当用户要求「打开应用 / 点击 / 输入 / 拖拽 / 按快捷键 / 看屏幕内容」等桌面界面操作时，使用 GUI 自动化工具：key_press 按快捷键（如 win+r、ctrl+s、alt+tab），type_text 输入文本（仅限 ASCII 短文本；输入中文/emoji/长文本一律改用 paste_text 剪贴板粘贴，否则会乱码），mouse_click 点击屏幕坐标，click_element 按名称点击控件（该控件若无无障碍树时，会自动降级：本地 OCR 定位文字 → Set-of-Marks 视觉标注框选 → 视觉模型猜坐标，点击后还会自动检测界面是否变化），capture_screen 截屏（加 ocr=true 直接返回画面文字，或加 question 让本地视觉模型描述截图），mouse_drag 拖拽，wheel_scroll 滚轮滚动（clicks 正=上/右负=下/左，可传 x/y 先把光标移到目标区域；长页面/列表滚动一律用它，不要拖滚动条），hover 悬停（展开悬停才出现的菜单/子菜单后再点击其中项），middle_click 中键（浏览器/编辑器关闭标签页、后台新标签打开链接），close_popup 关闭弹窗（打开应用后若有广告/更新/欢迎/协议等弹窗，默认 mode=close 点关闭/稍后/跳过/取消按钮；协议类弹窗用 mode=confirm 点确定/同意）。【交互纪律（必须遵守）】双击一律用 mouse_click 传 count=2——连续两次独立单击间隔过长，系统不会识别为双击，严禁用两次单击模拟；列表类界面（音乐/文件/搜索结果）的正确交互是先 hover 行让操作按钮浮现再点按钮，或点一下列表建立键盘焦点后用方向键+Enter 导航选择；find_element/ground_element 在页面动画/加载期间坐标会漂移，界面刚发生变化时先等待 1-2 秒再定位；自渲染应用（Electron/Qt 等 UIA 树为空）且 OCR 定位也失败时，兜底走键盘导航（点列表区域建立焦点 → 方向键 → Enter）。执行任何 GUI 操作前，必须先完成「清屏准备」：直接调用一次 window_prepare（name=目标窗口标题关键词，如「汽水音乐」）——它会一次完成最小化其余无关窗口、聚焦目标窗口并验证前台切换成功、按需置顶，并返回验证结果（最小化数量/聚焦成败/置顶成败）；接管屏幕时系统已按任务语义自动最小化无关窗口，不要再用 list_windows → window_minimize_all → window_focus → window_set_topmost 的多步组合（浪费轮次），仅当 window_prepare 返回聚焦失败或未找到窗口时才用 list_windows 排查。步骤弹幕在接管后自动推送每个工具调用，无需手动 show_step（只在需要主动补充提示文字时才用）；操作全部结束后用 screen_release 解除接管（任务结束时系统也会自动解除）。进入目标应用后，优先调用一次 screen_elements 一次性拿到全屏元素清单（UIA 可交互控件 + OCR 文字行合并，含屏幕物理坐标；自渲染应用 UIA 为空时自动只剩 OCR 行），基于它直接把整套操作规划成有序步骤批量派发，不要反复截屏/逐个 find_element——这张清单就是批量规划的依据。批量派发时以多个工具调用一次性发出（同一轮内按顺序执行，批量第一个调用必须是 set_expected_state 写明一句可核验的「预期状态」，例如 set_expected_state("记事本标题变为 todo.txt 且正文含三行文字")；批量结尾放一次 capture_screen），系统会在批量执行完毕后自动截屏核验是否符合预期并把结果回传给你——校验不符时立即停止批量推进，退回逐步模式修正，不要把「界面有变化」当成「符合预期」；对含 删除/清空/格式化/卸载/发送 字样的点击目标，一次只派发一个调用（系统会强制逐次审批并暂停批量），确认成功后再进行下一步；批量中某步失败或界面与预期不符时，再退回逐步模式（截屏观察 → 单步修正）。需要撤销已执行的 GUI 操作时用 gui_undo：action=last 清空最近文本输入、toggle 重击最近点击（开关类翻转）、back 返回上一界面、file 撤销文件操作、steps 逆序回退最近 n 步可逆操作；删除/发送类不可逆操作无法回退，只能靠事前审批拦截。不要在元素地图已明确、步骤确定的情况下每个操作之间都单独截屏观察——批量执行是 GUI 任务提速的关键。
【GUI 操作优先级（务必遵守）】遇到界面上的按钮/弹窗/确认框时，你的首选永远是用鼠标点击能力：click_element（按名称点控件）→ close_popup（点关闭/确定/同意）→ mouse_click 或 click_at（按坐标点）。绝对不要一遇到弹窗就去写脚本、提权、改注册表——你本身就具备「直接点击屏幕」的能力，写脚本是绕远路且常失败。【窗口与工具纪律（务必遵守）】用户要求「打开/切换到」某个应用窗口时，先用 list_windows 确认该窗口是否已存在：已存在就用 window_focus 聚焦已有窗口（聚焦后屏幕会给目标窗口点亮光圈），绝不要重新启动一个新实例；确认不存在才启动。界面上的点击与文字输入必须走 GUI 工具链（show_step → click_element/click_at/type_text/paste_text），禁止用 ps_exec/run_command 发送按键来模拟界面操作——那会绕过屏幕接管与桌面弹幕，用户将完全看不到你在做什么。当某个弹窗点了没反应时，先 list_windows 看当前到底弹出了几个窗口（可能连续弹多个确认框），再逐个 read_window / capture_screen 定位内容，继续点击即可。遇到空标题的 Dialog 或自渲染窗口（Electron/DirectUI/Qt/自绘卸载器等，read_window 的无障碍树是空的）时，用 list_windows 传 ==with_preview=true== 参数，它会截屏 OCR 并把每个窗口里的文字/按钮塞进 preview 字段，据此直接定位该点哪个按钮。"#;

/// GAME：回合制/半即时游戏自动化战术（连带注入 GUI 段）
const PROMPT_GAME: &str = r#"【游戏自动化（回合制/半即时通用，金铲铲/云顶/崩铁等）】核心原则：感知提速 + 动作合并。
① 局面感知：先用 capture_screen(ocr=true) 全屏看一次，记下棋盘/商店/血条/小地图/任务文字等固定区域的坐标；之后每回合用 region_ocr 只识别这些小区域（返回词的绝对坐标可直接用于点击），或用 board_diff（key=游戏+界面名，regions 传区域列表）——它自动缓存上次快照并增量 diff，只返回 changed 区域，本回合只对变化项做决策，不要全屏反复识别。
② 动作合并：重复性按键/点击用 macro 一次调用完成，不要拆成 N 次工具调用——「D 牌 5 次」= 5 组 [{action:'wait',ms:400},{action:'key',keys:'d'}]；战斗连招 = 多个 {action:'key'} 用 {action:'wait',ms} 控制节奏；种植/占格 = 多个 {action:'click',x,y}；点击沿用拟人化注入，与真人无异。
③ 崩铁跑图：board_diff 读右上小地图区域（箭头/罗盘）+ 左上任务目标文字校准方向，配合 key_press('w') + macro 固定步长前进，走到任务文字变化再校准；战斗为回合制，用 board_diff 读技能/血条区域决策，macro 释放技能。
④ 宏与 wait_ui_stable 组合：macro 执行完接一次 wait_ui_stable 再 board_diff，形成「感知→决策→合并动作→确认」的稳定循环。"#;

/// MUSIC：音乐应用交互（连带注入 GUI 段）
const PROMPT_MUSIC: &str = r#"【音乐播放器交互（务必遵守，踩过坑）】汽水音乐等 Electron 自渲染音乐应用（UIA 树为空）播放歌曲的正确姿势：先用 screen_elements 拿到元素坐标清单（它的坐标是精确的，禁止让视觉模型在截图里报坐标——视觉坐标有 ±10-20px 误差），定位到目标歌曲所在的行后按以下顺序尝试：① mouse_click(x=行中心x, y=行中心y, count=2) 双击该行直接播放；② mouse_click 传 button=right 右键该行，弹出上下文菜单后再用 screen_elements 定位「播放」菜单项并点击它；③ hover(x,y) 悬停该行等 1 秒让行首播放按钮浮现，再点那个按钮；④ 首选可靠路径：screen_elements 的输出里 type=Image、名字是「Image图标」、y 与歌曲行相同的元素就是行首播放按钮（Rust 已算好精确中心坐标 cx/cy），先 hover 该行让按钮进入可点状态，再 mouse_click 那个 cx/cy 即播放。严禁对歌曲行做两次独立单击（间隔超时不算双击）。系统的单击/双击注入已拟人化：自动「两段式逼近移动 + hover 停留 160ms + 双击内部 90ms 紧连」，与真人点击无异；若双击仍无效，问题在落点或该行交互方式，直接转②④路径，不要怀疑点击本身。每次尝试后必须验证播放是否真正开始：capture_screen(ocr=true) 查看出现「暂停按钮/播放中/进度条走动」，或再调 screen_elements 对比界面变化；验证无反应就换下一种方式，不要在同一坐标反复重试。"#;

/// BROWSER：浏览器工具选型与通道唯一性
const PROMPT_BROWSER: &str = r#"当用户要求「登录网站 / 操作网页 / 网页截图 / 读取网页内容」等浏览器交互时，使用 browser_act 工具（action 可选 goto 跳转、click 点击、type 输入、wait 等待、screenshot 截图、evaluate 执行 JS、content 读文本）。
当用户要求「打开网页 / 打开 XX 网站 / 打开链接 / 用白泽浏览器打开 XX」时，调用 browser_open 工具（kind 填 url，content 填完整网址如 https://example.com），把网页显示在内置的「白泽·浏览器」标签页里；若该网站拒绝内嵌导致打不开，改用 browser_navigate 工具在独立的系统浏览器窗口中打开。
【浏览器通道唯一性（必须遵守）】一次「打开某网站」的请求只允许使用一个通道：普通打开→browser_open；明确要「桌面浏览器/受控浏览器」→仅用 browser_act 的 goto/new_tab；内嵌被拒需要独立窗口→仅用 browser_navigate。严禁对同一请求叠加调用 browser_open + browser_navigate / browser_act。"#;

/// SOFTWARE：软件管家 + 系统环境配置
const PROMPT_SOFTWARE: &str = r#"当用户要求「查找软件 / 搜索软件 / 帮我装 XX / 安装某软件 / 卸载某软件」时，用软件管家工具集：先 env_check 探测环境（包管理器/运行时/权限），再 software_search 搜索（query 填软件关键词），从返回结果里取 id 和 name（软件显示名）。安装前先调用 disk_info 检测磁盘空间与装机习惯、拿到推荐安装盘符，并在回复里明确告诉用户「将安装到 X 盘（理由）」。然后用 software_install 安装（必填 id，尽量带上 name）或 software_uninstall 卸载；判断某软件是否已安装 / 查找其安装位置，一律用 software_locate（注册表+UWP 商店应用+开始菜单快捷方式三路秒级定位，返回版本/厂商/安装路径）——严禁用全盘搜索判断（Get-ChildItem -Recurse、dir /s 等，又慢又不准）；查完整已装清单用 software_list，看某个包详情用 software_info。安装会智能避开系统盘 C:，自动装到空间充足且符合你装机习惯的盘。
当用户要求「配置系统环境 / 设置环境变量 / 添加 PATH / 设置开机自启」时，先 system_get 读取现状（env/path/startup），再用 system_set 执行（action 可选 env_set / env_unset / path_add / path_remove / startup_add / startup_remove）。高亮可安装的包 id 用 ==id==。"#;

/// 按用户消息关键词挑选本轮需注入的提示词分段（CORE 恒定，不在此列）
fn prompt_segments(message: &str) -> Vec<&'static str> {
    let m = message.to_lowercase();
    let hit = |kws: &[&str]| kws.iter().any(|k| m.contains(k));
    let mut segs: Vec<&'static str> = Vec::new();

    const GUI_WORDS: &[&str] = &[
        "点击", "双击", "右键", "拖拽", "按住", "选中", "框选", "鼠标", "键盘", "屏幕",
        "界面", "桌面", "窗口", "弹窗", "截屏", "截图", "任务栏", "记事本", "资源管理器",
        "文件夹", "按钮", "菜单", "输入框", "剪贴板", "粘贴", "输入到", "打开", "启动",
        "关掉", "关闭", "最小化", "置顶", "写文件", "另存为", "保存到",
    ];
    const GAME_WORDS: &[&str] = &[
        "金铲铲", "云顶", "崩铁", "星穹", "自走棋", "回合制", "棋盘", "出牌", "打牌", "对局",
    ];
    const MUSIC_WORDS: &[&str] = &[
        "音乐", "歌曲", "歌单", "汽水", "网易云", "播放", "切歌", "下一首", "放首歌",
    ];
    const BROWSER_WORDS: &[&str] = &[
        "网页", "网站", "网址", "浏览器", "http", "www.", "搜索", "搜一下", "查一下",
        "查资料", "调研", "上网", "登录",
    ];
    const SOFTWARE_WORDS: &[&str] = &[
        "安装", "卸载", "软件", "环境变量", "path", "开机自启", "装个", "装一下", "装上",
    ];

    // 游戏/音乐任务必然伴随 GUI 操作，连带注入 GUI 段
    if hit(GAME_WORDS) {
        segs.push(PROMPT_GUI);
        segs.push(PROMPT_GAME);
    } else if hit(MUSIC_WORDS) {
        segs.push(PROMPT_GUI);
        segs.push(PROMPT_MUSIC);
    }
    if (is_gui_task(message) || hit(GUI_WORDS)) && !segs.contains(&PROMPT_GUI) {
        segs.insert(0, PROMPT_GUI);
    }
    if hit(BROWSER_WORDS) {
        segs.push(PROMPT_BROWSER);
    }
    if hit(SOFTWARE_WORDS) {
        segs.push(PROMPT_SOFTWARE);
    }
    segs
}

/// 计算一轮工具调用的「指纹」：工具名 + 参数拼接，用于识别重复操作。
/// 观察类工具（截图/读树/列窗口/查找/OCR）不参与指纹——它们重复是正常的观察节奏。
fn round_fingerprint(calls: &[Value]) -> String {
    let mut out = String::new();
    for c in calls {
        let name = c["function"]["name"].as_str().unwrap_or("");
        if OBSERVATIONAL_TOOLS.contains(&name) {
            continue;
        }
        let args = c["function"]["arguments"].clone();
        out.push_str(name);
        out.push('|');
        out.push_str(&args.to_string());
        out.push(';');
    }
    out
}

/// 提炼 GUI 工具调用的关键参数（配方沉淀用：短小、可复现，坐标/控件名/按键保留）
fn condense_gui_args(name: &str, args: &Value) -> String {
    let s = |k: &str| {
        args.get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .chars()
            .take(16)
            .collect::<String>()
    };
    let num = |k: &str| args.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0) as i32;
    match name {
        "mouse_click" | "click_at" => {
            let count = args.get("count").and_then(|v| v.as_i64()).unwrap_or(1);
            let c = if count > 1 { format!("×{count}") } else { String::new() };
            format!("({},{}){}", num("x"), num("y"), c)
        }
        "click_element" => format!("「{}」", s("name")),
        "mouse_drag" => format!("({},{})→({},{})", num("from_x"), num("from_y"), num("to_x"), num("to_y")),
        "key_press" => format!("({})", s("keys")),
        "type_text" | "paste_text" => format!("「{}…」", s("text")),
        "wheel_scroll" => format!("({})", num("clicks")),
        "hover" => format!("({},{})", num("x"), num("y")),
        "save_dialog" | "explorer_open" | "launch_app" => format!("({})", s("path")),
        _ => String::new(),
    }
}

/// Agent 状态机阶段（对应设计文档 4.1：Plan → Act → Observe → Reflect）
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub enum AgentPhase {
    Planning,
    Executing,
    WaitingApproval,
    Reflecting,
    Done,
}

/// Agent 循环：持有 AppHandle（发事件）与 AppState（取工具/模型/安全/存储）。
pub struct AgentLoop<'a> {
    app: &'a AppHandle,
    state: &'a AppState,
    /// 会话所属项目（可选）：注入【当前项目】上下文，模型由此知道自己在为哪个项目服务
    project: Option<crate::memory::ProjectRow>,
    /// 本轮成功的 GUI 操作链（收尾时提炼「成功操作配方」写入记忆库，下次同类任务直接照用）
    gui_ops: std::sync::Mutex<Vec<String>>,
    /// 本轮 GUI 操作的目标应用（window_prepare 的 name 参数）
    gui_target: std::sync::Mutex<Option<String>>,
}

impl<'a> AgentLoop<'a> {
    pub fn new(app: &'a AppHandle, state: &'a AppState) -> Self {
        Self {
            app,
            state,
            project: None,
            gui_ops: std::sync::Mutex::new(Vec::new()),
            gui_target: std::sync::Mutex::new(None),
        }
    }

    /// 附带会话所属项目（Supervisor 透传）
    pub fn with_project(mut self, project: Option<crate::memory::ProjectRow>) -> Self {
        self.project = project;
        self
    }

    /// 经验复盘（反思机制落地）：任务执行中出现过工具失败并最终完成时，
    /// 让模型把「遇到的问题→解决办法」提炼成一条可复用经验，写入记忆库（kind=lesson）。
    /// 相似经验会被 smart_remember 自动强化而非重复堆积；下次执行同类任务时
    /// recall_lessons 召回注入提示词，直接采用已验证的解决办法。
    async fn reflect_lessons(&self, user_task: &str, failures: &[String]) {
        let failures_text = failures
            .iter()
            .take(4)
            .map(|s| format!("- {s}"))
            .collect::<Vec<_>>()
            .join("\n");
        let prompt = format!(
            "执行任务时遇到了以下工具失败，但最终完成了任务。请把「遇到的问题→最终怎么解决的」\
             提炼成一条可复用的经验教训：一句话、120 字以内、以「遇到…时：…」句式开头。\
             只输出经验本身，不要任何解释或前后缀。\n\n用户任务：{user_task}\n\n遇到的失败：\n{failures_text}"
        );
        let msgs = vec![ChatMessage {
            role: "user".into(),
            content: prompt,
            tool_calls: None,
            tool_call_id: None,
        }];
        // 复盘最多等 12 秒：超时/失败静默跳过，绝不阻塞任务收尾
        let learned = match tokio::time::timeout(
            Duration::from_secs(12),
            self.state.model.chat(&msgs, &[]),
        )
        .await
        {
            Ok(Ok(r)) => r.content.unwrap_or_default(),
            _ => String::new(),
        };
        let lesson = learned
            .trim()
            .trim_matches(|c| matches!(c, '"' | '「' | '」' | '“' | '”' | '。'))
            .trim()
            .to_string();
        let len = lesson.chars().count();
        if !(10..=300).contains(&len) {
            return; // 模型没给出有效经验，静默放弃
        }
        match self.state.store.smart_remember(&lesson, "lesson") {
            Ok(crate::memory::RememberOutcome::Created) => {
                self.thought("reflect", "经验沉淀", &format!("{lesson}（已写入经验库）"));
            }
            Ok(crate::memory::RememberOutcome::Reinforced) => {
                self.thought("reflect", "经验强化", &format!("{lesson}（相似经验已存在，权重提升）"));
            }
            _ => {}
        }
    }

    /// 成功操作配方沉淀：本轮 GUI 操作 ≥3 步且任务顺利完成时，把成功操作链
    /// 提炼成一条「应用/场景：步骤链」配方写入记忆库（kind=recipe）。
    /// 下次操作同类应用时召回注入提示词——「记住上次怎么成功的」，规划好就一路跑完。
    async fn reflect_recipe(&self, user_task: &str) {
        let ops = self.gui_ops.lock().unwrap().clone();
        if ops.len() < 3 {
            return; // 操作太少没有沉淀价值
        }
        let target = self
            .gui_target
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| "通用".to_string());
        let ops_text = ops
            .iter()
            .take(14)
            .map(|s| format!("- {s}"))
            .collect::<Vec<_>>()
            .join("\n");
        let prompt = format!(
            "下面是一次成功的 GUI 自动化操作链。请提炼成一条「成功操作配方」：\
             格式为「应用/场景：步骤1 → 步骤2 → …」，150 字以内，\
             只保留可复现的关键步骤与坐标/控件技巧，省略截图等观察类调用。只输出配方本身。\n\n\
             用户任务：{user_task}\n目标应用：{target}\n\n操作链：\n{ops_text}"
        );
        let msgs = vec![ChatMessage {
            role: "user".into(),
            content: prompt,
            tool_calls: None,
            tool_call_id: None,
        }];
        let learned = match tokio::time::timeout(
            Duration::from_secs(12),
            self.state.model.chat(&msgs, &[]),
        )
        .await
        {
            Ok(Ok(r)) => r.content.unwrap_or_default(),
            _ => String::new(),
        };
        let recipe = learned
            .trim()
            .trim_matches(|c| matches!(c, '"' | '「' | '」' | '“' | '”'))
            .trim()
            .to_string();
        let len = recipe.chars().count();
        if !(15..=300).contains(&len) {
            return;
        }
        match self.state.store.smart_remember(&recipe, "recipe") {
            Ok(crate::memory::RememberOutcome::Created) => {
                self.thought("reflect", "配方沉淀", &format!("{recipe}（已写入配方库）"));
            }
            Ok(crate::memory::RememberOutcome::Reinforced) => {
                self.thought("reflect", "配方强化", &format!("{recipe}（相似配方已存在，权重提升）"));
            }
            _ => {}
        }
    }

    fn phase(&self, p: AgentPhase) {
        let (label, detail) = match p {
            AgentPhase::Planning => ("规划", "分析需求并制定执行步骤"),
            AgentPhase::Executing => ("执行", "调用工具执行任务"),
            AgentPhase::WaitingApproval => ("等待授权", "需要你确认本次操作"),
            AgentPhase::Reflecting => ("反思", "回顾执行结果，整理关键要点"),
            AgentPhase::Done => ("完成", "本轮任务处理完成"),
        };
        let _ = self.app.emit("phase", p);
        let _ = self
            .app
            .emit("thought", json!({ "kind": "phase", "label": label, "detail": detail }));
        self.state.log_thought("phase", label, detail);
    }

    fn thought(&self, kind: &str, label: &str, detail: &str) {
        let _ = self
            .app
            .emit("thought", json!({ "kind": kind, "label": label, "detail": detail }));
        self.state.log_thought(kind, label, detail);
    }

    /// 当前模式下可见的工具集合：无模式 = 全部；有模式 = 白名单过滤 + 自研工具合并
    fn mode_tools(&self) -> Vec<Value> {
        let mode = self.state.workmodes.current();
        let Some(mode) = mode else {
            return self.state.tools.schemas();
        };
        let mut allowed: Vec<String> = mode.allowed_tools.clone();
        allowed.extend(self.state.workmodes.authored());
        let refs: Vec<&str> = allowed.iter().map(|s| s.as_str()).collect();
        self.state.tools.schemas_filtered(&refs)
    }

    /// 运行一轮对话：规划 → 执行（含工具调用/审批）→ 反思 → 完成
    pub async fn run(&self, message: &str, history: Vec<ChatMessage>) -> Result<String, String> {
        self.phase(AgentPhase::Planning);
        // 新一轮任务开始：清空 GUI 关键帧日志，避免混入上一轮的截屏
        crate::replay::clear_keyframes();
        // 清空上一轮残留的聊天卡片（chat_card 槽位与关键帧同生命周期）
        take_chat_cards();
        crate::replay::clear_gui_ops();

        // 召回相关记忆（M3 v2：n-gram 相关度 + salience 重排）
        let memories = self.state.store.recall_related(message, 8).unwrap_or_default();
        // 记忆索引成功时发「memory」事件（思考流）+「memory-recall」（带召回 id，供意识网络精确高亮）
        if !memories.is_empty() {
            let preview: Vec<String> = memories
                .iter()
                .map(|m| m.content.chars().take(16).collect())
                .collect();
            self.thought(
                "memory",
                &format!("记忆索引 · {} 条", memories.len()),
                &preview.join("、"),
            );
            let ids: Vec<String> = memories.iter().map(|m| m.mem_id.clone()).collect();
            let _ = self.app.emit("memory-recall", json!({ "ids": ids }));
        }

        // 用户画像（长期偏好）+ 当前 UI 信号（内置浏览器正在看什么）
        let profile = self.state.store.recall_profile(5).unwrap_or_default();
        let ui_signal = {
            let b = self.state.browser.lock().unwrap();
            b.tabs
                .iter()
                .find(|t| t.active)
                .map(|t| {
                    let name = if t.title.is_empty() { t.kind.clone() } else { t.title.clone() };
                    format!("用户当前在内置浏览器查看「{name}」标签页。")
                })
                .unwrap_or_default()
        };

        // 分级注入：CORE 恒定 + 按任务关键词命中的场景段（PROMPT_*/prompt_segments 定义在文件顶部）
        let mut system_content = String::from(PROMPT_CORE);
        for seg in prompt_segments(message) {
            system_content.push_str(seg);
        }

        // 多模态自我认知：主模型勾选 multimodal 时，所有视觉调用（grounding/SoM/截图描述/状态校验）
        // 实际都由主模型自己的端点完成——明确告知它，避免它误以为存在另一个「视觉模型」
        if crate::visual_grounding::multimodal_main().is_some() {
            system_content.push_str(
                "\n\n【多模态能力（自我认知）】你自己就是多模态模型，配置已启用图片输入。\
                 系统中所有视觉调用（click_element 的视觉降级定位、Set-of-Marks 标注、capture_screen 的 question 图像理解、批量操作后的预期状态校验）\
                 都由你自己完成，不存在另一个独立视觉模型。因此：① 这些调用返回的是你自己看图后的结论，可信度按自己的视觉能力评估；\
                 ② 视觉坐标估计有 ±10-20px 误差，精确点击仍优先 screen_elements/OCR 坐标；\
                 ③ 用户发来图片附件时，图片内容会以描述文本形式注入对话，你基于描述作答即可。",
            );
        }

        // Token 节约：精简回复风格（约束输出长度与格式，直接省输出 token）
        if crate::token_saver::config().concise_reply {
            system_content.push_str(
                "\n\n【回复风格（节约输出 token，必须遵守）】\n\
                 1. 结论先行：第一句话直接给答案/结果，解释放后面且只讲必要信息。\n\
                 2. 默认精简：日常回答控制在 3~5 句以内；用户明确要「详细/展开/教程/报告」时才写长文，且长文一律走 markdown_set 写入文档窗口，不在聊天里贴全文。\n\
                 3. 不复述：不要复述工具输出的原文、不要重复用户的问题、不要把执行流里已展示的步骤再用文字叙述一遍。\n\
                 4. 不客套：省掉「好的」「明白了」「希望对你有帮助」等开场白与收尾语，也不要总结式收尾。\n\
                 5. 多条信息用短要点列表，每条一行；能用一句话说清的不写一段。\n\
                 6. 工具连续调用之间不要插入对用户说的话，全部做完后一次性汇报结果。",
            );
        }

        // 注入当前工作模式的系统提示词（专业身份 + 方法论 + 产出规范）
        if let Some(mode) = self.state.workmodes.current() {
            system_content.push_str(&format!(
                "\n\n【当前工作模式】{}\n{}\n（以该专业身份完成任务，遵循其工具与产出规范。）",
                mode.label, mode.system_prompt
            ));
        }

        // 注入当前工作空间（后端强绑定）：让模型知道默认根目录，文件工具的相对路径以它为基准
        if let Some(ws) = crate::tools::get_workspace() {
            if !ws.is_empty() {
                system_content.push_str(&format!(
                    "\n\n【当前工作空间】\n{}\n（文件工具的相对路径以此目录为根；读写该目录下文件时直接传相对路径即可。）",
                    ws
                ));
            }
        }

        // 注入当前项目（侧边栏「项目」导航）：会话归属哪个项目、项目工作目录在哪
        if let Some(p) = &self.project {
            let mut ctx = format!("\n\n【当前项目】\n本会话属于项目「{}」", p.name);
            if !p.path.is_empty() {
                ctx.push_str(&format!("，工作目录：{}", p.path));
            }
            ctx.push_str(
                "。\n（用户提到「这个项目 / 该项目 / 项目里」时即指它；涉及该项目文件与代码的读写、\
                 检索、执行操作优先在工作目录内进行，并优先考虑项目上下文来理解模糊指代。）",
            );
            system_content.push_str(&ctx);
        }

        if !memories.is_empty() {
            let mem_lines: Vec<String> = memories.iter().map(|m| format!("- {}", m.content)).collect();
            system_content.push_str(&format!(
                "\n\n【你记得的相关信息】\n{}\n\n（当用户问到这些信息时请引用；这些只是你的记忆片段，不一定是当前对话内容。）",
                mem_lines.join("\n")
            ));
        }

        // 经验教训召回（白泽自己的经验知识库）：注入过往「问题→解法」，
        // 同类任务直接采用已验证的解决办法，不必重新踩坑
        if let Ok(lessons) = self.state.store.recall_lessons(message, 3) {
            if !lessons.is_empty() {
                let lines: Vec<String> =
                    lessons.iter().map(|m| format!("- {}", m.content)).collect();
                system_content.push_str(&format!(
                    "\n\n【过往经验教训（遇到同类问题优先采用）】\n{}\n（这些是白泽过去任务中真实踩坑后总结的解决办法，可靠度高于临场尝试。）",
                    lines.join("\n")
                ));
            }
        }
        // 成功操作配方召回：注入过往同类应用「怎么成功的」操作链，
        // 模型规划 GUI 步骤时直接照用已验证序列——从「每步看一眼」变成「规划好一路跑完」
        if let Ok(recipes) = self.state.store.recall_recipes(message, 2) {
            if !recipes.is_empty() {
                let lines: Vec<String> =
                    recipes.iter().map(|m| format!("- {}", m.content)).collect();
                system_content.push_str(&format!(
                    "\n\n【过往成功操作配方（操作同类应用时优先照用）】\n{}\n（这些是白泽过去成功完成 GUI 任务的真实操作序列，坐标/控件名均验证有效；与本轮任务同类时按配方规划步骤，不必每步重新探索。）",
                    lines.join("\n")
                ));
                self.thought(
                    "memory",
                    "配方召回",
                    &format!("{} 条成功操作配方已注入规划", recipes.len()),
                );
            }
        }
        // 同类任务结果召回：相似指令直接参考上次执行结果——高频/重复任务秒回
        if let Ok(tasks) = self.state.store.recall_by_kind(message, "task", 1) {
            if let Some(t) = tasks.first() {
                if !t.content.is_empty() {
                    system_content.push_str(&format!(
                        "\n\n【上次同类任务的执行结果（相似指令参考）】\n{}\n（若本轮指令与上次相同且结果可复用，可直接告知用户上次结果或复用方式；情况有变化则正常重新执行。）",
                        t.content
                    ));
                    self.thought("memory", "任务记忆", "已召回上次同类任务的执行结果");
                }
            }
        }
        if !profile.is_empty() {
            let prof_lines: Vec<String> = profile.iter().map(|m| format!("- {}", m.content)).collect();
            system_content.push_str(&format!(
                "\n\n【关于用户的偏好/画像】\n{}\n（回答时考虑这些长期偏好；若与本轮无关则忽略。）",
                prof_lines.join("\n")
            ));
        }
        if !ui_signal.is_empty() {
            system_content.push_str(&format!("\n\n【当前 UI 信号】\n{ui_signal}"));
        }

        let mut messages = vec![ChatMessage {
            role: "system".into(),
            content: system_content,
            tool_calls: None,
            tool_call_id: None,
        }];
        messages.extend(history);
        messages.push(ChatMessage {
            role: "user".into(),
            content: message.to_string(),
            tool_calls: None,
            tool_call_id: None,
        });

        let tools = self.mode_tools();

        // GUI 任务：自动接管屏幕（阻断外部输入 + 最小化主窗口 + 桌面弹幕）。
        // 无论正常结束/停止/取消，退出时统一由 TakeoverGuard 释放接管。
        // 注意：这里不最小化其它窗口——目标窗口的防遮挡由模型按需调用
        // window_minimize_all / window_focus / window_set_topmost 完成（见 system 提示）。
        let gui_task = is_gui_task(message);
        let mut takeover_guard = if gui_task {
            self.thought(
                "takeover",
                "屏幕接管",
                "已接管屏幕并阻断外界键鼠输入，按 Ctrl+Shift+F12 可紧急解除",
            );
            crate::takeover::engage_screen(self.app, Some(message));
            Some(TakeoverGuard(self.app))
        } else {
            None
        };

        // 轮次不再有上限：仅靠循环检测（陷入重复才终止）与用户取消来结束
        let mut loop_detector = LoopDetector::new();
        // 本轮真实工具失败记录（用户拒绝/闸门占位不计）——任务收尾时提炼经验教训用
        let mut tool_failures: Vec<String> = Vec::new();
        // 本轮是否用过工具（纯聊天不存任务记忆，只有真正执行过任务的才值得记）
        let mut used_tools = false;

        loop {
            if self.state.cancel.load(Ordering::SeqCst) {
                return Ok("已停止。".to_string());
            }
            self.phase(AgentPhase::Executing);
            // 接管看门狗心跳：本轮仍活跃
            crate::takeover::touch_activity();
            let resp = match cancellable_stream_chat(self.app, self.state, &messages, &tools).await {
                Ok(r) => r,
                Err(e) if e == CANCELLED => return Ok("已停止。".to_string()),
                Err(e) => return Err(e),
            };

            // 模型只回文本 → 反思并结束（content 已流式推送到前端）
            if resp.tool_calls.is_none() {
                self.phase(AgentPhase::Reflecting);
                // 经验复盘（反思机制落地）：本轮发生过工具失败且最终完成 →
                // 提炼「遇到的问题→解决办法」写入经验库，下次同类任务自动召回复用
                if !tool_failures.is_empty() {
                    self.reflect_lessons(message, &tool_failures).await;
                }
                // 成功操作配方沉淀：GUI 操作链 ≥3 步的顺利任务 → 提炼「怎么成功的」
                // 写入配方库，下次操作同类应用直接照用（规划好就一路跑完）
                self.reflect_recipe(message).await;
                // 同类任务结果记忆：真正执行过的任务存「任务→结果要点」，
                // 下次相似指令直接参考上次结果——高频/重复任务越用越快
                if used_tools {
                    let task_brief: String = message.chars().take(60).collect();
                    let digest: String = resp
                        .content
                        .clone()
                        .unwrap_or_default()
                        .chars()
                        .take(180)
                        .collect();
                    if message.chars().count() >= 6 && !digest.is_empty() {
                        let _ = self.state.store.smart_remember(
                            &format!("任务「{task_brief}」的结果要点：{digest}"),
                            "task",
                        );
                    }
                }
                self.phase(AgentPhase::Done);
                return Ok(append_chat_cards(finalize(resp.content)));
            }

            // 有工具调用：这轮流式内容只是「过渡思考」，清空流式片段，
            // 但把「过渡思考」作为「话语」保留进执行流，避免被后续执行流覆盖
            let _ = self.app.emit("chat-round-reset", json!({}));
            let spoken = resp.content.clone().unwrap_or_default();
            if !spoken.trim().is_empty() {
                self.thought("saying", "话语", &spoken);
            }
            let calls = resp.tool_calls.clone().unwrap();
            used_tools = true;

            // 循环检测：以「本轮工具调用指纹」为基准，判断是否陷入重复无效操作。
            // 观察类工具（截图/读树等）不参与指纹；若本轮全是观察类，指纹为空则直接跳过检测。
            // 命中即终止并提示用户，避免无意义空转；未命中则继续执行到任务完成。
            let fp = round_fingerprint(&calls);
            if !fp.is_empty() && loop_detector.record(fp) {
                self.phase(AgentPhase::Reflecting);
                let msg = "检测到执行陷入循环（反复执行相同的操作），已自动停止以避免空转。请检查任务描述或调整需求后重试。";
                self.thought("loop", "检测到循环", msg);
                return Ok(msg.to_string());
            }

            messages.push(ChatMessage {
                role: "assistant".into(),
                content: resp.content.unwrap_or_default(),
                tool_calls: Some(calls.clone()),
                tool_call_id: None,
            });

            // 并行子代理：本轮若同时派发多个 spawn_subagent，则并发执行以缩短总耗时，
            // 结果按 call_id 暂存，下方循环直接取用（不再重复单个执行）。
            let mut subagent_results: HashMap<String, Value> = HashMap::new();
            let mut destructive_done = false;
            {
                let mut spawn_specs: Vec<(String, crate::subagent::SubAgentType, String)> = Vec::new();
                for call in &calls {
                    if call["function"]["name"].as_str() != Some("spawn_subagent") {
                        continue;
                    }
                    let call_id = call["id"].as_str().unwrap_or("").to_string();
                    let args = parse_args(call["function"]["arguments"].clone())?;
                    let Ok(agent_type) = crate::subagent::parse_agent_type(&args) else {
                        continue;
                    };
                    let Some(task) = args["task"].as_str().map(|s| s.to_string()) else {
                        continue;
                    };
                    spawn_specs.push((call_id, agent_type, task));
                }
                if spawn_specs.len() > 1 {
                    self.thought(
                        "subagent",
                        "并行子代理",
                        &format!("并发执行 {} 个子任务", spawn_specs.len()),
                    );
                    let specs: Vec<_> = spawn_specs
                        .iter()
                        .map(|(_, t, task)| (*t, task.clone()))
                        .collect();
                    let results = crate::subagent::execute_subagents_parallel(
                        specs,
                        self.state.model.clone(),
                        self.state.tools.clone(),
                        crate::subagent::SubAgentTrace::enabled(self.app.clone()),
                    )
                    .await;
                    for (i, (call_id, _, _)) in spawn_specs.iter().enumerate() {
                        if let Some(r) = results.get(i) {
                            subagent_results.insert(
                                call_id.clone(),
                                json!({
                                    "ok": r.success,
                                    "agent_type": r.agent_type,
                                    "summary": r.summary,
                                    "files_examined": r.files_examined,
                                    "duration_ms": r.duration_ms,
                                }),
                            );
                        }
                    }
                }
            }

            // 逐个执行工具调用
            for call in calls {
                if self.state.cancel.load(Ordering::SeqCst) {
                    return Ok("已停止。".to_string());
                }
                let call_id = call["id"].as_str().unwrap_or("").to_string();
                let name = call["function"]["name"].as_str().unwrap_or("").to_string();
                let args: Value = parse_args(call["function"]["arguments"].clone())?;

                self.thought("tool_call", &format!("调用工具 · {name}"), &args.to_string());
                println!("[工具调用] {name} {}", args);

                // GUI 交互工具触发自动接管：消息没命中 GUI 关键词、但模型实际
                // 开始做界面操作时，同样阻断外界键鼠并弹出步骤弹幕
                if takeover_guard.is_none() && !crate::takeover::is_active() {
                    const GUI_AUTO_TAKEOVER_TOOLS: &[&str] = &[
                        "click_element", "click_at", "mouse_click", "mouse_drag",
                        "key_press", "key_down", "key_up", "type_text", "paste_text",
                        "save_dialog", "wait_ui_stable", "macro",
                        "close_popup", "takeover", "window_minimize_all",
                        "window_focus", "window_set_topmost", "window_prepare", "show_step",
                    ];
                    if GUI_AUTO_TAKEOVER_TOOLS.contains(&name.as_str()) {
                        self.thought(
                            "takeover",
                            "屏幕接管",
                            "GUI 操作触发自动接管并阻断外界键鼠输入（Ctrl+Shift+F12 解除）",
                        );
                        crate::takeover::engage_screen(self.app, Some(message));
                        takeover_guard = Some(TakeoverGuard(self.app));
                    }
                }
                // 接管期间：每步操作实时推送到桌面弹幕（不依赖模型主动调 show_step）
                if crate::takeover::is_active() {
                    let brief: String = args.to_string().chars().take(64).collect();
                    crate::windows::push_step(self.app, &format!("▶ {} {}", name, brief));
                }

                let tool = match self.state.tools.get(&name) {
                    Some(t) => t.clone(),
                    None => {
                        // 未知工具（模型幻觉了不存在的工具名，如 bash）：不终止任务，
                        // 回传错误 + 提示正确工具，让模型下一轮自我纠正
                        let hint = match name.as_str() {
                            "bash" | "sh" | "shell" | "zsh" | "terminal" => {
                                "（执行命令请改用 ps_exec 或 run_command 工具）"
                            }
                            "screenshot" | "screenshot_tool" => "（截屏请改用 capture_screen）",
                            "search" | "websearch" | "web_search_tool" => {
                                "（搜索请改用 web_search）"
                            }
                            _ => "（请从系统给定的可用工具列表中选择正确工具重试）",
                        };
                        let output = json!({ "error": format!("未知工具: {name}{hint}") });
                        self.thought(
                            "tool_result",
                            &format!("工具失败 · {name}"),
                            &output.to_string(),
                        );
                        if crate::takeover::is_active() {
                            crate::windows::push_step(self.app, &format!("✕ {name}"));
                        }
                        let _ = self.state.store.add_audit(&AuditEntry {
                            ts: now_ms(),
                            subject: "assistant".to_string(),
                            tool: name.clone(),
                            args: args.clone(),
                            decision: "unknown-tool".to_string(),
                            result: output.to_string(),
                        });
                        messages.push(ChatMessage {
                            role: "tool".into(),
                            content: output.to_string(),
                            tool_calls: None,
                            tool_call_id: Some(call_id),
                        });
                        continue;
                    }
                };
                let class = tool.permission();
                // 回退原则第 2 级：破坏性点击目标（删除/清空/卸载/发送…）强制高危审批，
                // 即使工具本身声明为只读——不可逆操作必须逐次经用户确认
                let args_text = args.to_string();
                let destructive = matches!(
                    name.as_str(),
                    "click_element" | "click_at" | "mouse_click"
                ) && DESTRUCTIVE_WORDS.iter().any(|w| args_text.contains(w));
                let class = if destructive {
                    crate::tools::PermissionClass::HighRisk
                } else {
                    class
                };

                // 权限决策：只读/一般读写/已记住 → 直接执行；系统路径写/高危/未记住 → 推前端 + 升级通知
                let mut denied_by_remember = false;
                let decision = match self.state.security.classify(&name, &args, class) {
                    PermissionDecision::AutoAllow => "auto-allow".to_string(),
                    PermissionDecision::AutoDeny => {
                        denied_by_remember = true;
                        "denied".to_string()
                    }
                    PermissionDecision::Prompt(req) => {
                        self.phase(AgentPhase::WaitingApproval);
                        let _ = self.app.emit("permission-request", &req);

                        // 生成动态通知消息：让模型用上下文生成人性化的审批理由
                        let recent = extract_recent_context(&messages);
                        let (what, detail) = crate::notify::generate_approval_message(
                            &self.state.model,
                            message,
                            &name,
                            &args,
                            &recent,
                        )
                        .await;

                        // IM 二次确认：经消息总线向最近活跃通道推送审批（回复「允许/拒绝」即 resolve）
                        let channels = self
                            .state
                            .im_bus
                            .push_approval(&req.id, &what, &detail)
                            .await;
                        // 回传本次审批实际推送的通道，供前端在审批卡上标注（无通道则仅桌面端审批）
                        let _ = self.app.emit(
                            "permission-channel",
                            json!({ "approval_id": req.id, "channels": channels }),
                        );

                        if wait_for_decision_with_escalation(
                            self.app,
                            self.state,
                            &self.state.security,
                            &req.id,
                            &what,
                            &detail,
                        )
                        .await
                        {
                            "approved".to_string()
                        } else {
                            "denied".to_string()
                        }
                    }
                };

                // 并行子代理的预计算结果（spawn_subagent 且本轮已并发执行过）
                let precomputed = if name == "spawn_subagent" {
                    subagent_results.remove(&call_id)
                } else {
                    None
                };

                // 回退原则第 2 级：破坏性调用执行后，本轮剩余的批量调用不再执行，
                // 以占位结果回填（保持 tool_calls 协议完整），交由模型重新评估后再继续
                let tool_t0 = std::time::Instant::now();
                let output = if destructive_done {
                    json!({
                        "error": "破坏性操作闸门：本轮批量已暂停（前序执行了删除/清空类操作），此调用未执行。请先截屏确认当前界面状态，再决定是否继续"
                    })
                } else if decision == "denied" {
                    if denied_by_remember {
                        json!({ "error": "用户此前已记住拒绝此类操作，本次自动拒绝" })
                    } else {
                        json!({ "error": "用户拒绝了此操作" })
                    }
                } else if let Some(pre) = precomputed {
                    pre
                } else {
                    // 工具可能阻塞（如 browser_search 最多 3 引擎 × 12s），
                    // 放到 blocking 线程池执行，并随时响应「停止」取消信号。
                    let tool = tool.clone();
                    let args_for_run = args.clone();
                    tokio::select! {
                        r = tokio::task::spawn_blocking(move || tool.run(args_for_run)) => {
                            match r {
                                Ok(res) => res.unwrap_or_else(|e| json!({ "error": e })),
                                Err(e) => json!({ "error": format!("工具执行失败: {e}") }),
                            }
                        }
                        _ = wait_cancel(self.state) => {
                            return Ok("已停止。".to_string());
                        }
                    }
                };

                if destructive && decision != "denied" {
                    destructive_done = true;
                    self.thought(
                        "permission",
                        "破坏性操作 · 批量已暂停",
                        "检测到删除/清空类目标：已强制审批并暂停本轮批量，剩余步骤将逐步确认",
                    );
                }

                // 耗时可视化：label 直接带人类可读耗时（回放/审计自动兼容），payload 附 duration_ms
                let tool_ms = tool_t0.elapsed().as_millis() as u64;
                let dur_txt = if tool_ms >= 1000 {
                    format!("{:.1}s", tool_ms as f64 / 1000.0)
                } else {
                    format!("{tool_ms}ms")
                };
                let result_label = format!("工具完成 · {name} · {dur_txt}");
                let _ = self.app.emit(
                    "thought",
                    json!({
                        "kind": "tool_result",
                        "label": result_label,
                        "detail": output.to_string(),
                        "duration_ms": tool_ms,
                    }),
                );
                self.state.log_thought("tool_result", &result_label, &output.to_string());
                // 接管看门狗心跳：工具完成仍活跃
                crate::takeover::touch_activity();

                // 接管期间：结果状态推弹幕（✓/✕），点击类工具在目标位置闪一圈光环
                if crate::takeover::is_active() {
                    let ok = output.get("error").is_none()
                        && output.get("ok").and_then(|v| v.as_bool()).unwrap_or(true);
                    crate::windows::push_step(
                        self.app,
                        &format!("{} {}", if ok { "✓" } else { "✕" }, name),
                    );
                    if ok
                        && matches!(
                            name.as_str(),
                            "click_at" | "mouse_click" | "click_element" | "mouse_drag" | "close_popup"
                                | "wheel_scroll" | "hover" | "paste_text" | "type_text" | "macro"
                        )
                    {
                        let (cx, cy) = crate::capability::windows::cursor_pos();
                        crate::windows::halo_flash_at(self.app, cx, cy);
                    }
                    // 成功 GUI 操作链记录（配方沉淀素材）：任务顺利完成后提炼成可复用配方
                    if ok {
                        const GUI_OPS: &[&str] = &[
                            "click_element", "click_at", "mouse_click", "mouse_drag",
                            "key_press", "type_text", "paste_text", "wheel_scroll", "hover",
                            "close_popup", "save_dialog", "explorer_open", "launch_app",
                            "middle_click",
                        ];
                        if GUI_OPS.contains(&name.as_str()) {
                            let brief = condense_gui_args(&name, &args);
                            let mut ops = self.gui_ops.lock().unwrap();
                            if ops.len() < 30 {
                                ops.push(format!("{name}{brief}"));
                            }
                        }
                        if name == "window_prepare" {
                            if let Some(n) = args.get("name").and_then(|v| v.as_str()) {
                                *self.gui_target.lock().unwrap() = Some(n.to_string());
                            }
                        }
                    }
                }

                // GUI 关键帧留档：写操作成功且未拒绝时自动截屏，供失败时回看定位
                let tool_ok = output.get("error").is_none()
                    && output.get("ok").and_then(|v| v.as_bool()).unwrap_or(true);
                if tool_ok && crate::replay::is_gui_action_tool(&name) {
                    let cap = self.state.capability.clone();
                    let label = name.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        crate::replay::record_keyframe(cap.as_ref(), &label);
                    })
                    .await;
                }
                // 操作日志：gui_undo 的回退依据（记录工具名 + 完整参数 JSON，供 toggle/steps 逆操作复用）
                if tool_ok && crate::replay::is_gui_action_tool(&name) {
                    crate::replay::record_gui_op(&name, &args.to_string());
                }

                // 经验复盘素材：记录真实工具失败（用户拒绝/破坏性闸门占位不计——
                // 那是刻意拦截不是「踩坑」），任务收尾时提炼「问题→解法」
                if let Some(err) = output.get("error").and_then(|v| v.as_str()) {
                    if !err.contains("拒绝") && !err.contains("破坏性操作闸门") {
                        let brief: String =
                            format!("{name}: {err}").chars().take(200).collect();
                        tool_failures.push(brief);
                    }
                }

                // 审计（SQLite 持久化）—— 保留完整结果以保证透明
                let _ = self.state.store.add_audit(&AuditEntry {
                    ts: now_ms(),
                    subject: "assistant".to_string(),
                    tool: name.clone(),
                    args: args.clone(),
                    decision,
                    result: output.to_string(),
                });

                // Token 节约：喂给模型的工具结果做「首尾保留」截断（审计与前端展示仍为完整内容）
                let model_output = crate::token_saver::cap_tool_result(&output.to_string());

                messages.push(ChatMessage {
                    role: "tool".into(),
                    content: model_output,
                    tool_calls: None,
                    tool_call_id: Some(call_id),
                });
            }

            // 回退原则第 1 级：批尾预期状态校验。
            // 模型在本轮批量里调用了 set_expected_state → 自动截屏 + 取元素树 + 视觉描述，
            // 交给模型比对「是否变成预期的样子」而非只看「变了没」；不符立即叫停。
            if let Some(expected) = take_expected_state() {
                let cap = self.state.capability.clone();
                let cap2 = self.state.capability.clone();
                let (ui_tree, shot) = tokio::task::spawn_blocking(move || {
                    let tree = cap.interactive_map(None).ok();
                    let path = cap2
                        .capture_screen()
                        .ok()
                        .map(|s| s.path)
                        .unwrap_or_default();
                    (tree, path)
                })
                .await
                .unwrap_or((None, String::new()));

                // 视觉链路可用时把截图描述一并给校验器（失败静默降级到元素树）
                let vision_desc = if !shot.is_empty() {
                    let shot = shot.clone();
                    tokio::task::spawn_blocking(move || {
                        crate::visual_grounding::describe_image(
                            &shot,
                            "描述当前屏幕界面状态：窗口标题、可见的主要区域、列表/文本内容、弹窗等",
                        )
                        .ok()
                    })
                    .await
                    .unwrap_or(None)
                } else {
                    None
                };

                let mut prompt = format!(
                    "你是 GUI 执行校验器。刚执行完一批界面操作，请判断当前界面是否达到了「预期状态」。\
                     只看事实，不要臆测；比对的是正确性（变成预期样子），不是存在性（界面有变化）。\n\n\
                     【预期状态】{expected}\n\n"
                );
                if let Some(desc) = &vision_desc {
                    prompt.push_str(&format!("【当前屏幕（视觉描述）】\n{desc}\n\n"));
                }
                if let Some(tree) = &ui_tree {
                    let tree_text = tree.to_string();
                    let trimmed: String = tree_text.chars().take(2500).collect();
                    prompt.push_str(&format!("【当前界面元素树（截取）】\n{trimmed}\n\n"));
                }
                if vision_desc.is_none() && ui_tree.is_none() {
                    prompt.push_str("（视觉与元素树均不可用，请基于截屏路径标注说明无法核验）\n\n");
                }
                prompt.push_str(
                    "请只输出一个 JSON 对象，格式：{\"match\": true/false, \"reason\": \"30字以内说明\"}",
                );

                let msgs = vec![ChatMessage {
                    role: "user".into(),
                    content: prompt,
                    tool_calls: None,
                    tool_call_id: None,
                }];
                let verdict = match self.state.model.chat(&msgs, &[]).await {
                    Ok(resp) => resp.content.unwrap_or_default(),
                    Err(e) => format!("{{\"match\": false, \"reason\": \"校验器调用失败: {e}\"}}"),
                };
                // 解析校验器输出的 JSON（容错：提取首个 {...} 块）
                let parsed: Option<(bool, String)> = {
                    let start = verdict.find('{');
                    let end = verdict.rfind('}');
                    match (start, end) {
                        (Some(s), Some(e)) if e > s => serde_json::from_str::<Value>(&verdict[s..=e])
                            .ok()
                            .map(|v| {
                                (
                                    v["match"].as_bool().unwrap_or(false),
                                    v["reason"]
                                        .as_str()
                                        .unwrap_or("无说明")
                                        .to_string(),
                                )
                            }),
                        _ => None,
                    }
                };
                let (matched, reason) = parsed.unwrap_or((false, "校验器输出无法解析".into()));
                self.thought(
                    "verify",
                    if matched { "预期校验 · 通过" } else { "预期校验 · 不符" },
                    &reason,
                );
                messages.push(ChatMessage {
                    role: "user".into(),
                    content: if matched {
                        format!("【预期状态校验 · 通过】{reason}。预期已达成，可继续后续步骤或收尾汇报。")
                    } else {
                        format!(
                            "【预期状态校验 · 不符】预期：{expected}；核验结果：{reason}。\
                             立即停止批量推进，退回逐步模式：先 capture_screen 观察，\
                             修正后单步执行并重新 set_expected_state 登记新预期。\
                             也可调用 gui_undo（action=toggle/back/last/steps）回退刚才的操作。"
                        )
                    },
                    tool_calls: None,
                    tool_call_id: None,
                });
            }
        }
    }
}

/// 带升级通知的审批等待：启动升级链，用户在超时前响应则返回决定
/// `what`: 简短标题（Agent 动态生成或回退）
/// `detail`: 带上下文的人性化消息（Agent 动态生成或回退）
async fn wait_for_decision_with_escalation(
    app: &AppHandle,
    state: &AppState,
    security: &SecurityManager,
    id: &str,
    what: &str,
    detail: &str,
) -> bool {
    // 启动通知升级
    let first_timeout = state.escalation.start_escalation(app, id, what, detail);

    // 如果升级被禁用，使用简化超时
    let timeout = if first_timeout == Duration::MAX {
        Duration::from_secs(60)
    } else {
        APPROVAL_TIMEOUT
    };

    let deadline = Instant::now() + timeout;
    loop {
        if let Some(d) = security.decision(id) {
            // 用户已响应，取消升级
            state.escalation.cancel_escalation(id);
            return d;
        }
        if state.cancel.load(Ordering::SeqCst) {
            state.escalation.cancel_escalation(id);
            return false;
        }
        if Instant::now() > deadline {
            state.escalation.cancel_escalation(id);
            return false; // 超时默认拒绝
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Ollama 的 tool_calls.arguments 通常是 JSON 字符串，需解析；也可能是对象
fn parse_args(v: Value) -> Result<Value, String> {
    match v {
        Value::String(s) => serde_json::from_str(&s).map_err(|e| e.to_string()),
        other => Ok(other),
    }
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// 若模型返回空内容，给一个默认回复（避免前端显示空气泡）
fn finalize(content: Option<String>) -> String {
    let c = content.unwrap_or_default();
    if c.trim().is_empty() {
        "已根据你的要求完成任务。".to_string()
    } else {
        c
    }
}

// ───── chat_card 万能聊天卡片 ─────
// 模型用 chat_card 工具推送 HTML 卡片（天气/日程/比分/系统状态等结构化展示），
// 卡片以 ```chat_card 围栏块（JSON）附加到回复末尾——随消息持久化，前端解析渲染。

static CHAT_CARDS: OnceLock<Mutex<Vec<Value>>> = OnceLock::new();

fn push_chat_card(card: Value) {
    let slot = CHAT_CARDS.get_or_init(|| Mutex::new(Vec::new()));
    if let Ok(mut g) = slot.lock() {
        g.push(card);
    }
}

fn take_chat_cards() -> Vec<Value> {
    let slot = CHAT_CARDS.get_or_init(|| Mutex::new(Vec::new()));
    slot.lock()
        .ok()
        .map(|mut g| std::mem::take(&mut *g))
        .unwrap_or_default()
}

/// 把本轮收集的卡片序列化为围栏块，附加到回复文本末尾（无卡片返回原文）
fn append_chat_cards(text: String) -> String {
    let cards = take_chat_cards();
    if cards.is_empty() {
        return text;
    }
    let mut out = text;
    for card in cards {
        out.push_str("\n\n```chat_card\n");
        out.push_str(&serde_json::to_string(&card).unwrap_or_default());
        out.push_str("\n```");
    }
    out
}

/// chat_card：在聊天中渲染一张可自由排版的「万能卡片」
pub struct ChatCardTool;

impl crate::tools::Tool for ChatCardTool {
    fn name(&self) -> &str {
        "chat_card"
    }
    fn description(&self) -> &str {
        "在聊天框中嵌入一张「万能卡片」，以精美的可视化卡片展示结构化信息（天气/日程/比赛比分/行情/系统状态/对比表等）。\
         html 字段为卡片主体的完整 HTML 片段：支持内联 style（渐变背景/圆角/阴影/flex-grid 布局）、emoji 图标、表格、以及 https:// 外链图片。\
         width 控制卡片宽度（如 \"340px\" 或 \"100%\"，默认 100%），height 控制高度（默认自适应）。\
         排版要美观克制：深色友好配色、适当留白、信息分层；一次任务可推送多张卡片"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "html": { "type": "string", "description": "卡片主体 HTML 片段（内联样式自由排版）" },
                "title": { "type": "string", "description": "卡片标题（可选，显示在卡片顶部栏）" },
                "width": { "type": "string", "description": "卡片宽度 CSS 值，默认 100%，如 \"340px\"" },
                "height": { "type": "string", "description": "卡片固定高度 CSS 值（可选，默认按内容自适应）" }
            },
            "required": ["html"]
        })
    }
    fn permission(&self) -> crate::tools::PermissionClass {
        crate::tools::PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let html = args["html"]
            .as_str()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .ok_or("缺少 html 参数")?;
        if html.len() > 30_000 {
            return Err("html 过长（上限 30KB）".into());
        }
        let card = json!({
            "title": args["title"].as_str().unwrap_or(""),
            "html": html,
            "width": args["width"].as_str().unwrap_or("100%"),
            "height": args["height"].as_str().unwrap_or(""),
        });
        push_chat_card(card);
        Ok(json!({ "ok": true, "note": "卡片已生成，将随本轮回复一起显示在聊天框中" }))
    }
}

/// 可取消的「指定 tier」模型调用（用于规划/审查用强模型）
pub(crate) async fn cancellable_chat_with_tier(
    state: &AppState,
    tier: crate::model::ModelTier,
    messages: &[ChatMessage],
    tools: &[Value],
) -> Result<ChatResponse, String> {
    tokio::select! {
        r = state.model.chat_with_tier(tier, messages, tools) => r,
        _ = wait_cancel(state) => Err(CANCELLED.to_string()),
    }
}

/// 可取消的流式模型调用：边生成边 emit chat-token，cancel 置位时中断
async fn cancellable_stream_chat(
    app: &AppHandle,
    state: &AppState,
    messages: &[ChatMessage],
    tools: &[Value],
) -> Result<ChatResponse, String> {
    tokio::select! {
        r = stream_chat_with_emit(app, state, messages, tools) => r,
        _ = wait_cancel(state) => Err(CANCELLED.to_string()),
    }
}

/// 流式模型调用：把 content 片段实时 emit 到前端 chat-token 事件
async fn stream_chat_with_emit(
    app: &AppHandle,
    state: &AppState,
    messages: &[ChatMessage],
    tools: &[Value],
) -> Result<ChatResponse, String> {
    state
        .model
        .stream_chat(messages, tools, &|token: &str| {
            let _ = app.emit("chat-token", json!({ "token": token }));
        })
        .await
}

/// 等待取消标志置位（用于 select! 的取消分支）
async fn wait_cancel(state: &AppState) {
    loop {
        if state.cancel.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

/// 从消息历史中提取最近几条工具调用结果，作为上下文供通知消息生成
fn extract_recent_context(messages: &[ChatMessage]) -> String {
    let mut parts = Vec::new();
    // 跳过 system 消息，取最近 5 条非用户消息
    for m in messages.iter().rev().take(10) {
        if m.role == "tool" {
            let preview = m.content.chars().take(80).collect::<String>();
            if !preview.is_empty() {
                parts.push(format!("已完成: {preview}"));
            }
        } else if m.role == "assistant" && m.tool_calls.is_some() {
            parts.push("正在调用工具…".to_string());
        }
    }
    if parts.is_empty() {
        "刚开始执行任务".to_string()
    } else {
        parts.reverse();
        parts.join("；")
    }
}
