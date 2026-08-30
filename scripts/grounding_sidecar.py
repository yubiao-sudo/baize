#!/usr/bin/env python3
"""
视觉定位 sidecar（三级接地 · 二级）
协议见《白泽桌面Agent框架-ComputerUse接口设计.md》§7：
  POST /locate  {"image_b64": "...", "target": {"description": "登录按钮"}, "bbox_hints": [...]}
      → {"bboxes": [{"bbox": {"x","y","w","h"}, "label": "...", "confidence": 0.92}]}

默认 MOCK 模式：返回占位 bbox，用于联调协议（无需模型/GPU）。

接入真实模型（OS-Atlas，推荐）：
  1. pip install transformers torch accelerate pillow
  2. 首次运行会自动从 HuggingFace 下载模型（约 7B，需 ~15GB 显存或 CPU offload）
  3. 把下方 USE_REAL_MODEL 改为 True
  也可换成 UI-TARS / ShowUI，只需改 REAL 分支的推理代码。

运行：python grounding_sidecar.py --port 8765
"""

import argparse
import base64
import io
import json
import re
from http.server import BaseHTTPRequestHandler, HTTPServer

USE_REAL_MODEL = False  # 改为 True 启用 OS-Atlas 真实推理


def locate_real(image_b64: str, target_desc: str):
    """真实推理（OS-Atlas）。参考实现，需在你的环境验证细节。"""
    from PIL import Image
    from transformers import AutoModelForCausalLM, AutoProcessor

    model_id = "OS-Copilot/OS-Atlas-Base-7B"  # 或 OS-Atlas-Base-4B
    processor = AutoProcessor.from_pretrained(model_id, trust_remote_code=True)
    model = AutoModelForCausalLM.from_pretrained(
        model_id, trust_remote_code=True, device_map="auto"
    )

    image = Image.open(io.BytesIO(base64.b64decode(image_b64))).convert("RGB")
    messages = [
        {
            "role": "user",
            "content": [
                {"type": "image", "image": image},
                {"type": "text", "text": f"Locate the {target_desc} on this screen."},
            ],
        }
    ]
    text = processor.apply_chat_template(messages, tokenize=False, add_generation_prompt=True)
    inputs = processor(text=[text], images=[image], return_tensors="pt").to(model.device)
    outputs = model.generate(**inputs, max_new_tokens=128)
    response = processor.batch_decode(outputs, skip_special_tokens=False)[0]

    # 解析坐标（OS-Atlas 输出形如 <|box_start|>(x1,y1),(x2,y2)<|box_end|>，具体格式需按版本核对）
    boxes = []
    for m in re.finditer(r"\((\d+),(\d+)\),\((\d+),(\d+)\)", response):
        x1, y1, x2, y2 = map(int, m.groups())
        boxes.append({"bbox": {"x": x1, "y": y1, "w": x2 - x1, "h": y2 - y1},
                      "label": target_desc, "confidence": 0.9})
    return {"bboxes": boxes}


def locate_mock(target_desc: str):
    return {
        "bboxes": [
            {"bbox": {"x": 400, "y": 300, "w": 120, "h": 40},
             "label": target_desc, "confidence": 0.5}
        ]
    }


def locate(image_b64: str, target_desc: str, hints):
    if USE_REAL_MODEL:
        return locate_real(image_b64, target_desc)
    return locate_mock(target_desc)


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        if self.path == "/locate":
            length = int(self.headers.get("Content-Length", 0))
            body = json.loads(self.rfile.read(length) or b"{}")
            target = body.get("target", {})
            target_desc = target.get("description", "") if isinstance(target, dict) else str(target)
            result = locate(body.get("image_b64"), target_desc, body.get("bbox_hints"))
            data = json.dumps(result).encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
        else:
            self.send_response(404)
            self.end_headers()

    def log_message(self, *args):
        pass


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=8765)
    a = ap.parse_args()
    print(f"[grounding sidecar] 监听 127.0.0.1:{a.port}（real_model={USE_REAL_MODEL}）")
    HTTPServer(("127.0.0.1", a.port), Handler).serve_forever()


if __name__ == "__main__":
    main()
