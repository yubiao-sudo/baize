//! 桌面级快捷操作与应用快捷键知识库。
//!
//! 两个工具：
//! - `system_action`：把常用系统动作（锁屏 / 设置页 / 截图 / 任务管理器 / 显示桌面 …）
//!   做成**命名动作**。比让模型自己拼 `win+shift+s` 这类组合键可靠——有些动作根本不是
//!   按键能完成的（锁屏走 LockWorkStation 远比模拟 Win+L 稳）。
//! - `app_shortcuts`：常见应用的默认快捷键查询表。模型记错了就会乱按，
//!   所以「不确定就先查表」比「凭印象按」可靠。
//!
//! 表里的快捷键都是各应用的**默认值**；用户自定义过的会不生效，
//! 这一点在工具输出里明确告知模型，避免它把「按了没反应」判成工具坏了。

use serde_json::{json, Value};
use std::sync::Arc;

use crate::capability::{Action, Capability};
use crate::tools::{silent_command, PermissionClass, Tool};

// ───────────────────────── 系统动作 ─────────────────────────

/// 一个系统动作的落地方式
enum SysImpl {
    /// 模拟按键（走 Capability，与 key_press 同一条注入链路）
    Keys(&'static str),
    /// 启动一个 shell 目标（start / explorer / rundll32）
    Shell(&'static str),
    /// 需要拼参数的 shell 目标（设置页）
    ShellWithArg(&'static str),
    /// 锁屏：专门 API，不用 Win+L
    Lock,
}

fn sys_action(action: &str) -> Option<(SysImpl, &'static str)> {
    let a = action.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    let v = match a.as_str() {
        // 会改变屏幕状态的（需要提前告知用户）
        "lock" | "lock_screen" | "锁屏" => (SysImpl::Lock, "锁屏"),

        // 打开系统面板 / 工具
        "screenshot" | "snip" | "截图" => (SysImpl::Shell("ms-screenclip:"), "系统截图（框选区域）"),
        "settings" | "设置" => (SysImpl::ShellWithArg("ms-settings:"), "Windows 设置"),
        "recycle_bin" | "回收站" => (
            SysImpl::Shell("shell:RecycleBinFolder"),
            "回收站",
        ),
        "run" | "run_dialog" | "运行" => (SysImpl::Keys("win+r"), "运行对话框"),
        "explorer" | "文件资源管理器" => (SysImpl::Keys("win+e"), "文件资源管理器"),
        "task_manager" | "任务管理器" => (SysImpl::Keys("ctrl+shift+esc"), "任务管理器"),
        "task_view" | "任务视图" => (SysImpl::Keys("win+tab"), "任务视图"),
        "show_desktop" | "desktop" | "显示桌面" => (SysImpl::Keys("win+d"), "显示桌面"),
        "switch_window" | "切换窗口" => (SysImpl::Keys("alt+tab"), "切换窗口"),
        "close_window" | "关闭窗口" => (SysImpl::Keys("alt+f4"), "关闭当前窗口"),
        "clipboard_history" | "剪贴板" => (SysImpl::Keys("win+v"), "剪贴板历史"),
        "emoji" | "表情" => (SysImpl::Keys("win+."), "表情 / 符号面板"),
        "notification_center" | "通知中心" => (SysImpl::Keys("win+a"), "通知中心 / 快捷设置"),
        "search" | "搜索" => (SysImpl::Keys("win+s"), "系统搜索"),
        "magnifier" | "放大镜" => (SysImpl::Keys("win++"), "放大镜（再按一次放大）"),
        "magnifier_out" | "缩小" => (SysImpl::Keys("win+-"), "放大镜缩小"),
        "quick_link" | "快捷菜单" => (SysImpl::Keys("win+x"), "快速链接菜单（设备管理器/终端/设置）"),
        "project" | "投影" => (SysImpl::Keys("win+p"), "投影 / 多显示器"),
        "start_menu" | "开始菜单" => (SysImpl::Keys("win"), "开始菜单"),
        "volume_up" | "音量增" => (SysImpl::Keys("volume_up"), "音量 +"),
        "volume_down" | "音量减" => (SysImpl::Keys("volume_down"), "音量 -"),
        "volume_mute" | "静音" => (SysImpl::Keys("volume_mute"), "静音切换"),
        "play_pause" | "播放暂停" => (SysImpl::Keys("media_play_pause"), "播放 / 暂停"),
        "next_track" | "下一首" => (SysImpl::Keys("media_next"), "下一首"),
        "prev_track" | "上一首" => (SysImpl::Keys("media_prev"), "上一首"),
        _ => return None,
    };
    Some(v)
}

/// 支持的 action 名称清单（写进 schema 描述，模型才知道有哪些）
const SYS_ACTIONS: &[&str] = &[
    "lock", "screenshot", "settings", "recycle_bin", "run", "explorer", "task_manager",
    "task_view", "show_desktop", "switch_window", "close_window", "clipboard_history",
    "emoji", "notification_center", "search", "magnifier", "magnifier_out", "quick_link",
    "project", "start_menu", "volume_up", "volume_down", "volume_mute", "play_pause",
    "next_track", "prev_track",
];

pub struct SystemActionTool {
    capability: Arc<dyn Capability>,
}

impl SystemActionTool {
    pub fn new(capability: Arc<dyn Capability>) -> Self {
        Self { capability }
    }
}

/// `start "" "<target>" [extra]`：走 ShellExecute 语义，能把 exe / URI 协议 / .msc / 文件夹一起处理。
/// 用 `raw_arg` 是为了绕开 Rust 的参数再转义——URL 里带 `&` 时被 cmd 拆开是常见事故。
pub(crate) fn shell_start(target: &str) -> Result<(), String> {
    shell_start_with(target, "")
}

pub(crate) fn shell_start_with(target: &str, extra: &str) -> Result<(), String> {
    let mut line = format!("/c start \"\" \"{target}\"");
    let extra = extra.trim();
    if !extra.is_empty() {
        line.push(' ');
        line.push_str(extra);
    }
    let mut cmd = silent_command("cmd");
    {
        use std::os::windows::process::CommandExt;
        cmd.raw_arg(line);
    }
    cmd.spawn().map(|_| ()).map_err(|e| format!("启动失败: {e}"))
}

impl Tool for SystemActionTool {
    fn name(&self) -> &str {
        "system_action"
    }
    fn description(&self) -> &str {
        "执行系统级动作（命名动作，不用自己拼快捷键）：锁屏 lock、截图 screenshot、打开设置页 settings(page=display/privacy/network/windowsupdate…)、\
         回收站 recycle_bin、运行对话框 run、资源管理器 explorer、任务管理器 task_manager、任务视图 task_view、显示桌面 show_desktop、\
         切换窗口 switch_window、关闭窗口 close_window、剪贴板历史 clipboard_history、表情 emoji、通知中心 notification_center、\
         系统搜索 search、放大镜 magnifier/magnifier_out、快捷链接菜单 quick_link、投影 project、开始菜单 start_menu、\
         音量 volume_up/volume_down/volume_mute、媒体 play_pause/next_track/prev_track。\
         lock 会把屏幕锁掉、白泽随后无法继续桌面操作（需用户自己解锁），执行前先确认用户意图"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "description": format!("动作名：{}", SYS_ACTIONS.join(" / "))
                },
                "page": {
                    "type": "string",
                    "description": "仅 settings 用：设置页标识，如 display、sound、bluetooth、network、appsfeatures、windowsupdate、privacy、personalization、about；省略则打开设置首页"
                }
            },
            "required": ["action"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let action = args["action"].as_str().ok_or("缺少参数 action")?.trim();
        let (imp, label) = sys_action(action).ok_or_else(|| {
            format!(
                "未知系统动作「{action}」，可用：{}",
                SYS_ACTIONS.join(" / ")
            )
        })?;

        let mut via = "keys";
        match imp {
            SysImpl::Keys(keys) => {
                let r = self
                    .capability
                    .act(&Action::KeyPress {
                        keys: keys.to_string(),
                    })
                    .map_err(|e| e.to_string())?;
                if !r.ok {
                    return Err(format!("{label} 按键注入失败"));
                }
            }
            SysImpl::Shell(target) => {
                shell_start(target)?;
                via = "shell";
            }
            SysImpl::ShellWithArg(prefix) => {
                let page = args["page"]
                    .as_str()
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .unwrap_or("");
                // 只放行形如 display / appsfeatures 的页面标识，避免把任意串拼进 URI
                let page = if page
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-')
                {
                    page
                } else {
                    ""
                };
                shell_start(&format!("{prefix}{page}"))?;
                via = "shell";
            }
            SysImpl::Lock => {
                silent_command("rundll32.exe")
                    .arg("user32.dll,LockWorkStation")
                    .spawn()
                    .map_err(|e| format!("锁屏失败: {e}"))?;
                via = "shell";
            }
        }

        // 锁屏后不要再让模型以为还能接着操作桌面
        let note = if matches!(action.to_ascii_lowercase().as_str(), "lock" | "lock_screen" | "锁屏") {
            "已发出锁屏命令：屏幕会立刻锁上，桌面自动化到此为止，等用户解锁后再继续"
        } else {
            "动作已发出。若目标窗口是管理员权限，Windows 会拦截模拟输入——需要用户手动点一下"
        };

        Ok(json!({
            "ok": true,
            "action": action,
            "via": via,
            "description": label,
            "note": note,
        }))
    }
}

// ───────────────────────── 应用快捷键查询 ─────────────────────────

struct AppEntry {
    key: &'static str,
    title: &'static str,
    aliases: &'static [&'static str],
    note: &'static str,
    items: &'static [(&'static str, &'static str)],
}

const COMMON_NOTE: &str = "这些是应用默认快捷键；用户可能自定义过，按下没反应就改走菜单或应用内搜索";

static APPS: &[AppEntry] = &[
    AppEntry {
        key: "windows",
        title: "Windows 系统",
        aliases: &["system", "系统", "win", "通用", "windows"],
        note: "系统级动作优先用 system_action 工具（命名动作），这些组合键用于兜底",
        items: &[
            ("Win", "开始菜单"),
            ("Win+S", "系统搜索"),
            ("Win+R", "运行对话框（敲 exe / 路径 / 命令）"),
            ("Win+E", "文件资源管理器"),
            ("Win+I", "设置"),
            ("Win+A", "通知中心 / 快捷设置（音量、亮度、WiFi）"),
            ("Win+X", "快速链接菜单（设备管理器 / 终端 / 设置 / 关机）"),
            ("Win+D", "显示桌面（再按恢复）"),
            ("Win+Tab", "任务视图"),
            ("Win+方向键", "窗口贴靠 / 最大化 / 最小化"),
            ("Win+P", "投影 / 多显示器模式"),
            ("Win+L", "锁屏"),
            ("Win+V", "剪贴板历史"),
            ("Win+Shift+S", "截图（框选区域）"),
            ("Win+Plus / Win+Minus", "放大镜放大 / 缩小"),
            ("Win+Ctrl+D", "新建虚拟桌面"),
            ("Win+Ctrl+Left / Right", "切换虚拟桌面"),
            ("Win+Ctrl+F4", "关闭当前虚拟桌面"),
            ("Alt+Tab", "切换窗口"),
            ("Alt+F4", "关闭当前窗口（焦点在桌面时是关机对话框）"),
            ("Alt+Enter", "查看选中项的属性"),
            ("Ctrl+Shift+Esc", "任务管理器"),
            ("Ctrl+Shift+Enter", "以管理员身份运行（在搜索/运行里选中目标时）"),
            ("F2", "重命名选中的文件"),
            ("PrtSc", "全屏截图到剪贴板"),
        ],
    },
    AppEntry {
        key: "chrome",
        title: "Chrome / Edge 浏览器（桌面窗口）",
        aliases: &["edge", "msedge", "chrome", "浏览器", "谷歌浏览器", "chromium"],
        note: "这些是浏览器窗口自己的快捷键；受控浏览器（CDP 通道）触发不了浏览器级快捷键，那侧要用 new_tab / close_tab / goto 工具",
        items: &[
            ("Ctrl+T", "新建标签页"),
            ("Ctrl+W", "关闭当前标签页"),
            ("Ctrl+Shift+T", "恢复刚关闭的标签页"),
            ("Ctrl+Tab / Ctrl+Shift+Tab", "切到下一个 / 上一个标签页"),
            ("Ctrl+1 ~ Ctrl+8", "跳到第 1~8 个标签页"),
            ("Ctrl+9", "跳到最后一个标签页"),
            ("Ctrl+L / Alt+D / F6", "聚焦地址栏（直接敲 URL 或搜索词）"),
            ("Ctrl+F", "页面内查找"),
            ("F5 / Ctrl+R", "刷新"),
            ("Ctrl+Shift+R", "强制刷新（忽略缓存）"),
            ("Ctrl+D", "收藏当前页"),
            ("Ctrl+Shift+B", "显示 / 隐藏书签栏"),
            ("Ctrl+H", "历史记录"),
            ("Ctrl+J", "下载内容"),
            ("Ctrl+Shift+Delete", "清除浏览数据"),
            ("Ctrl+Shift+N", "无痕窗口"),
            ("Ctrl+P", "打印"),
            ("Ctrl+S", "网页另存为"),
            ("Ctrl+Plus / Minus / 0", "放大 / 缩小 / 恢复 100%"),
            ("Alt+Left / Right", "后退 / 前进"),
            ("Ctrl+Shift+Tab", "上一个标签页"),
            ("Ctrl+Shift+A", "搜索标签页"),
            ("F12 / Ctrl+Shift+I", "开发者工具"),
            ("Shift+Esc", "浏览器任务管理器"),
        ],
    },
    AppEntry {
        key: "explorer",
        title: "文件资源管理器",
        aliases: &["资源管理器", "文件管理器", "文件夹", "此电脑", "explorer"],
        note: "路径直达优先用 Ctrl+L 或 Alt+D 敲完整路径，比逐层双击快得多",
        items: &[
            ("Ctrl+L / Alt+D", "聚焦地址栏（可敲完整路径）"),
            ("Alt+Up", "上一级目录"),
            ("Alt+Left / Right", "后退 / 前进"),
            ("Ctrl+Shift+N", "新建文件夹"),
            ("F2", "重命名"),
            ("Delete", "删除到回收站"),
            ("Shift+Delete", "永久删除（不进回收站）"),
            ("Alt+Enter", "属性"),
            ("F5", "刷新"),
            ("Ctrl+Shift+E", "展开左侧导航树到当前目录"),
            ("Ctrl+E / Ctrl+F", "搜索框"),
            ("Ctrl+N", "新窗口"),
            ("Ctrl+W", "关闭窗口"),
            ("Ctrl+滚轮", "放大 / 缩小图标"),
            ("Ctrl+Shift+1 ~ 8", "切换视图（大图标 / 列表 / 详细信息…）"),
        ],
    },
    AppEntry {
        key: "notepad",
        title: "记事本",
        aliases: &["记事本", "notepad"],
        note: "写文件内容优先用 write_file 工具；只有用户明确要求「在记事本里操作」才走 GUI",
        items: &[
            ("Ctrl+N / Ctrl+O", "新建 / 打开"),
            ("Ctrl+S / Ctrl+Shift+S", "保存 / 另存为"),
            ("Ctrl+F / Ctrl+H", "查找 / 替换"),
            ("Ctrl+G", "转到行"),
            ("Ctrl+Z / Ctrl+Y", "撤销 / 重做"),
            ("Ctrl+A", "全选"),
            ("Ctrl+Plus / Minus", "放大 / 缩小字号"),
            ("F5", "插入当前日期时间"),
        ],
    },
    AppEntry {
        key: "word",
        title: "Word（含 WPS 文字，大体一致）",
        aliases: &["word", "winword", "文档", "wps", "wps文字"],
        note: "保存用 Ctrl+S；已存在文件通常静默保存，弹「另存为」时用 save_dialog 工具填完整路径",
        items: &[
            ("Ctrl+S / F12", "保存 / 另存为"),
            ("Ctrl+O / Ctrl+N / Ctrl+P", "打开 / 新建 / 打印"),
            ("Ctrl+F / Ctrl+H / Ctrl+G", "查找 / 替换 / 定位"),
            ("Ctrl+B / I / U", "粗体 / 斜体 / 下划线"),
            ("Ctrl+E / L / R / J", "居中 / 左对齐 / 右对齐 / 两端对齐"),
            ("Ctrl+Z / Ctrl+Y", "撤销 / 重做"),
            ("Ctrl+A", "全选"),
            ("Ctrl+Enter", "插入分页符"),
            ("Ctrl+Shift+C / V", "复制 / 粘贴格式"),
            ("Ctrl+D", "字体对话框"),
        ],
    },
    AppEntry {
        key: "excel",
        title: "Excel（含 WPS 表格，大体一致）",
        aliases: &["excel", "表格", "电子表格", "wps表格"],
        note: "定位到数据边界用 Ctrl+方向键；批量填写用 paste_text 粘贴制表符分隔的多行文本最快",
        items: &[
            ("Ctrl+方向键", "跳到数据区域边界"),
            ("Ctrl+Shift+方向键", "从当前格扩展到边界（选区）"),
            ("Ctrl+Home / Ctrl+End", "回到 A1 / 跳到数据末格"),
            ("F2", "编辑当前单元格"),
            ("F4", "切换绝对 / 相对引用（编辑公式时）"),
            ("Ctrl+1", "设置单元格格式"),
            ("Alt+=", "自动求和"),
            ("Ctrl+Shift+L", "筛选开关"),
            ("Ctrl+T", "创建表"),
            ("Ctrl+PageUp / PageDown", "切换工作表"),
            ("Ctrl+; / Ctrl+Shift+;", "插入日期 / 时间"),
            ("Ctrl+Shift+加号 / Ctrl+减号", "插入 / 删除单元格（行、列）"),
            ("Ctrl+S", "保存"),
        ],
    },
    AppEntry {
        key: "powerpoint",
        title: "PowerPoint（含 WPS 演示）",
        aliases: &["ppt", "powerpoint", "演示文稿", "幻灯片", "wps演示"],
        note: COMMON_NOTE,
        items: &[
            ("F5", "从头放映"),
            ("Shift+F5", "从当前页放映"),
            ("Esc", "结束放映"),
            ("Ctrl+M", "新建幻灯片"),
            ("Ctrl+D", "复制选中对象 / 幻灯片"),
            ("Ctrl+G / Ctrl+Shift+G", "组合 / 取消组合"),
            ("Ctrl+S / F12", "保存 / 另存为"),
        ],
    },
    AppEntry {
        key: "vscode",
        title: "VS Code / Cursor 等代码编辑器",
        aliases: &["vscode", "vs code", "code", "cursor", "编辑器"],
        note: "命令面板 Ctrl+Shift+P 是万能入口：忘了快捷键就（Ctrl+Shift+P 里敲中文/英文命令名）搜",
        items: &[
            ("Ctrl+Shift+P / F1", "命令面板（万能入口）"),
            ("Ctrl+P", "按文件名快速打开"),
            ("Ctrl+Shift+F", "全局搜索"),
            ("Ctrl+F / Ctrl+H", "当前文件 查找 / 替换"),
            ("Ctrl+`", "集成终端"),
            ("Ctrl+B", "侧边栏开关"),
            ("Ctrl+Shift+E", "资源管理器视图"),
            ("Ctrl+/ ", "行注释"),
            ("Shift+Alt+A", "块注释"),
            ("Alt+Up / Down", "上下移动当前行"),
            ("Shift+Alt+Up / Down", "上下复制当前行"),
            ("Ctrl+D", "选中下一个相同词"),
            ("Ctrl+Shift+L", "选中所有相同词"),
            ("F2", "重命名符号（全项目）"),
            ("Ctrl+G", "跳到指定行"),
            ("Ctrl+\\", "拆分编辑器"),
            ("Ctrl+W", "关闭当前编辑器"),
        ],
    },
    AppEntry {
        key: "terminal",
        title: "命令提示符 / PowerShell / Windows Terminal",
        aliases: &["cmd", "powershell", "命令行", "终端", "terminal", "wt"],
        note: "cmd 里粘贴要右键或 Ctrl+V（旧版）；Windows Terminal 是 Ctrl+Shift+V",
        items: &[
            ("Ctrl+C", "中断当前命令"),
            ("Ctrl+V / 右键", "粘贴（cmd）"),
            ("Ctrl+Shift+V", "粘贴（Windows Terminal）"),
            ("Tab", "补全路径 / 命令"),
            ("↑ / ↓", "历史命令"),
            ("Ctrl+Shift+T / Ctrl+Shift+W", "新标签 / 关标签（Terminal）"),
            ("Ctrl+Tab", "切换标签（Terminal）"),
            ("Alt+Shift+D", "分屏（Terminal）"),
            ("Ctrl+L", "清屏（PowerShell）"),
        ],
    },
    AppEntry {
        key: "wechat",
        title: "微信（Windows 桌面版）",
        aliases: &["微信", "wechat", "weixin"],
        note: "全局热键在「设置 → 快捷键」里可改，用户改过就会不同；4.x 与 3.x 默认值有差异",
        items: &[
            ("Alt+A", "截图（默认）"),
            ("Ctrl+Alt+W", "显示 / 隐藏主窗口（默认）"),
            ("Ctrl+F", "在聊天窗口内查找聊天记录"),
            ("Enter", "发送消息"),
            ("Ctrl+Enter", "换行（设置里为「Enter 发送」时）"),
            ("Ctrl+A", "选择当前聊天里的全部消息"),
        ],
    },
    AppEntry {
        key: "qq",
        title: "QQ（Windows 桌面版）",
        aliases: &["qq", "腾讯qq"],
        note: "NT 版快捷键与旧版不同，用户可能改过；不确定就查应用内设置",
        items: &[
            ("Ctrl+Alt+A", "截图（旧版默认）"),
            ("Alt+S", "发送消息"),
            ("Ctrl+Enter", "换行"),
        ],
    },
    AppEntry {
        key: "photoshop",
        title: "Photoshop",
        aliases: &["ps", "photoshop", "psd"],
        note: COMMON_NOTE,
        items: &[
            ("Ctrl+T", "自由变换"),
            ("Ctrl+J", "复制图层"),
            ("Ctrl+Shift+N", "新建图层"),
            ("Ctrl+Shift+S / Ctrl+Alt+Shift+S", "另存为 / 存储为 Web 格式"),
            ("Ctrl+0 / Ctrl+1", "适合屏幕 / 实际像素"),
            ("Ctrl+Shift+I", "反选"),
            ("Ctrl+D", "取消选区"),
            ("V / M / B / T", "移动 / 选框 / 画笔 / 文字工具"),
        ],
    },
];

fn find_app(query: &str) -> Option<&'static AppEntry> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return None;
    }
    // 归一：去掉空格与连字符，便于「VS Code」「vscode」「微信截图」这类写法命中
    let qn: String = q
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect();
    let mut best: Option<(&'static AppEntry, i32)> = None;
    for a in APPS {
        let aliases: Vec<String> = a.aliases.iter().map(|x| x.to_ascii_lowercase()).collect();
        let title = a.title.to_lowercase();
        // 打分而不是「第一个命中就返回」：避免「微信」被 windows 之类先截胡
        let score = if a.key == qn || a.key == q {
            100
        } else if aliases.iter().any(|x| *x == qn || *x == q) {
            95
        } else if title.contains(&qn) {
            70
        } else if a.key.len() >= 3 && qn.contains(a.key) {
            // 「chrome浏览器」→ chrome
            60
        } else if aliases
            .iter()
            .any(|x| x.len() >= 2 && qn.contains(x.as_str()))
        {
            // 「微信截图」→ wechat
            55
        } else if qn.len() >= 3 && a.key.contains(&qn) {
            50
        } else if qn.len() >= 2 && aliases.iter().any(|x| x.len() >= 3 && x.contains(&qn)) {
            45
        } else {
            0
        };
        if score > 0 && best.map(|(_, s)| score > s).unwrap_or(true) {
            best = Some((a, score));
        }
    }
    best.map(|(a, _)| a)
}

pub struct AppShortcutsTool;

impl Tool for AppShortcutsTool {
    fn name(&self) -> &str {
        "app_shortcuts"
    }
    fn description(&self) -> &str {
        "查常见桌面应用的默认快捷键表（不确定某个应用的快捷键时先查它，别凭印象按）。\
         支持：windows 系统 / chrome(edge) / explorer 资源管理器 / notepad / word / excel / powerpoint / \
         vscode / terminal / wechat / qq / photoshop。app 省略则返回支持的应用清单；q 可按动作关键词过滤"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "app": { "type": "string", "description": "应用名或别名，如 chrome / 资源管理器 / excel / 微信 / vscode；省略则列出支持的应用" },
                "q": { "type": "string", "description": "可选：只保留动作描述里含该关键词的项，如 截图 / 保存 / 切换" }
            }
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let app = args["app"].as_str().unwrap_or("").trim();
        let q = args["q"].as_str().unwrap_or("").trim().to_lowercase();

        if app.is_empty() || app == "list" || app == "all" {
            return Ok(json!({
                "apps": APPS.iter().map(|a| json!({
                    "app": a.key,
                    "title": a.title,
                    "aliases": a.aliases,
                    "count": a.items.len(),
                })).collect::<Vec<_>>(),
                "note": "用 app_shortcuts(app=\"chrome\") 取具体快捷键",
            }));
        }

        let entry = find_app(app).ok_or_else(|| {
            format!(
                "没有「{app}」的快捷键表。可用：{}。其他应用请在应用内「帮助 / 快捷键」或设置里查，不要凭记忆按",
                APPS.iter().map(|a| a.key).collect::<Vec<_>>().join(" / ")
            )
        })?;

        let items: Vec<Value> = entry
            .items
            .iter()
            .filter(|(_, act)| q.is_empty() || act.to_lowercase().contains(&q) || act.contains(&q))
            .map(|(keys, act)| json!({ "keys": keys, "action": act }))
            .collect();

        Ok(json!({
            "app": entry.key,
            "title": entry.title,
            "matched": items.len(),
            "shortcuts": items,
            "note": entry.note,
            "caveat": COMMON_NOTE,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::{find_app, sys_action, APPS};

    #[test]
    fn sys_actions_resolve() {
        for a in [
            "lock", "screenshot", "settings", "task_manager", "show_desktop", "volume_up",
            "explorer", "任务管理器", "magnifier",
        ] {
            assert!(sys_action(a).is_some(), "系统动作缺失: {a}");
        }
        assert!(sys_action("不存在的动作").is_none());
    }

    #[test]
    fn app_lookup_by_alias_and_fuzzy() {
        assert_eq!(find_app("chrome").map(|a| a.key), Some("chrome"));
        assert_eq!(find_app("谷歌浏览器").map(|a| a.key), Some("chrome"));
        assert_eq!(find_app("资源管理器").map(|a| a.key), Some("explorer"));
        assert_eq!(find_app("vs code").map(|a| a.key), Some("vscode"));
        assert_eq!(find_app("微信").map(|a| a.key), Some("wechat"));
        assert_eq!(find_app("chrome浏览器").map(|a| a.key), Some("chrome"));
        assert!(find_app("不存在的应用").is_none());
    }

    /// 表内不允许出现空项 / 重复 app key（手写常量表最容易出的错）
    #[test]
    fn table_is_sane() {
        let mut keys = std::collections::HashSet::new();
        for a in APPS {
            assert!(keys.insert(a.key), "重复的 app key: {}", a.key);
            assert!(!a.items.is_empty(), "{} 没有快捷键项", a.key);
            for (k, act) in a.items {
                assert!(!k.trim().is_empty(), "{} 有空的键名", a.key);
                assert!(!act.trim().is_empty(), "{} 有空的动作描述", a.key);
            }
        }
    }
}
