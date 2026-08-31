# 创建/更新 GitHub Release 并上传 NSIS 安装包
# Token 从 git 凭据存储读取（git push 时已保存），不回显
# 用法：改 $tag 后运行；同名旧资产会先删除再上传
$ErrorActionPreference = "Stop"

$tag  = "v0.1.5"
$name = "v0.1.5 · 白泽终端实时可见"
$body = "## 新增`n- 命令行任务优先在「白泽终端」窗口真实执行：命令与输出实时滚动可见，会话状态延续（cd/环境变量），ps_exec 降为后台后备`n## 修复`n- 黑窗闪窗二次清剿：docker 探测、winget、ffmpeg/whisper、tesseract、powershell 邮件/日历、python/node 沙箱等残留窗口全部隐藏，子进程启动 100% 无黑窗"

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
