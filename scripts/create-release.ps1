# 创建/更新 GitHub Release 并上传 NSIS 安装包
# Token 从 git 凭据存储读取（git push 时已保存），不回显
# 用法：改 $tag 后运行；同名旧资产会先删除再上传
$ErrorActionPreference = "Stop"

$tag  = "v0.1.6"
$name = "v0.1.6 · 越用越好用"
$body = "## 新增`n- 成功操作配方库：GUI 任务成功后自动提炼操作链，下次同类任务直接照用（规划好就一路跑完）`n- 同类任务结果记忆：相似指令直接参考上次执行结果，高频任务越用越快`n- wait_ui_stable 界面稳定性感知：像素级检测动画结束再定位，告别坐标漂移与「点了没反应」`n- save_dialog 另存为对话框原语 / explorer_open 一步打开文件夹 / write_file 优先写入`n- software_locate 三路秒级定位已装软件（注册表+UWP+开始菜单），拒绝全盘搜索`n## 修复`n- list_windows 漏掉最小化后台窗口 + 新增进程名字段（自绘应用「后台开着却找不到」修复）`n- paste_text 剪贴板恢复时机与覆盖问题；assert_ui 视觉通道空输出假失败`n- 点击光环视觉加强（80px/2s），触发面扩到滚轮/悬停/输入"

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
