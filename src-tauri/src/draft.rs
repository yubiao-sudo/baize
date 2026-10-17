//! 流式回复草稿：边生成边落库，进程被掐断时最多丢最后几秒的内容。
//!
//! 背景（2026-10-17）：助手消息是「整轮跑完才 `add_message`」，而 17:15:33 那次
//! 「未响应被关掉」发生在生成途中 —— 17:12:24 开始的那一轮（已生成第 3 章约 3900 字）
//! 在库里一个字都没有，整轮凭空消失，用户侧表现就是「任务全失败了」。
//!
//! 这里把生成中的文本按节流频率写进 settings（key = `chat_draft:<conv_id>`），
//! 下次启动由 [`recover`] 捞回来补成一条 assistant 消息，并明确标注「被中断」。
//! 正常结束走 [`finish`] 清掉草稿，不留残影。

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::memory::MemoryStore;

const KEY_PREFIX: &str = "chat_draft:";
/// 累积多少字符落一次（中文按 UTF-8 字节算，400 字节约等于 130 汉字，够细了）
const FLUSH_BYTES: usize = 400;
/// 或者最多隔多久落一次（长文慢速生成时的兜底）
const FLUSH_EVERY: Duration = Duration::from_secs(2);

const INTERRUPT_NOTE: &str = "\n\n---\n> ⚠ 这条回复在生成途中白泽被强制关闭（卡死/被杀），以上是中断前已生成的部分，可能不完整。";

struct Draft {
    store: Arc<MemoryStore>,
    conv: String,
    buf: String,
    flushed: usize,
    last_flush: Instant,
}

fn slot() -> &'static Mutex<Option<Draft>> {
    static SLOT: OnceLock<Mutex<Option<Draft>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

fn key_of(conv: &str) -> String {
    format!("{KEY_PREFIX}{conv}")
}

/// 开始新一轮：清掉该会话可能残留的旧草稿（上一轮异常残留属于已放弃的内容）
pub fn begin(store: Arc<MemoryStore>, conv_id: &str) {
    let _ = store.delete_setting(&key_of(conv_id));
    if let Ok(mut g) = slot().lock() {
        *g = Some(Draft {
            store,
            conv: conv_id.to_string(),
            buf: String::new(),
            flushed: 0,
            last_flush: Instant::now(),
        });
    }
}

/// 追加流式片段（热路径）。正常路径下每 2 秒或每 400 字节才落一次盘。
pub fn append(token: &str) {
    let Ok(mut g) = slot().lock() else { return };
    let Some(d) = g.as_mut() else { return };
    d.buf.push_str(token);
    if d.buf.len() < d.flushed + FLUSH_BYTES && d.last_flush.elapsed() < FLUSH_EVERY {
        return;
    }
    d.last_flush = Instant::now();
    d.flushed = d.buf.len();
    let value = serde_json::json!({
        "conv_id": d.conv,
        "text": d.buf,
        "ts": now_ms(),
    })
    .to_string();
    let _ = d.store.set_setting(&key_of(&d.conv), &value);
}

/// 结束本轮：删除草稿。**调用方必须已经把完整回复（或明确的失败态）落库**，
/// 否则这里一删，内容就真没了。
pub fn finish() {
    if let Ok(mut g) = slot().lock() {
        if let Some(d) = g.take() {
            let _ = d.store.delete_setting(&key_of(&d.conv));
        }
    }
}

/// 启动时恢复被中断的草稿（在窗口创建前调用，保证前端读到的历史里已经包含它）
pub fn recover(store: &MemoryStore) {
    let keys = store.settings_with_prefix(KEY_PREFIX);
    for (key, value) in keys {
        let _ = store.delete_setting(&key);
        let parsed: serde_json::Value = match serde_json::from_str(&value) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[草稿恢复] {key} 解析失败，已丢弃: {e}");
                continue;
            }
        };
        let conv = parsed.get("conv_id").and_then(|v| v.as_str()).unwrap_or("");
        let text = parsed.get("text").and_then(|v| v.as_str()).unwrap_or("");
        if conv.is_empty() || text.trim().is_empty() {
            continue;
        }
        // 安全闸：只有当该会话最后一条仍是「用户消息」时才补写。
        // 若最后一条已是 assistant，说明完整回复当时已经落库，这份草稿是残影。
        let last_is_user = store
            .messages(conv, 1)
            .ok()
            .and_then(|m| m.last().map(|r| r.role == "user"))
            .unwrap_or(false);
        if !last_is_user {
            continue;
        }
        let body = format!("{text}{INTERRUPT_NOTE}");
        match store.add_message(conv, "assistant", &body, None) {
            Ok(_) => {
                let chars = text.chars().count();
                crate::windows::diag_log(&format!(
                    "[草稿恢复] 会话 {conv} 补回被中断的回复（{chars} 字）"
                ));
            }
            Err(e) => eprintln!("[草稿恢复] 写回失败: {e}"),
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个测试用独立临时库，避免互相串味
    fn temp_store(tag: &str) -> MemoryStore {
        let dir = std::env::temp_dir().join(format!(
            "baize-draft-test-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("t.db");
        let _ = std::fs::remove_file(&path);
        MemoryStore::open(path.to_string_lossy().as_ref()).expect("打开临时库失败")
    }

    fn put_draft(store: &MemoryStore, conv: &str, text: &str) {
        let value = serde_json::json!({ "conv_id": conv, "text": text, "ts": 1 }).to_string();
        store
            .set_setting(&format!("{KEY_PREFIX}{conv}"), &value)
            .unwrap();
    }

    #[test]
    fn recover_restores_interrupted_turn() {
        let store = temp_store("recover");
        store.ensure_conversation("c1", "写小说", None).unwrap();
        store.add_message("c1", "user", "继续写第三章", None).unwrap();
        put_draft(&store, "c1", "第三章 下午四点，建设路");

        recover(&store);

        let msgs = store.messages("c1", 10).unwrap();
        assert_eq!(msgs.len(), 2, "应当补回一条 assistant 消息");
        assert_eq!(msgs[1].role, "assistant");
        assert!(msgs[1].content.starts_with("第三章 下午四点，建设路"));
        assert!(msgs[1].content.contains("被强制关闭"));
        assert!(
            store.get_setting(&format!("{KEY_PREFIX}c1")).unwrap().is_none(),
            "恢复后草稿键必须删除，否则每次启动都会重复补写"
        );
    }

    #[test]
    fn recover_skips_draft_when_reply_already_saved() {
        let store = temp_store("stale");
        store.ensure_conversation("c2", "写小说", None).unwrap();
        store.add_message("c2", "user", "继续写", None).unwrap();
        store.add_message("c2", "assistant", "完整回复", None).unwrap();
        put_draft(&store, "c2", "残影");

        recover(&store);

        let msgs = store.messages("c2", 10).unwrap();
        assert_eq!(msgs.len(), 2, "最后一条已是 assistant，不得重复补写");
        assert_eq!(msgs[1].content, "完整回复");
        assert!(store
            .get_setting(&format!("{KEY_PREFIX}c2"))
            .unwrap()
            .is_none());
    }

    /// 前缀扫描不能被 LIKE 语义坑到（键里的 `_` 是通配符）
    #[test]
    fn settings_prefix_matches_exactly() {
        let store = temp_store("prefix");
        store.set_setting("chatXdraft:err", "不该命中").unwrap();
        store.set_setting("chat_draft:a", "1").unwrap();
        store.set_setting("other", "2").unwrap();

        let hit = store.settings_with_prefix(KEY_PREFIX);
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].0, "chat_draft:a");

        store.delete_setting("chat_draft:a").unwrap();
        assert!(store.settings_with_prefix(KEY_PREFIX).is_empty());
        assert_eq!(
            store.get_setting("chatXdraft:err").unwrap().as_deref(),
            Some("不该命中"),
            "删除必须精确命中 key，不能连带误删"
        );
    }
}
