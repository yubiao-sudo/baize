# 白泽 · 技术选型 POC 验证脚本

对应实施路线图第 6 节「风险前置验证清单」的三个最高风险项。

## 脚本清单

| 脚本 | 验证项 | 平台 | 依赖 |
|---|---|---|---|
| `poc_a11y_windows.ps1` | S2.1 无障碍树读取 | Windows | 无（.NET UIAutomation 内置） |
| `poc_a11y_tree.py` | S2.1 无障碍树读取 | macOS / Linux | pyobjc / pyatspi |
| `poc_ollama_tool_calling.py` | 本地模型工具调用 | 全平台 | 无（标准库） |
| `poc_grounding_bench.py` | S2.2 视觉定位延迟 | 全平台 | 无（需先启 sidecar） |

## 运行方式

```bash
# Windows：无障碍树（已实测可用）
pwsh -File poc_a11y_windows.ps1 -MaxDepth 5

# macOS / Linux：无障碍树
python3 poc_a11y_tree.py --max-depth 6

# 本地模型工具调用
python3 poc_ollama_tool_calling.py --model qwen2.5:7b

# 视觉定位延迟基准
python3 poc_grounding_bench.py --server http://127.0.0.1:8000 --image shot.png --target "登录按钮"
```

## 已实测结论（Windows UIA）

在 Windows 上运行 `poc_a11y_windows.ps1`，结果：

- ✅ **UIA 读取管道可用**：基于 .NET `System.Windows.Automation` 无需任何外部依赖即可读到焦点窗口的树。
- ✅ **`uiautomation` crate 读到完整树**：`examples/read_screen.rs` 实测前台 Chrome 完整 **60 节点树**（按钮/树/输入框齐全）。
- ⚠️ 早期用 .NET PowerShell 初测仅得 3 节点，属**误判**（未正确激活完整树）；结论已修正。

**对设计的启示**：WebView/Electron 应用的无障碍树在正确调用下是**完整**的，a11y 是可靠的语义接地通道。但仍需保留"视觉定位"兜底——游戏/自绘 UI 等场景没有 a11y 树。

## 通过标准速查

- `poc_a11y_*`：打印结构化树且节点数 > 0 → `[PASS]`。
- `poc_ollama_tool_calling.py`：模型返回 `tool_calls`，arguments 可解析且含 `path` → `[PASS]`。
- `poc_grounding_bench.py`：平均延迟 < 2s → `[PASS]`，否则 `[WARN]` 建议降级云端 grounding。
