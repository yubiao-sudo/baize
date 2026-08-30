#!/usr/bin/env python3
"""
POC S2.2：GUI 定位模型（UI-TARS / OS-Atlas / ShowUI）本地推理延迟基准

用法：python poc_grounding_bench.py --server http://127.0.0.1:8000 \
        --image shot.png --target "登录按钮" --n 10
前置：先启动 grounding sidecar（协议见 ComputerUse 接口设计 §7，JSON-RPC over HTTP）。
通过标准：平均延迟 < 2s/次（本地 GPU 期望值），并输出置信度。
"""

import argparse
import base64
import json
import sys
import time
import urllib.request


def locate(server, image_b64, target_desc, hints):
    req = {
        "op": "locate",
        "image_b64": image_b64,
        "target": {"type": "semantic", "description": target_desc},
        "bbox_hints": hints,
    }
    r = urllib.request.Request(
        server,
        data=json.dumps(req).encode("utf-8"),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(r, timeout=120) as resp:
        return json.loads(resp.read().decode("utf-8"))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--server", required=True, help="grounding sidecar 地址")
    ap.add_argument("--image", required=True, help="截图 PNG 路径")
    ap.add_argument("--target", default="登录按钮")
    ap.add_argument("--n", type=int, default=10)
    a = ap.parse_args()

    image_b64 = base64.b64encode(open(a.image, "rb").read()).decode("utf-8")
    hints = [{"x": 0, "y": 0, "w": 1920, "h": 1040}]  # 示例：全屏提示

    times = []
    for i in range(a.n):
        t0 = time.time()
        try:
            out = locate(a.server, image_b64, a.target, hints)
            dt = time.time() - t0
            times.append(dt)
            top = out["bboxes"][0] if out.get("bboxes") else None
            print("  #%d: %.0fms  top=%s" % (i + 1, dt * 1000, top))
        except Exception as e:
            print("  #%d: FAIL %s" % (i + 1, e))

    if times:
        avg = sum(times) / len(times)
        print("平均延迟: %.0fms（%d 次成功）" % (avg * 1000, len(times)))
        print("[PASS]" if avg < 2.0 else "[WARN] 延迟偏高，考虑本地小模型/云端 grounding 回退")
        sys.exit(0 if avg < 2.0 else 1)
    else:
        print("[FAIL] 无成功样本")
        sys.exit(1)


if __name__ == "__main__":
    main()
