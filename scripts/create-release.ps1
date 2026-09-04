# 创建/更新 GitHub Release 并上传 NSIS 安装包
# Token 从 git 凭据存储读取（git push 时已保存），不回显
# 用法：改 $tag 后运行；同名旧资产会先删除再上传
$ErrorActionPreference = "Stop"

$tag  = "v0.5.0"
$name = "v0.5.0 · 首次启动环境自检 + 运行时自动索引"
$body = "## 新增`n- 首次安装启动环境自检：全屏引导层逐项检测运行环境（PowerShell / 网络连通 / Windows OCR 中文语言包 / 磁盘空间 / 管理员权限 / Python / Kokoro 本地语音 / Tesseract / 音频设备 / Node.js / Git），每项独立超时，单卡片实时出结论`n- 必需项缺失软拦截：给出影响说明与修复命令（一键复制），可「仍然进入」；增强项缺失不影响核心功能，Esc 可跳过引导`n- 运行时自动索引：检测到的 Python / Kokoro / Tesseract / Node / Git 路径写入本地配置，相关功能直接使用无需重复探测；本地 Kokoro 服务目录不再硬编码，任意安装位置均可识别`n- 非首次启动零打扰：只读缓存报告秒判断；若必需环境仍未就绪（上次跳过/带病进入），主界面顶部非阻塞提示卡可一键复检，通过后自动消失`n- 设置新增「环境检测」页签：查看上次报告、手动重新检测`n`n## 优化`n- 探测子进程统一静默启动（无黑窗闪烁），逐项 4-10s 独立超时不互相拖垮；并发触发自动防重"

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
