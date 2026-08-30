fn main() {
    // Windows：重编译链接前先结束仍在运行的调试版实例。
    // 运行中的 baize.exe 被系统锁定，链接器无法替换它，会报
    // 「failed to remove file ... baize.exe · 拒绝访问 (os error 5)」。
    #[cfg(windows)]
    kill_stale_debug_instance();
    tauri_build::build()
}

/// 只结束 target\debug 目录下的实例（tauri dev / cargo build 产物），
/// 不碰用户安装的正式版。
#[cfg(windows)]
fn kill_stale_debug_instance() {
    let exe = std::env::current_dir()
        .map(|d| d.join("target").join("debug").join("baize.exe"))
        .unwrap_or_default();
    let mtime_before = std::fs::metadata(&exe)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // 1) 立即查杀一次
    let _ = kill_once();

    // 2) 派生后台守护：编译可能持续数分钟，期间用户/看护逻辑可能重新拉起实例。
    //    守护进程每 0.5s 查杀一次，直到 exe 被新版本替换（链接完成，mtime 变化，
    //    此时 tauri dev 要拉起的新实例不能再杀）或超时（10 分钟）自动退出。
    //    detached 启动，不阻塞构建本身。
    let script = format!(
        "$exe='{exe}'; $born={mtime_before}; $end=(Get-Date).AddMinutes(10); \
         while((Get-Date) -lt $end) {{ \
           if(Test-Path $exe) {{ \
             $now=[int][double]::Parse((Get-Item $exe).LastWriteTimeUtc.Subtract([datetime]'1970-01-01').TotalSeconds); \
             if($now -ne $born) {{ exit }} \
           }}; \
           Get-Process baize -ErrorAction SilentlyContinue | \
             Where-Object {{ $_.Path -like '*\\target\\debug\\*' }} | Stop-Process -Force; \
           Start-Sleep -Milliseconds 500 \
         }}",
        exe = exe.to_string_lossy().replace('\'', "''"),
    );
    let _ = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(windows)]
fn kill_once() -> std::io::Result<()> {
    std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Get-Process baize -ErrorAction SilentlyContinue | \
             Where-Object { $_.Path -like '*\\target\\debug\\*' } | Stop-Process -Force",
        ])
        .output()
        .map(|_| ())
}
