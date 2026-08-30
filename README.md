# 白泽 BaiZe · 本地优先的桌面 AI Agent

> 一个「会想、会动手、懂隐私、可编排、能记住你」的 Windows 桌面 AI 助手。
> Tauri 2 + Rust + React 18 构建，本地模型优先、云端兜底，全量审计、安全可控。

![白泽](src-tauri/icons/128x128.png)

## ✨ 功能总览

### 对话与模型
- **模型链路**：本地 Ollama ⇄ 云端（DeepSeek / 豆包 / GLM / OpenAI 兼容协议）失败自动切换，设置页可视化配置
- **万能聊天卡片**：天气 / 日程 / 比分 / 系统状态等结构化信息以精美 HTML 卡片嵌入聊天框，模型自由排版、自调尺寸
- **多模型对比**：一次提问并行对比多个模型，分支展示
- **语音对话**：语音输入（唤醒词「白泽」）+ 本地 Kokoro TTS 朗读独白，回声门防自唤醒

### GUI 自动化（Computer Use）
- 看屏三件套：无障碍树 / 截屏 OCR（Windows.Media.Ocr 双引擎，全屏 ~0.5s）/ 视觉模型
- `screen_elements` 一键提取全屏可交互元素与文字行坐标，批量规划操作步骤
- `window_prepare` 一键清屏：聚焦目标窗口（前台切换验证）+ 置顶验证 + 最小化其余窗口
- 拟人化鼠标注入：两段式逼近移动 + hover 停留 + 紧连双击；左/中/右键、拖拽、滚轮、悬停
- 三级回退：预期状态校验 → 破坏性操作闸门（删除/发送类强制审批）→ 操作回放与逆操作
- 屏幕接管：阻断物理键鼠输入 + 桌面弹幕直播执行进度（Ctrl+Shift+F12 紧急解除）

### 生态与通道
- **微信 / 飞书机器人**：文字指令远程指挥白泽，图片经 CDN 加密上传真实回传，高危操作 IM 审批
- **MCP 客户端**：接入 Model Context Protocol 工具生态
- **软件管家**：搜索 / 安装 / 卸载（智能避让系统盘），浏览器路径探测与指定
- **120+ 内置工具**：文件 / 命令（Docker 沙箱隔离）/ HTTP / 邮件 / 数据库 / 定时任务 / OCR / 表格 / 剪贴板……

### 记忆与自我进化
- 智能记忆：召回 → 合并 → 语义固化 → 衰减 → 知识图谱（Canvas 星云可视化）
- **经验知识库**：任务失败自动复盘，提炼「问题 → 解法」，下次同类任务自动召回
- 主动唤醒：监听目录 / 定时任务 / 看门狗
- 自维护：审计裁剪、截屏清理、WAL 检查点每小时自动运行

### 桌面集成
- 内置浏览器（多标签 + CDP 受控桌面 Chrome 精细操控）与 Markdown 文档窗口（目录跳转 / 打印导出 PDF）
- 内置终端（PTY）、执行流可视化、GUI 关键帧回放
- 系统托盘 + 全局快捷键 Alt+Space + NSIS 安装包

## 📥 安装（Windows）

前往 [**Releases**](https://github.com/yubiao-sudo/baize/releases) 页面下载最新版 `BaiZe_x.y.z_x64-setup.exe`：

1. 双击安装包完成安装（NSIS 安装向导）
2. 启动白泽，在「设置」中配置模型（本地 Ollama 或云端 API Key）
3. Alt+Space 随时呼出 / 隐藏

> 首次使用建议先在设置页完成模型配置；微信 / 飞书机器人在设置页扫码或填入凭证即可启用。

## 🛠 从源码构建

环境要求：Node.js ≥ 18、pnpm、Rust（stable, MSVC）、WebView2。

```bash
git clone https://github.com/yubiao-sudo/baize.git
cd baize
pnpm install

# 开发调试（热重载）
pnpm tauri dev

# 打包 NSIS 安装包
pnpm tauri build
# 产物：src-tauri/target/release/bundle/nsis/*.exe
```

> 注意：日常 `cargo build` 出的是 dev 模式 exe（加载 devUrl，需配合 `pnpm tauri dev`）。
> 双击独立运行 / 分发必须用 `pnpm tauri build` 或 `cargo build --features custom-protocol`。

## ⚙️ 配置

环境变量均可选（也可在「设置」面板可视化配置，持久化到本地 SQLite）：

```powershell
$env:BAIZE_CLOUD_API_KEY = "sk-xxx"        # 云端兜底（DeepSeek 默认）
$env:BAIZE_MODEL_PRIORITY = "cloud"        # 云端优先
$env:BAIZE_WATCH_DIR = "C:\Users\xx\Downloads"   # 主动唤醒监听目录
$env:BAIZE_CHROME_PATH = "C:\...\chrome.exe"     # 受控浏览器路径（默认自动探测）
$env:BAIZE_MCP_COMMAND = "npx"             # MCP 服务器命令
$env:BAIZE_MCP_ARGS = "-y,@modelcontextprotocol/server-filesystem,D:\"  # 逗号分隔参数
```

模型 API Key 加密存储在本地 vault，不上传、不入库明文。

## 🔒 隐私与安全

- 本地优先：对话、记忆、审计全部存本地 SQLite；云端仅在配置后作为兜底
- 危险操作分级审批：只读自动放行 / 写操作确认 / 高危操作（删除、发送等）强制逐次审批
- Shell 命令 Docker 沙箱隔离（断网 + 限资源，不可用时宿主机降级 + 警告）
- 全量审计日志，可回放每一次工具调用

## 📁 目录结构

```
baize/
├── src/                    # React 前端（意识台 UI）
│   ├── components/         # ChatView / BrowserWindow / MarkdownWindow / HaloOverlay / ...
│   ├── hooks/              # 语音对话 / 语音输入
│   ├── stores/             # Zustand 会话状态
│   └── api.ts              # Tauri IPC 封装
├── scripts/                # 构建辅助脚本
├── make_icon.py            # 桌面图标生成（Pillow 水球风格）
└── src-tauri/
    ├── src/
    │   ├── lib.rs          # AppState + 工具注册 + Tauri 装配
    │   ├── agent/          # supervisor(规划) + runtime(AgentLoop + 工具执行)
    │   ├── model.rs        # 模型路由 / 多厂商 / 失败切换
    │   ├── browser.rs      # CDP 受控桌面浏览器
    │   ├── capability/     # Computer Use（observe/act/ground/screen_elements）
    │   ├── wechat.rs       # 微信机器人（ilink 协议：收发/图片 CDN/审批）
    │   ├── tts.rs          # Kokoro 本地 / 云端 / 豆包 TTS
    │   ├── security.rs     # 权限分级 + HITL 审批 + 审计
    │   └── ...             # 记忆 / 调度 / OCR / 沙箱 / 通知升级
    ├── icons/              # 水球风格应用图标
    └── tauri.conf.json
```

## 📄 相关文档

- 《白泽桌面Agent框架-设计文档.md》—— 架构与创新设计
- 《白泽桌面Agent框架-实施路线图.md》—— 工程蓝图
- 《白泽桌面Agent框架-ComputerUse接口设计.md》—— 接口规格
- 《白泽桌面Agent框架-实现状态报告.md》—— 实现进度 + 跨平台指南

## License

MIT
