#!/usr/bin/env python3
"""
POC M1：本地 Ollama 工具调用（function calling）能力验证
纯标准库（urllib），无需 pip。

用法：python poc_ollama_tool_calling.py --model qwen2.5:7b --base http://127.0.0.1:11434
通过标准：模型返回 tool_calls，arguments 为合法 JSON 且含所需参数（path）。
"""

import argparse
import json
import sys
import urllib.request


def chat(base, model, messages, tools):
    body = json.dumps({
        "model": model,
        "messages": messages,
        "tools": tools,
        "stream": False,
    }).encode("utf-8")
    req = urllib.request.Request(
        f"{base}/v1/chat/completions",
        data=body,
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.loads(r.read().decode("utf-8"))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", default="qwen2.5:7b")
    ap.add_argument("--base", default="http://127.0.0.1:11434")
    a = ap.parse_args()

    tools = [{
        "type": "function",
        "function": {
            "name": "list_files",
            "description": "列出指定目录下的文件与子目录",
            "parameters": {
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
            },
        },
    }]
    messages = [{"role": "user", "content": "请调用 list_files 工具，列出 C:\\Windows 目录下的文件"}]

    print("模型: %s @ %s" % (a.model, a.base))
    try:
        resp = chat(a.base, a.model, messages, tools)
    except Exception as e:
        print("[FAIL] 请求失败（请确认已 ollama serve 并拉取模型）: %s" % e)
        sys.exit(1)

    msg = resp["choices"][0]["message"]
    tc = msg.get("tool_calls")
    if not tc:
        print("[FAIL] 模型未返回 tool_calls，文本回复: %s" % msg.get("content"))
        sys.exit(1)

    fn = tc[0]["function"]
    print("[OK] 工具名: %s" % fn["name"])
    try:
        args = json.loads(fn["arguments"])
    except json.JSONDecodeError:
        print("[FAIL] arguments 不是合法 JSON: %s" % fn["arguments"])
        sys.exit(1)
    print("[OK] 参数: %s" % json.dumps(args, ensure_ascii=False))

    if "path" in args:
        print("[PASS] 本地模型工具调用能力正常，参数完整")
        sys.exit(0)
    else:
        print("[FAIL] 缺少参数 path")
        sys.exit(1)


if __name__ == "__main__":
    main()
