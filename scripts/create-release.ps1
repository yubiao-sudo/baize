# 创建/更新 GitHub Release 并上传 NSIS 安装包
# Token 从 git 凭据存储读取（git push 时已保存），不回显
# 用法：改 $tag 后运行；同名旧资产会先删除再上传
$ErrorActionPreference = "Stop"

$tag  = "v0.8.8"
$name = "v0.8.8 · 键盘补全 + 系统级动作 + 应用快捷键 + 启动应用支持别名/路径/网址"
$body = @(
  '## 这一版补的是什么',
  '',
  '### 1. 键盘：浏览器和桌面两条通道都补齐了',
  '- **浏览器以前只能按单键**（组合键会直接报「按键找不到」）。现在支持 `ctrl+a`、`ctrl+enter`、`shift+Tab`、`ctrl++`，多步可以一次敲完：`ctrl+a Backspace`。',
  '- **输入框覆盖写入**：`clear` 参数先全选再写。以前第二次往同一个框里填值只会越填越长。',
  '- **长文本快一个数量级**：改成整串写入（中文、emoji 原生支持），不再逐字符派发。',
  '- **桌面符号键和大写字母终于按对了**：以前 `key_press("A")` 会敲出小写 `a`、`!` 会敲成 `1`。现在按你当前键盘布局补 Shift / Ctrl / Alt 位。',
  '- **桌面也支持多步**：`ctrl+a delete`、`ctrl+s Enter` 一次调用按顺序敲完；`key_down` / `key_up` 补了扫描码，「按住 Shift 多选」不再失效。',
  '- **写错的组合键会直接报错**，比如 `ctrl+a+b`（两个主键），不再静默把键名当文字打进输入框。',
  '',
  '### 2. 系统级动作：26 个命名动作，不用再猜快捷键',
  '以前这些只能让白泽自己拼 `win+xxx`，拼错就白按一次。现在是明确的名字：',
  '- 会改屏幕状态：`lock` 锁屏、`screenshot` 截图、`settings` 设置页（可直达显示 / 声音 / 蓝牙 / 网络 / 更新 / 隐私）',
  '- 常用面板：运行框、资源管理器、任务管理器、任务视图、显示桌面、切换窗口、关机菜单（Win+X）、回收站',
  '- 输入查找：搜索、剪贴板历史、表情面板、通知中心、放大镜、开始菜单',
  '- 媒体音量：音量加 / 减 / 静音、播放暂停、上一首 / 下一首',
  '- 刻意没做**关机 / 重启 / 注销**——不可逆的操作留给你自己。锁屏用的是系统接口而不是模拟 Win+L（后者在部分环境会被吞）。',
  '',
  '### 3. 应用快捷键查询：13 个应用的默认快捷键表',
  'Chrome / Edge、资源管理器、记事本、Word、Excel、PPT、VS Code、终端、微信、QQ、Photoshop、Windows 系统、通用。不确定时先查表再按，而不是凭印象乱按。',
  '',
  '### 4. 打开应用：一个工具开四类东西',
  '- 内置别名：`cmd`、`powershell`、`wt`、`notepad`、`calc`、`control`、`settings`、`taskmgr`、`regedit`、`mstsc`、`services`、`devmgmt`、`diskmgmt`、`eventvwr`、`osk`、`chrome`、`msedge`、`firefox` 等（不必在开始菜单里有）',
  '- 绝对路径：exe、文档、文件夹 → 交给默认程序打开',
  '- 网址：含 `www.` 会自动补 `https://`',
  '- 协议：`ms-settings:display`、`shell:AppsFolder\…`、`mailto:…`',
  '- 还支持带启动参数；网址和协议类不再白等十几秒找窗口',
  '',
  '安装包口径不变：Windows x64，NSIS 安装程序。升级会保留原有数据（模型 / 语音 / 记忆设置）。'
) -join "`n"

$credFile = Join-Path $env:USERPROFILE ".git-credentials"
$token = $null
Get-Content $credFile | ForEach-Object {
    if ($_ -match "://([^:]+):([^@]+)@github\.com") { $token = $Matches[2] }
}
if (-not $token) { Write-Output "NO_TOKEN_FOUND"; exit 1 }

$repo = "yubiao-sudo/baize"
$headers = @{
    Authorization          = "Bearer $token"
    Accept                 = "application/vnd.github+json"
    "X-GitHub-Api-Version" = "2022-11-28"
    UserAgent              = "baize-release"
}

# 1) 建标签与 Release（已存在则取回）
$bodyBytes = [Text.Encoding]::UTF8.GetBytes((@{
            tag_name         = $tag
            target_commitish = "main"
            name             = $name
            body             = $body
            draft            = $false
            prerelease       = $false
        } | ConvertTo-Json))
try {
    $rel = Invoke-RestMethod -Method Post -Uri "https://api.github.com/repos/$repo/releases" -Headers $headers -ContentType "application/json" -Body $bodyBytes
    Write-Output "RELEASE_CREATED: $($rel.id) $($rel.html_url)"
} catch {
    $msg = $_.ErrorDetails.Message
    if ($msg -match "already_exists") {
        Write-Output "RELEASE_EXISTS"
        $rel = Invoke-RestMethod -Method Get -Uri "https://api.github.com/repos/$repo/releases/tags/$tag" -Headers $headers
    } else {
        Write-Output "CREATE_FAIL: $msg"
        exit 1
    }
}

# 2) 定位安装包
$nsisDir = Join-Path (Split-Path $PSScriptRoot -Parent) "src-tauri\target\release\bundle\nsis"
$exe = Get-ChildItem $nsisDir -Filter *.exe | Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $exe) { Write-Output "NO_INSTALLER_FOUND in $nsisDir"; exit 1 }

# 3) 清理全部 Release 中的同名旧资产（含误传到其他 tag 的）
$allRels = Invoke-RestMethod -Method Get -Uri "https://api.github.com/repos/$repo/releases?per_page=100" -Headers $headers
foreach ($r in $allRels) {
    foreach ($a in $r.assets) {
        if ($a.name -eq $exe.Name) {
            Invoke-RestMethod -Method Delete -Uri "https://api.github.com/repos/$repo/releases/assets/$($a.id)" -Headers $headers | Out-Null
            Write-Output "OLD_ASSET_DELETED: $($r.tag_name) / $($a.name)"
        }
    }
}

# 4) 上传安装包
$uploadHeaders = @{
    Authorization  = "Bearer $token"
    "Content-Type" = "application/octet-stream"
    UserAgent      = "baize-release"
}
$upUri = "https://uploads.github.com/repos/$repo/releases/$($rel.id)/assets?name=$([uri]::EscapeDataString($exe.Name))"
$asset = Invoke-RestMethod -Method Post -Uri $upUri -Headers $uploadHeaders -InFile $exe.FullName
Write-Output "ASSET_UPLOADED: $($asset.browser_download_url)"
