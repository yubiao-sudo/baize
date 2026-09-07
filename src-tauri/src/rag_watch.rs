//! Workspace 文件监听（#8）：监听最近一次 RAG 索引的目录，
//! 变更防抖 30s 后自动重索引知识库，并发通知 + 后台任务进度。
//!
//! 监听目录来自 settings key `rag_watch_dir`（index_rag_dir 成功后写入）。

use crate::jobs;
use crate::rag::RagIndex;
use std::path::Path;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;
use tauri::Emitter;

use notify::{RecursiveMode, Watcher};

/// 防抖窗口：文件批量保存/编译等会产生连续事件，静默 30s 才重索引
const DEBOUNCE: Duration = Duration::from_secs(30);

pub fn spawn(app: tauri::AppHandle, rag: Arc<RagIndex>, store: Arc<crate::memory::MemoryStore>) {
    std::thread::spawn(move || {
        let dir = match store.get_setting("rag_watch_dir") {
            Ok(Some(d)) if !d.trim().is_empty() => d.trim().to_string(),
            _ => return, // 从未索引过任何目录：不启动监听
        };
        if !Path::new(&dir).exists() {
            return;
        }

        let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
        let mut watcher = match notify::recommended_watcher(tx) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("[RAG监听] 创建 watcher 失败: {e}");
                return;
            }
        };
        if let Err(e) = watcher.watch(Path::new(&dir), RecursiveMode::Recursive) {
            eprintln!("[RAG监听] 监听 {dir} 失败: {e}");
            return;
        }
        println!("[RAG监听] 监听目录: {dir}");

        loop {
            // 等待第一个变更事件
            match rx.recv() {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    eprintln!("[RAG监听] 事件错误: {e}");
                    continue;
                }
                Err(_) => return, // channel 关闭
            }
            // 防抖：持续吸收事件直到静默 30s
            loop {
                match rx.recv_timeout(DEBOUNCE) {
                    Ok(_) => continue,
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }

            // 自动重索引（全量替换存储，不会重复）
            let job_id = jobs::start("rag", &format!("自动重索引 · {dir}"), 0);
            let result = rag.index_dir_with_progress(&dir, 200, None);
            match &result {
                Ok(count) => jobs::finish(&job_id, true, &format!("已更新 {count} 个分块")),
                Err(e) => jobs::finish(&job_id, false, e),
            }
            let _ = app.emit(
                "proactive",
                serde_json::json!({
                    "id": uuid::Uuid::new_v4().to_string(),
                    "title": "知识库已自动更新",
                    "body": match &result {
                        Ok(count) => format!("检测到「{dir}」文件变更，已重新索引（{count} 个分块）"),
                        Err(e) => format!("检测到「{dir}」文件变更，但重索引失败：{e}"),
                    },
                    "files": [],
                }),
            );
        }
    });
}
