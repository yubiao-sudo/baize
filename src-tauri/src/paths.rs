//! 数据目录绑定：所有运行时数据（数据库 / 浏览器登录态 / 截图缓存）统一落在
//! 「安装目录\data」下，安装/首次启动自动生成，不再散落在 C 盘工作目录或 AppData。
//!
//! 目录解析策略：
//!   1. 首选 `<exe 所在目录>\data` —— NSIS 默认按当前用户安装（%LOCALAPPDATA%\Programs\Baize），
//!      该目录对普通用户可写；用户自定义安装到 D 盘等位置时同样可写。
//!   2. 兜底：目录不可写（如按机器级别装进 Program Files 且未提权）时回退
//!      `%LOCALAPPDATA%\baize\data`，并置 fallback 标记（设置页/引导层如实提示）。
//!
//! 旧数据迁移（一次性，幂等）：
//!   - baize.db：历史版本用相对路径打开，可能散落在「启动时工作目录 / exe 目录 /
//!     LocalAppData\baize」任一处 —— 找到即复制进 data（库 + wal + shm）。
//!   - browser-profile：历史在 %LOCALAPPDATA%\baize\browser-profile —— 整目录复制，
//!     保登录态/Cookie 不丢。
//!   迁移采用「复制不删除」：旧文件留在原地由用户自行清理，避免误删风险。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

static DATA_ROOT: OnceLock<PathBuf> = OnceLock::new();
static FALLBACK: AtomicBool = AtomicBool::new(false);

/// 已解析的数据根目录（init 之前调用返回 exe 旁临时推算值，正常流程 init 最早执行）
pub fn data_root() -> &'static std::path::Path {
    DATA_ROOT.get().map(|p| p.as_path()).unwrap_or_else(|| {
        // 未 init（理论不会发生）：返回 exe 旁 data，保证调用方拿到确定路径
        Box::leak(Box::new(default_root())) as &std::path::Path
    })
}

/// 是否处于兜底目录（安装目录不可写）
pub fn is_fallback() -> bool {
    FALLBACK.load(Ordering::Relaxed)
}

pub fn db_path() -> PathBuf {
    data_root().join("baize.db")
}

pub fn screens_dir() -> PathBuf {
    data_root().join("screens")
}

pub fn browser_profile_dir() -> PathBuf {
    data_root().join("browser-profile")
}

fn default_root() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("data")
}

fn localappdata_baize() -> Option<PathBuf> {
    std::env::var("LocalAppData")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(|lad| PathBuf::from(lad).join("baize"))
}

/// 启动最早时机调用：解析 + 创建 + 迁移。幂等，可安全重复调用。
pub fn init() {
    let candidate = default_root();
    let root = match std::fs::create_dir_all(&candidate) {
        Ok(()) => {
            // 试写验证：目录存在 ≠ 可写（Program Files 场景）
            let probe = candidate.join(".write-test");
            match std::fs::write(&probe, b"ok").and_then(|_| std::fs::remove_file(&probe)) {
                Ok(()) => candidate,
                Err(_) => fallback_root(),
            }
        }
        Err(_) => fallback_root(),
    };
    let _ = DATA_ROOT.set(root);
    migrate_legacy();
}

fn fallback_root() -> PathBuf {
    FALLBACK.store(true, Ordering::Relaxed);
    let lad = localappdata_baize()
        .unwrap_or_else(|| std::env::temp_dir().join("baize"));
    let root = lad.join("data");
    let _ = std::fs::create_dir_all(&root);
    root
}

/// 升级前把整个数据目录备份到 %LocalAppData%\baize\update-backup。
/// NSIS 升级会先运行旧版卸载器清空 $INSTDIR（含 data），此备份是升级链路的数据保险。
/// 返回是否备份成功（尚无数据 / 备份失败均返回 false，不阻断升级）。
pub fn backup_to_appdata() -> bool {
    let root = data_root().to_path_buf();
    if !root.join("baize.db").exists() {
        return false; // 还没有任何用户数据，无需备份
    }
    let Some(lad) = localappdata_baize() else {
        return false;
    };
    let backup = lad.join("update-backup");
    let _ = std::fs::remove_dir_all(&backup);
    let _ = std::fs::create_dir_all(&backup);
    copy_dir_recursive(&root, &backup)
}

/// 一次性迁移：旧位置的数据复制进 data 目录（目标已存在则跳过，幂等）
fn migrate_legacy() {
    let root = data_root().to_path_buf();

    // ── 1) baize.db：从历史散落位置找回 ──
    let new_db = root.join("baize.db");
    if !new_db.exists() {
        let mut legacy: Vec<PathBuf> = vec![];
        if let Ok(cwd) = std::env::current_dir() {
            legacy.push(cwd.join("baize.db"));
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                legacy.push(dir.join("baize.db"));
            }
        }
        if let Some(lad) = localappdata_baize() {
            legacy.push(lad.join("baize.db"));
            legacy.push(lad.join("data").join("baize.db"));
        }
        for src in legacy {
            if src.exists() && copy_db_trio(&src, &new_db) {
                println!(
                    "[数据迁移] 已从 {} 迁移数据库至 {}",
                    src.display(),
                    new_db.display()
                );
                break;
            }
        }
    }

    // ── 1.5) 升级备份回填：应用内升级时已把 data 备份到 update-backup，
    //    若当前 data 缺主库（NSIS 清空 $INSTDIR 后未由安装钩子回填），整体回填 ──
    if !new_db.exists() {
        if let Some(lad) = localappdata_baize() {
            let backup = lad.join("update-backup");
            if backup.join("baize.db").exists() && copy_dir_recursive(&backup, &root) {
                println!(
                    "[数据恢复] 已从升级备份 {} 回填数据目录 {}",
                    backup.display(),
                    root.display()
                );
            }
        }
    }

    // ── 2) 浏览器登录态：LocalAppData\baize\browser-profile → data\browser-profile ──
    let new_profile = root.join("browser-profile");
    if !new_profile.exists() {
        if let Some(lad) = localappdata_baize() {
            let old = lad.join("browser-profile");
            if old.is_dir() && copy_dir_recursive(&old, &new_profile) {
                println!(
                    "[数据迁移] 浏览器登录态已从 {} 迁移至 {}",
                    old.display(),
                    new_profile.display()
                );
            }
        }
    }
}

/// 复制数据库三件套（db + wal + shm；wal/shm 缺失忽略）
fn copy_db_trio(src: &std::path::Path, dst: &std::path::Path) -> bool {
    if std::fs::copy(src, dst).is_err() {
        return false;
    }
    for ext in ["-wal", "-shm"] {
        let s = src.with_extension(format!("db{ext}"));
        if s.exists() {
            let _ = std::fs::copy(&s, dst.with_extension(format!("db{ext}")));
        }
    }
    true
}

/// 递归复制目录（浏览器 profile 可能上千个小文件，同步复制一次性成本可接受）
fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> bool {
    if std::fs::create_dir_all(dst).is_err() {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(src) else {
        return false;
    };
    for entry in entries.flatten() {
        let ty = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        let target = dst.join(entry.file_name());
        if ty.is_dir() {
            if !copy_dir_recursive(&entry.path(), &target) {
                return false;
            }
        } else if std::fs::copy(entry.path(), &target).is_err() {
            // 单个文件失败不中断（Cookie 库文件被占用时跳过，登录态部分保留也强于全丢）
            continue;
        }
    }
    true
}
