# 创建/更新 GitHub Release 并上传 NSIS 安装包
# Token 从 git 凭据存储读取（git push 时已保存），不回显
# 用法：改 $tag 后运行；同名旧资产会先删除再上传
$ErrorActionPreference = "Stop"

$tag  = "v0.8.6"
$name = "v0.8.6 · 受控浏览器：页内画线定位 + 模型自主决定接管 + 交互提速 + 标签页/对话框/上传下载"
$body = @(
  '## 本轮重点：受控浏览器操作大改',
  '',
  '### 元素定位改成「页内画线」',
  '- 不再用屏幕透明覆盖层（要处理工具栏偏移、最小化坐标、覆盖层未就绪，还慢）',
  '- 改为在网页里直接注入一条 `position:fixed` 的定位线：2px 红线、无特效、元素在哪线就在哪，不遮挡页面内容',
  '',
  '### 什么时候接管屏幕，由白泽自己判断',
  '- 删掉了按你说话里的关键词预判接管（「打开 / 窗口 / 按钮」太容易误触发，纯网页任务也会被抢屏）',
  '- 现在只有白泽真的去调用桌面操作工具（鼠标点击 / 键盘输入等）时才接管屏幕',
  '',
  '### 操作明显更快',
  '- 找元素先走 DOM 文字匹配（一次脚本调用、十几毫秒），失败才逐级回落到截图 + OCR / 视觉模型',
  '- 页面加载判定不再死等 `readyState=complete`（原来最长等 3 秒）',
  '- 标签页同步、可见性探测等固定开销做了节流与合并',
  '',
  '### 标签页不再走错',
  '- 每步都会告诉你「当前在操作哪个页面」（标题 + 地址），出错时也带',
  '- 新开的标签页会被记住并自动拉回前台，不会继续在原页面上瞎点',
  '- 新增「清理空白标签页」：只关白泽自己开的、空白的、非当前操作对象的，你自己的页面永远不关',
  '',
  '### 不再无意识滑动页面',
  '- 之前找到元素后会无条件把页面滚到中间；现在只有元素确实在视口外时才轻微滚动一下',
  '',
  '### 新增能力',
  '- **扫描元素**：一次把当前页面可交互元素全部框出来并编号，之后按编号点击即可（浏览器面板里有「扫描元素」按钮）',
  '- **对话框**：网页弹 alert / confirm / prompt 时，白泽会先处理再继续，不会带着阻塞点继续点',
  '- **文件上传**：可以指定本地文件路径上传',
  '- **下载目录**：可以指定下载保存位置，并查看最近下载的文件',
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
