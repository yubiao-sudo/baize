// ==========================================================================
// 细胞器 · Tauri Backend Bridge
// --------------------------------------------------------------------------
// 职责：
//   1. 订阅 Bus 上 tier=command 的上行命令信号
//   2. 调用 Tauri invoke（浏览器预览模式走内置 mock）
//   3. 把后端异步事件（chat token / tool / approval）回灌成 Bus 信号
// 与旧前端 kernel/api.ts 的本质区别：
//   不直接导出 chat() 等函数，Cell 永远通过发送 cmd.* 信号触发调用
// ==========================================================================
import { prismBus } from "../../bus/prism.bus";
import type { SignalEnvelope } from "../../bus/prism.types";

// ---- Tauri 检测 + 优雅降级 mock ----
const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!inTauri) return mockInvoke<T>(cmd, args ?? {});
  // 延迟加载：让浏览器模式无需实际安装 tauri 依赖即可跑通
  const mod = await import("@tauri-apps/api/core");
  return (mod.invoke as typeof import("@tauri-apps/api/core").invoke)<T>(cmd, args as never);
}
async function listen<T>(event: string, handler: (e: { payload: T }) => void): Promise<() => void> {
  if (!inTauri) return () => {};
  const mod = await import("@tauri-apps/api/event");
  return (mod.listen as typeof import("@tauri-apps/api/event").listen)<T>(event, (e) =>
    handler({ payload: e.payload })
  );
}

// ---- 浏览器预览模式内置 mock（模拟流式 chat + 工具调用 + 审批） ----
const MOCK_REPLIES: Record<string, string> = {
  default:
    "这是棱镜舱（Nexus Prism）返回的模拟答复。\n\n我会像真实模型一样，逐 token 输出、中间穿插工具调用，并会在需要写入文件时向你请求审批。\n\n你可以在右侧「记忆棱镜」中看到刚刚这条问题已经被自动记录为一个会话碎片。",
  code: "```ts\nexport function hello(name: string): string {\n  return `Hello, ${name}, from Prism Nexus.`;\n}\n```\n\n上面是一段 TypeScript 示例。如果你在真实 Tauri 环境下运行，这段代码会由后端实际模型生成。",
  plan: "好的，我来分步拆解这个任务：\n\n1. **理解上下文** — 收集相关文件与类型\n2. **设计信号流** — 定义输入/输出信号和各 Cell 的订阅关系\n3. **落地实现** — 先 Bus，再主题层，最后 Cell 组件\n4. **验证闭环** — tsc / vite build / dev server 三通关",
};
function* tokenize(text: string): Generator<string> {
  // 中文按字、英文按词、标点直接输出 —— 简单规则让"流式感"自然
  const re = /[\u4e00-\u9fa5]|[A-Za-z0-9_-]+|\s+|[^\s]/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text))) yield m[0];
}

async function mockInvoke<T>(cmd: string, args: Record<string, unknown>): Promise<T> {
  if (cmd === "chat") {
    const prompt = String((args.message as string) ?? "");
    const key = /代码|code/i.test(prompt) ? "code" : /计划|步骤|todo|怎么/i.test(prompt) ? "plan" : "default";
    const text = MOCK_REPLIES[key];
    // 模拟异步流式
    const convId = String(args.convId ?? "mock");
    let tick = 0;
    for (const tok of tokenize(text)) {
      const delay = 18 + Math.random() * 28;
      await new Promise((r) => setTimeout(r, delay));
      prismBus.emit("chat.token", { convId, token: tok }, { tier: "atomic", source: "mock.bridge" });
      tick++;
      // 每 ~30 token 插入一个工具帧样例
      if (tick === 30 && key === "plan") {
        mockEmitToolFrame(convId, "search", { query: "棱镜信号流契约" }, "running");
        await new Promise((r) => setTimeout(r, 220));
        mockEmitToolFrame(convId, "search", { query: "棱镜信号流契约" }, "ok", "Found 3 docs: bus/types, prism/engine, scaffold layout.");
      }
      if (tick === 60 && key === "code") {
        // 假装要 write 文件 —— 触发审批
        const approvalId = "app-mock-" + Date.now();
        prismBus.emit(
          "approval.arrive",
          {
            id: approvalId,
            tool: "write_file",
            cls: "Write",
            args: { path: "C:/demo/hello.ts", content: "..." },
            description: "计划写入 hello.ts 示例代码文件",
            arrivedAt: Date.now(),
          },
          { tier: "pulse", source: "mock.bridge" }
        );
      }
    }
    prismBus.emit(
      "chat.turn.done",
      { convId, text, role: "assistant" },
      { tier: "pulse", source: "mock.bridge" }
    );
    // 记录一个记忆碎片
    prismBus.emit(
      "memory.shard.added",
      {
        id: "m-" + Date.now(),
        content: prompt.slice(0, 40) + (prompt.length > 40 ? "…" : ""),
        tag: "session",
        weight: 0.6 + Math.random() * 0.3,
        createdAt: Date.now(),
      },
      { tier: "pulse", source: "mock.bridge" }
    );
    return text as unknown as T;
  }
  if (cmd === "resolve_permission") {
    const id = String((args.id as string) ?? "");
    const allow = !!args.allow;
    prismBus.emit(
      "approval.resolved",
      { id, allow, reason: String((args.reason as string) ?? "") },
      { tier: "pulse", source: "mock.bridge" }
    );
    return true as unknown as T;
  }
  if (cmd === "set_active_model") {
    const modelId = String((args.modelId as string) ?? "");
    prismBus.emit("model.changed", { modelId }, { tier: "pulse", source: "mock.bridge" });
    return true as unknown as T;
  }
  if (cmd === "pick_files") return ([] as string[]) as unknown as T;
  if (cmd === "pick_folder") return "" as unknown as T;
  return null as unknown as T;
}

function mockEmitToolFrame(
  convId: string,
  tool: string,
  args: unknown,
  status: "pending" | "running" | "ok" | "fail" | "skip",
  resultSnip?: string
) {
  const frameId = "tf-" + convId + "-" + Math.random().toString(36).slice(2, 8);
  prismBus.emit(
    "tool.frame",
    {
      frameId,
      tool,
      args,
      status,
      resultSnip,
      convId,
      startedAt: status === "pending" || status === "running" ? Date.now() : undefined,
      finishedAt: status === "ok" || status === "fail" || status === "skip" ? Date.now() : undefined,
    } as object,
    { tier: status === "running" ? "wave" : "pulse", source: "mock.bridge" }
  );
  return frameId;
}

// ---- 上行命令消费：所有 cmd.* 信号统一在这里转给 Tauri ----
function handleCommand(env: SignalEnvelope) {
  if (env.tier !== "command") return;
  const kind = env.kind;
  const payload = env.payload as Record<string, unknown>;
  switch (kind) {
    case "cmd.chat.send":
      void invoke("chat", {
        convId: payload.convId ?? "main",
        message: payload.message,
        history: payload.history ?? [],
        attachments: payload.attachments ?? [],
      });
      break;
    case "cmd.approval.resolve":
      void invoke("resolve_permission", {
        id: payload.id,
        allow: !!payload.allow,
        reason: String(payload.reason ?? ""),
      });
      break;
    case "cmd.model.set":
      void invoke("set_active_model", { modelId: payload.modelId });
      break;
    case "cmd.files.pick":
      void invoke("pick_files", { multi: true }).then((paths) => {
        prismBus.emit("files.picked", { paths }, { tier: "pulse", source: "bridge.tauri", parent: env.sid });
      });
      break;
    case "cmd.folder.pick":
      void invoke("pick_folder").then((path) => {
        prismBus.emit("folder.picked", { path }, { tier: "pulse", source: "bridge.tauri", parent: env.sid });
      });
      break;
    default:
      // 未知命令：直接发射 reply（便于 inspector 观察）
      prismBus.emit("cmd.unknown", { kind, payload }, { tier: "pulse", source: "bridge.tauri" });
  }
}

// ---- Tauri 事件回灌（Tauri 模式下订阅真实事件）----
async function wireBackendEvents() {
  if (!inTauri) return;
  const mk = <T>(ev: string, kind: string, tier: "atomic" | "pulse" | "wave" = "pulse") =>
    listen<T>(ev, (e) => prismBus.emit(kind, e.payload as object, { tier, source: "bridge.tauri" }));
  await mk<{ convId: string; token: string }>("chat-token", "chat.token", "atomic");
  await mk<{ convId: string; role: string; text: string }>("chat-turn-done", "chat.turn.done", "pulse");
  await mk<object>("tool-frame", "tool.frame", "wave");
  await mk<object>("approval-arrive", "approval.arrive", "pulse");
  await mk<object>("approval-resolved", "approval.resolved", "pulse");
  await mk<object>("memory-shard-added", "memory.shard.added", "pulse");
}

/** 启动桥：在 main.tsx 调用一次即可 */
export function mountBridge(): () => void {
  const unsub = prismBus.subscribe(handleCommand, undefined, [
    "cmd.chat.send",
    "cmd.approval.resolve",
    "cmd.model.set",
    "cmd.files.pick",
    "cmd.folder.pick",
  ]);
  void wireBackendEvents();
  return unsub;
}