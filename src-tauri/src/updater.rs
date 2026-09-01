//! 应用内自更新：检查 GitHub Releases 最新版 → 下载安装包（进度事件推送）→ 静默安装并自动重启。
//!
//! 流程：
//! 1. `update_check`：请求 GitHub Releases latest，比对版本号与当前 `CARGO_PKG_VERSION`
//! 2. `update_install`：下载 x64-setup.exe 到临时目录（每 256KB emit 一次 update-progress），
//!    下载完成后拉起「更新链」bat（等白泽退出 → NSIS 静默安装 /S → 自动启动新版），
//!    白泽展示 1.2s「安装中」状态后退出，之后无需人工干预

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

const RELEASES_API: &str = "https://api.github.com/repos/yubiao-sudo/baize/releases/latest";

/// 下载候选镜像前缀（按序回退）：GitHub 直连时常被墙，gh 加速代理前缀 + 完整原始 URL。
/// 空串 = 直连优先；某个候选连接失败/非 2xx 时自动尝试下一个。
/// 2026-08-31 实测：gh-proxy.com 可用，ghfast.top 超时，ghproxy.net 证书异常——故 gh-proxy 优先
const DOWNLOAD_MIRRORS: &[&str] = &[
    "",
    "https://gh-proxy.com/",
    "https://ghfast.top/",
    "https://ghproxy.net/",
];

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent("baize-updater")
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("构建 HTTP 客户端失败: {e}"))
}

/// 当前版本是否比 latest 旧（语义化版本逐段比较，忽略非数字后缀）
fn version_lt(current: &str, latest: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> {
        s.trim_start_matches('v')
            .split('.')
            .map(|p| p.chars().take_while(|c| c.is_ascii_digit()).collect::<String>())
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect()
    };
    let a = parse(current);
    let b = parse(latest);
    for i in 0..3 {
        let av = a.get(i).copied().unwrap_or(0);
        let bv = b.get(i).copied().unwrap_or(0);
        if av != bv {
            return av < bv;
        }
    }
    false
}

/// 检查最新版本：返回 { current, latest, has_update, notes, download_url, size, url }
#[tauri::command]
pub async fn update_check() -> Result<Value, String> {
    let client = http_client()?;
    let v: Value = client
        .get(RELEASES_API)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("网络请求失败: {e}"))?
        .json()
        .await
        .map_err(|e| format!("解析发布信息失败: {e}"))?;

    let latest = v["tag_name"]
        .as_str()
        .unwrap_or("")
        .trim_start_matches('v')
        .to_string();
    let current = env!("CARGO_PKG_VERSION").to_string();

    let mut download_url = String::new();
    let mut size = 0u64;
    if let Some(arr) = v["assets"].as_array() {
        for a in arr {
            let name = a["name"].as_str().unwrap_or("");
            if name.to_lowercase().ends_with("x64-setup.exe") {
                download_url = a["browser_download_url"].as_str().unwrap_or("").to_string();
                size = a["size"].as_u64().unwrap_or(0);
            }
        }
    }

    let has_update = !latest.is_empty() && version_lt(&current, &latest) && !download_url.is_empty();
    Ok(json!({
        "current": current,
        "latest": latest,
        "has_update": has_update,
        "notes": v["body"].as_str().unwrap_or(""),
        "url": v["html_url"].as_str().unwrap_or(""),
        "download_url": download_url,
        "size": size,
    }))
}

/// 下载最新安装包（推送 update-progress 进度事件）并以静默模式拉起安装器，然后退出白泽
#[tauri::command]
pub async fn update_install(app: AppHandle) -> Result<Value, String> {
    let client = http_client()?;
    // 复用 check 逻辑拿下载地址
    let info = update_check().await?;
    if !info["has_update"].as_bool().unwrap_or(false) {
        return Err("没有可用的新版本".into());
    }
    let url = info["download_url"].as_str().unwrap_or("").to_string();
    if url.is_empty() {
        return Err("发布页未找到 x64-setup.exe 安装包".into());
    }

    let _ = app.emit(
        "update-progress",
        json!({ "phase": "download", "pct": 0, "downloaded": 0, "total": info["size"] }),
    );

    // 直连 + 镜像按序回退：GitHub 直连被墙时自动切加速代理
    let mut last_err = String::new();
    let mut resp = None;
    for prefix in DOWNLOAD_MIRRORS {
        let candidate = format!("{}{}", prefix, url);
        match client
            .get(&candidate)
            .timeout(std::time::Duration::from_secs(600))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => {
                resp = Some(r);
                break;
            }
            Ok(r) => last_err = format!("HTTP {}", r.status()),
            Err(e) => last_err = e.to_string(),
        }
    }
    let mut resp = resp.ok_or_else(|| {
        format!("下载请求失败（直连与镜像均不可用）: {last_err}；可手动从发布页下载安装")
    })?;
    if !resp.status().is_success() {
        return Err(format!("下载失败：HTTP {}", resp.status()));
    }
    let total = resp.content_length().unwrap_or(0);

    let dir = std::env::temp_dir().join("baize-update");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建更新目录失败: {e}"))?;
    let dest = dir.join("BaiZe-update-setup.exe");
    let mut file = std::fs::File::create(&dest).map_err(|e| format!("创建安装包文件失败: {e}"))?;

    let mut downloaded: u64 = 0;
    let mut last_emit = 0u64;
    use std::io::Write;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("下载中断: {e}"))?
    {
        file.write_all(&chunk).map_err(|e| format!("写入失败: {e}"))?;
        downloaded += chunk.len() as u64;
        // 每下载 256KB 或每 2% 推一次进度
        let pct = if total > 0 { downloaded * 100 / total } else { 0 };
        if downloaded - last_emit >= 256 * 1024 || pct != last_emit % 100 && total > 0 {
            let _ = app.emit(
                "update-progress",
                json!({ "phase": "download", "pct": pct, "downloaded": downloaded, "total": total }),
            );
            last_emit = downloaded;
        }
    }
    file.flush().ok();
    drop(file);

    if downloaded < 1024 * 1024 {
        return Err("下载的安装包异常（过小），已放弃安装".into());
    }
    let _ = app.emit(
        "update-progress",
        json!({ "phase": "install", "pct": 100, "downloaded": downloaded, "total": total }),
    );

    // 更新链（脱离白泽进程存活）：等白泽退出 → NSIS 静默安装 → 自动重启新版。
    // 装机目录不变（覆盖安装），直接复用当前 exe 路径拉起新版。
    // 用 bat 承载链条规避 cmd /C 复杂引号转义；cmd 以裸文件名运行（current_dir 已设到该目录）。
    let cur_exe = std::env::current_exe().map_err(|e| format!("定位当前程序失败: {e}"))?;
    let bat = dir.join("baize-update-chain.bat");
    // cmd 按 ANSI(GBK) 解析 bat，路径含中文时 UTF-8 直写会乱码——用 GBK 编码写入
    let bat_content = format!(
        "@echo off\r\ntimeout /t 2 /nobreak >nul\r\n\"{}\" /S\r\ntimeout /t 1 /nobreak >nul\r\nstart \"\" \"{}\"\r\n",
        dest.to_string_lossy(),
        cur_exe.to_string_lossy()
    );
    let (gbk, _, _) = encoding_rs::GBK.encode(&bat_content);
    std::fs::write(&bat, &*gbk).map_err(|e| format!("写入更新脚本失败: {e}"))?;
    let mut chain = crate::tools::silent_command("cmd");
    chain.args(["/C", "baize-update-chain.bat"]).current_dir(&dir);
    chain
        .spawn()
        .map_err(|e| format!("拉起更新链失败: {e}"))?;
    // 给前端 1.2s 展示「安装中」状态，随后退出交给更新链
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    app.exit(0);
    Ok(json!({ "ok": true, "path": dest.to_string_lossy() }))
}
