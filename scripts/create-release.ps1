# 创建/更新 GitHub Release 并上传 NSIS 安装包
# Token 从 git 凭据存储读取（git push 时已保存），不回显
# 用法：改 $tag 后运行；同名旧资产会先删除再上传
$ErrorActionPreference = "Stop"

$tag  = "v0.8.7"
$name = "v0.8.7 · 按「模块 / 区域」描述目标时，默认先把那一块的可交互元素编号框出来"
$body = @(
  '## 修了什么',
  '',
  '### 操作浏览器时看不到「一整块功能模块的元素编号框」',
  '- 先说结论：**编号框的注入链路本身是好的**。我把它原样跑在真实网页上验证过——百度首页一次扫出 32 个可交互元素，红框和编号位置全部对得上，单条定位线也精确框住目标。',
  '- 真正的原因在**策略层**：编号框（scan）此前被写成「文字定位不准时的备选」，而按文字直接点有一条快速通道，所以白泽正常干活时基本走不到那一步，你自然就看不到群体框。',
  '- 现在改成：**只要你按「位置 / 区域 / 功能模块」描述目标**（「右上角那个按钮」「侧边栏里的 XX」「这个面板上的图标」「那一排的第三个」），白泽就默认先扫描这一块、把里面所有可交互元素编号框出来给你看，再按编号点；只有目标文字在页面上唯一（「点登录」「点搜索」）才直接点、省掉这一步。',
  '- 另外修了一个坑：指定模块时如果那个模块不在当前视口里，会先把模块带进视口再扫描——否则框全落在视口外，你看到的就是「说扫这个模块却什么都没框」。',
  '',
  '### 想手动看的时候',
  '- 浏览器面板里有「扫描元素」按钮：留空扫整页，也可以填一个模块名（如「侧边栏」「工具栏」「评论区」）只扫那一块。',
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
