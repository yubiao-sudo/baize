# 创建/更新 GitHub Release 并上传 NSIS 安装包
# Token 从 git 凭据存储读取（git push 时已保存），不回显
# 用法：改 $tag 后运行；同名旧资产会先删除再上传
$ErrorActionPreference = "Stop"

$tag  = "v0.6.0"
$name = "v0.6.0 · 可靠性与功能大版本"
$body = "## 新增`n- 模型可靠性：429/5xx 指数退避重试、提供方熔断器、流式中断探测与自动恢复、5 分钟健康探活（设置页状态点）`n- 代理显式化：提供方代理模式可选 env / 强制直连 / 自定义，彻底规避系统代理截胡国内 API`n- 用量统计：按天/按模型统计 token 消耗与调用次数，内置主流模型定价表换算费用（纯 CSS 柱状图）`n- 快捷指令：`/` 唤起指令浮层，常用提示词一键展开，支持增删管理`n- 会话操作：编辑重发最后一条消息、从任意回复分叉新会话`n- 审计与权限：操作审计日志查询回放、权限规则管理`n- RAG：索引改异步任务队列（右下角进度浮层）、目录文件监听自动重索引`n- 屏幕感知：工具栏「看屏幕」一键截屏分析`n`n## 优化`n- 上下文预算裁剪：超限时自动用快模型生成摘要压缩历史，长会话不再爆上下文`n- 工具结果截断（单条 4000 字 / 历史 48000 字符）`n- 思考模型 reasoning_content 兜底，不再出现假装完成的空回复`n`n## 修复`n- 执行流独白与最新轨迹被输入框遮挡：ResizeObserver 动态补挂新子元素、独白出现/消失瞬间强制钉底、deferred 渲染对齐"

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
