//! 跨 IM 消息总线：统一通道抽象 + 调度中枢。
//!
//! 设计目标：让白泽能同时接入多个 IM 通道（微信 iLink / 飞书 Lark），
//! 并把「高危操作审批回传」「任务结果回传」等跨通道行为收敛到一处统一调度，
//! 而非像早期那样在 runtime 里硬编码 `wechat.push_approval`。
//!
//! 结构：
//!   - `ImChannel`：通道统一「描述接口」（只读），供前端枚举通道状态（id/名称/是否已配置/状态快照）。
//!   - `ImBus`：消息总线，持有各通道具体实例，统一调度审批回传、结果回传、全通道启停。
//!
//! 通道的「登录 / 停止」等专属命令仍由各通道模块自行暴露（wechat_login / feishu_login …），
//! 总线只负责任意通道已连接后的统一转发，避免把登录流程也强塞进 trait 造成不必要的耦合。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tauri::{AppHandle, State};

use crate::feishu::FeishuState;
use crate::memory::MemoryStore;
use crate::wechat::WeChatState;

/// 通道统一描述接口（只读，供前端枚举 + 调度判断连通性）。
pub trait ImChannel: Send + Sync {
    /// 通道标识（wechat / feishu）
    fn id(&self) -> &'static str;
    /// 通道中文名
    fn label(&self) -> &'static str;
    /// 是否已配置凭证 / 已登录
    fn has_credentials(&self) -> bool;
    /// 连接状态快照（JSON），至少包含 "status" 与 "connected" 字段
    fn snapshot(&self) -> Value;
}

/// 消息日志最大保留条数（内存环形缓冲）
const MAX_LOG_ENTRIES: usize = 500;

/// IM 消息收发日志池：跨通道共享，记录「手机发来的指令」与「白泽回传的审批/结果」，
/// 供前端日志面板回看。纯内存环形缓冲，超出上限丢弃最早条目。
pub struct ImLog {
    entries: Mutex<VecDeque<Value>>,
    max: usize,
}

impl ImLog {
    pub fn new(max: usize) -> Self {
        Self {
            entries: Mutex::new(VecDeque::new()),
            max,
        }
    }

    /// 追加一条日志。direction：`in` 收到 / `out` 发出；channel/channel_label：通道标识与中文名。
    pub fn push(&self, direction: &str, channel: &str, channel_label: &str, peer: &str, text: &str) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let clipped: String = if text.chars().count() > 200 {
            let mut s: String = text.chars().take(200).collect();
            s.push('…');
            s
        } else {
            text.to_string()
        };
        let mut e = self.entries.lock().unwrap();
        e.push_back(json!({
            "ts": ts,
            "direction": direction,
            "channel": channel,
            "channel_label": channel_label,
            "peer": peer,
            "text": clipped,
        }));
        while e.len() > self.max {
            e.pop_front();
        }
    }

    /// 读取全部日志（时间正序）
    pub fn list(&self) -> Vec<Value> {
        self.entries.lock().unwrap().iter().cloned().collect()
    }
}

/// 消息总线：持有各通道实例，统一调度跨通道行为。
pub struct ImBus {
    pub wechat: Arc<WeChatState>,
    pub feishu: Arc<FeishuState>,
    /// 跨通道共享的消息收发日志（供前端日志面板回看）
    pub log: Arc<ImLog>,
}

impl ImBus {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        let log = Arc::new(ImLog::new(MAX_LOG_ENTRIES));
        Self {
            wechat: Arc::new(WeChatState::new(store.clone(), log.clone())),
            feishu: Arc::new(FeishuState::new(store, log.clone())),
            log,
        }
    }

    fn channel_json(c: &dyn ImChannel) -> Value {
        let snap = c.snapshot();
        json!({
            "id": c.id(),
            "label": c.label(),
            "connected": c.has_credentials(),
            "status": snap.get("status").cloned().unwrap_or(Value::Null),
        })
    }

    /// 枚举所有通道的状态，供前端通道管理面板展示。
    pub fn list(&self) -> Vec<Value> {
        vec![
            Self::channel_json(&*self.wechat),
            Self::channel_json(&*self.feishu),
        ]
    }

    /// 启动所有「已配置凭证」的通道（后台长轮询 / 长连接）。
    pub fn start_all(&self, app: &AppHandle) {
        if self.wechat.has_credentials() {
            self.wechat.start(app.clone());
        }
        if self.feishu.has_credentials() {
            self.feishu.start(app.clone());
        }
    }

    /// 停止所有通道接收。
    pub fn stop_all(&self) {
        self.wechat.stop();
        self.feishu.stop();
    }

    /// 向「最近一个活跃用户」推送高危操作审批，任一通道送达即视为成功。
    /// 返回真正推送出去的通道 id 列表（无活跃用户的通道会静默跳过）。
    pub async fn push_approval(&self, approval_id: &str, what: &str, detail: &str) -> Vec<String> {
        let mut pushed = Vec::new();
        if self.wechat.push_approval(approval_id, what, detail).await {
            pushed.push("wechat".to_string());
        }
        if self.feishu.push_approval(approval_id, what, detail).await {
            pushed.push("feishu".to_string());
        }
        pushed
    }

    /// 向活跃通道回传文本结果（依次尝试已连接通道，任一成功即返回 Ok）。
    pub async fn send_text(&self, text: &str) -> Result<(), String> {
        let mut last_err = "无已连接的 IM 通道".to_string();
        if self.wechat.has_credentials() {
            if let Err(e) = self.wechat.send_text_to_last_user(text).await {
                last_err = e;
            } else {
                return Ok(());
            }
        }
        if self.feishu.has_credentials() {
            if let Err(e) = self.feishu.send_text_to_last_user(text).await {
                last_err = e;
            } else {
                return Ok(());
            }
        }
        Err(last_err)
    }

    /// 向活跃通道回传图片（path 为空则由调用方先补全；本方法只负责转发已存在文件）。
    pub async fn send_image(&self, path: &str, caption: Option<&str>) -> Result<(), String> {
        let mut last_err = "无已连接的 IM 通道".to_string();
        if self.wechat.has_credentials() {
            if let Err(e) = self.wechat.send_image_to_last_user(path, caption).await {
                last_err = e;
            } else {
                return Ok(());
            }
        }
        if self.feishu.has_credentials() {
            if let Err(e) = self.feishu.send_image_to_last_user(path, caption).await {
                last_err = e;
            } else {
                return Ok(());
            }
        }
        Err(last_err)
    }
}

/// 枚举所有 IM 通道状态（供前端通道管理面板）
#[tauri::command]
pub fn im_list(state: State<'_, crate::AppState>) -> Vec<Value> {
    state.im_bus.list()
}

/// 读取 IM 消息收发日志（手机发来的指令 + 白泽回传的审批/结果）
#[tauri::command]
pub fn im_log(state: State<'_, crate::AppState>) -> Vec<Value> {
    state.im_bus.log.list()
}