# 创建 GitHub Release 并上传 NSIS 安装包
# Token 从 git 凭据存储读取（git push 时已保存），不回显
$ErrorActionPreference = "Stop"

$credFile = Join-Path $env:USERPROFILE ".git-credentials"
$token = $null
Get-Content $credFile | ForEach-Object {
    if ($_ -match "://([^:]+):([^@]+)@github\.com") { $token = $Matches[2] }
}
if (-not $token) { Write-Output "NO_TOKEN_FOUND"; exit 1 }

$repo = "yubiao-sudo/baize"
$headers = @{
    Authorization = "Bearer $token"
    Accept        = "application/vnd.github+json"
    "X-GitHub-Api-Version" = "2022-11-28"
    UserAgent     = "baize-release"
}

# 1) 建标签与 Release
$body = @{
    tag_name         = "v0.1.0"
    target_commitish = "main"
    name             = "v0.1.0 · 白泽首个公开版"
    body             = "白泽 BaiZe 首个公开版本。`n`n## 亮点`n- 万能聊天卡片（天气/日程/比分精美可视化）`n- GUI 自动化：screen_elements 全屏元素标注 + window_prepare 一键清屏 + 拟人化点击注入`n- 微信/飞书机器人：图片真实回传、消息去重、IM 审批`n- 受控浏览器与用户 Chrome 冲突治理、单实例保护`n- 水球风格全新桌面图标`n`n下载下方 x64-setup.exe 安装即可。"
    draft            = $false
    prerelease       = $false
} | ConvertTo-Json

try {
    $bodyBytes = [Text.Encoding]::UTF8.GetBytes($body)
    $rel = Invoke-RestMethod -Method Post -Uri "https://api.github.com/repos/$repo/releases" -Headers $headers -ContentType "application/json" -Body $bodyBytes
    Write-Output "RELEASE_CREATED: $($rel.id) $($rel.html_url)"
} catch {
    $msg = $_.ErrorDetails.Message
    if ($msg -match "already_exists") {
        Write-Output "RELEASE_EXISTS"
        $rel = Invoke-RestMethod -Method Get -Uri "https://api.github.com/repos/$repo/releases/tags/v0.1.0" -Headers $headers
    } else {
        Write-Output "CREATE_FAIL: $msg"
        exit 1
    }
}

# 2) 上传安装包资产（脚本位于 scripts/ 下，仓库根为其上一级）
$nsisDir = Join-Path (Split-Path $PSScriptRoot -Parent) "src-tauri\target\release\bundle\nsis"
$exe = Get-ChildItem $nsisDir -Filter *.exe | Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $exe) { Write-Output "NO_INSTALLER_FOUND in $nsisDir"; exit 1 }

$uploadHeaders = @{
    Authorization  = "Bearer $token"
    "Content-Type" = "application/octet-stream"
    UserAgent      = "baize-release"
}
$upUri = "https://uploads.github.com/repos/$repo/releases/$($rel.id)/assets?name=$([uri]::EscapeDataString($exe.Name))"
$asset = Invoke-RestMethod -Method Post -Uri $upUri -Headers $uploadHeaders -InFile $exe.FullName
Write-Output "ASSET_UPLOADED: $($asset.browser_download_url)"
