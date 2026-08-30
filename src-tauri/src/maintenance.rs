//! 白泽自维护机制：周期性检测并清理运行期膨胀的资源。
//!
//! 清理项：
//!   1. 审计日志（audit_log 只写不读，裁剪到最近 5000 条 + WAL 压缩）
//!   2. GUI 任务截屏残留（baize-screenshot-*.png 写在工作目录且此前永不删除，
//!      单张 1~2MB，重度使用会积累上千张）
//!
//! 挂载：随每小时记忆治理线程运行，每 6 小时执行一次（lib.rs）。
//! 结果打印到控制台日志，并记录 maintenance_last 时间戳。

use crate::memory::MemoryStore;
use std::time::SystemTime;

/// 截图保留时长：3 天（关键帧回放只看最近任务，3 天远超需要）
const SCREENSHOT_MAX_AGE_MS: i64 = 3 * 24 * 3600 * 1000;
/// 审计日志保留条数
const AUDIT_KEEP: usize = 5000;

/// 一次维护的检测结果
pub struct MaintenanceReport {
    pub audit_pruned: usize,
    pub screenshots_pruned: usize,
    pub screenshot_freed_bytes: u64,
}

impl MaintenanceReport {
    pub fn summary(&self) -> String {
        format!(
            "审计裁剪 {} 条；清理任务截屏 {} 张（{:.1} MB）",
            self.audit_pruned,
            self.screenshots_pruned,
            self.screenshot_freed_bytes as f64 / 1024.0 / 1024.0
        )
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 执行一次完整维护；任何单项失败都不阻塞其余项
pub fn run_maintenance(store: &MemoryStore) -> MaintenanceReport {
    // 1) 审计日志裁剪 + WAL 压缩
    let audit_pruned = store.prune_audit(AUDIT_KEEP).unwrap_or(0);

    // 2) 工作目录里的任务截屏残留（baize-screenshot-{ts}.png）
    let (mut screenshots_pruned, mut screenshot_freed_bytes) = (0usize, 0u64);
    if let Ok(cwd) = std::env::current_dir() {
        let now = now_ms();
        if let Ok(entries) = std::fs::read_dir(&cwd) {
            for e in entries.flatten() {
                let p = e.path();
                let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                let Some(ts) = name
                    .strip_prefix("baize-screenshot-")
                    .and_then(|s| s.strip_suffix(".png"))
                    .and_then(|s| s.parse::<i64>().ok())
                else {
                    continue;
                };
                if now - ts > SCREENSHOT_MAX_AGE_MS {
                    let sz = e.metadata().map(|m| m.len()).unwrap_or(0);
                    if std::fs::remove_file(&p).is_ok() {
                        screenshots_pruned += 1;
                        screenshot_freed_bytes += sz;
                    }
                }
            }
        }
    }

    MaintenanceReport {
        audit_pruned,
        screenshots_pruned,
        screenshot_freed_bytes,
    }
}

/// 是否到期（距上次维护超过 interval_ms）
pub fn due(store: &MemoryStore, interval_ms: i64) -> bool {
    let now = now_ms();
    store
        .get_setting("maintenance_last")
        .ok()
        .flatten()
        .and_then(|v| v.parse::<i64>().ok())
        .map(|t| now - t > interval_ms)
        .unwrap_or(true)
}

/// 执行并记录时间戳；返回摘要（未到期返回 None）
pub fn tick(store: &MemoryStore, interval_ms: i64) -> Option<String> {
    if !due(store, interval_ms) {
        return None;
    }
    let report = run_maintenance(store);
    let _ = store.set_setting("maintenance_last", &now_ms().to_string());
    Some(report.summary())
}
