# 创建/更新 GitHub Release 并上传 NSIS 安装包
# Token 从 git 凭据存储读取（git push 时已保存），不回显
# 用法：改 $tag 后运行；同名旧资产会先删除再上传
$ErrorActionPreference = "Stop"

$tag  = "v0.9.0"
$name = "v0.9.0 · 远程知识库接入 + 配置视图 + 流式草稿 + 卡顿看门狗"
$body = @(
  '## 这一版加的是什么',
  '',
  '### 1. 远程知识库：把白泽接上外部知识库底座',
  '`rag_search` 的后端可以切到 knowledge-fabric 底座，**工具名和参数不变**——提示词、技能、工作流都不用改。',
  '',
  '- 设置页新增「企业知识库（远程底座）」：填**服务地址 + 8 位接入码**即可连接，带状态横幅（已连接 · 租户/库 · 文档数/分片数）。',
  '- 检索质量提升明显：底座有重排、有可核实的引用区间（`citation` / `char_range`），并且**能在没有依据时明确拒答**。',
  '- **拒答是结论、不是故障**：底座判「知识库里没依据」时原样透传，绝不偷偷回落本地索引——',
  '  否则会出现「底座说没依据、本地翻出过期文档、模型据此编答案」。只有网络故障才回落，且结果标注来源。',
  '- `rag_index` 在远程模式下改为**上传到底座**（支持单个文件或整个目录，目录会挑出 PDF/Markdown 逐个传）。',
  '- 令牌经 Windows DPAPI 加密后落库，不存明文；本地索引模式仍然保留（默认行为不变）。',
  '',
  '### 2. 配置视图：让模型「知道自己被配成了什么样」',
  '改了视觉模型、加了数据库，模型的认知却还停在旧印象——于是按过时的假设做决策。',
  '现在把影响决策的配置项汇总成摘要注入上下文，并提供只读的 `config_get` 查询（凭据类字段自动打码）。',
  '',
  '### 3. 流式回复草稿：进程被掐断不再整轮丢失',
  '此前助手消息是「整轮跑完才落库」，生成途中被关掉就整轮没了（10-17 那次丢了约 3900 字）。',
  '现在边生成边落库，最多丢最后几秒。',
  '',
  '### 4. 卡顿看门狗：被 Windows 判「未响应」关闭后，事后有现场',
  '0.8.8 曾出现被判定 Application Hang（事件 1002）后关闭，而 panic 钩子、SEH 过滤器、WER 都没有留下任何记录。',
  '现在卡顿期间会定期采样主线程栈与当时在跑的工具，便于定位。',
  '',
  '### 5. 修复：`cargo test` 启动即崩溃（0xC0000139）',
  '根因不在代码也不在环境：`comctl32` 的 `TaskDialogIndirect` 只存在于 **v6**（WinSxS 重定向），',
  '而 Tauri 只给主 bin 嵌了带 Common-Controls v6 的清单；lib 的单元测试二进制没有清单，加载期绑定到 5.82 就报',
  '`STATUS_ENTRYPOINT_NOT_FOUND`。改为**延迟加载** comctl32 解决。',
  '',
  '### 6. 修复：构建卡在「Compiling baize」8 分钟以上',
  '历史上 build script 会 spawn 一个每 0.5 秒查杀、持续 10 分钟的常驻守护进程（为了防 dev watcher 重复拉起实例）。',
  '实测它让构建卡住 8 分钟以上，且反复构建会累积残留守护、越编越慢。',
  '改为「仅在调试版实例确实存在时同步查杀一次」——同一构建 37 秒即进入 rustc。',
  '',
  '### 7. GUI 接地：按名称找控件，并可限定窗口',
  '`find_element` / `click_element` 支持按名称模糊查找（不依赖坐标），可用 `window` 限定搜索范围。',
  '同时修掉「Win10 记事本焦点在菜单栏时只拿得到『文件(F)』一个节点」导致不带窗口名时形同废掉的问题。',
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
