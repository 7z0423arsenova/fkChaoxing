#!/usr/bin/env python3
"""ddddocr 常驻识别进程。

Rust 核心通过 stdin/stdout 逐行交换 JSON：
- 启动成功：{"ready": true}
- 识别请求：{"image": "<png/jpeg 的 base64>"}
- 识别结果：{"code": "abcd"}
- 任意错误：{"error": "..."}

模型只在启动时加载一次，之后复用进程，避免每次验证码都重新加载 ONNX。
"""

import base64
import json
import sys


def respond(payload):
    sys.stdout.write(json.dumps(payload, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def recognize(ocr, image):
    """先按学习通验证码做二值化/反色/膨胀，失败时回退为原图识别。"""
    try:
        import cv2
        import numpy as np

        gray = cv2.imdecode(np.frombuffer(image, np.uint8), cv2.IMREAD_GRAYSCALE)
        if gray is not None:
            _, binary = cv2.threshold(gray, 190, 255, cv2.THRESH_BINARY)
            inverted = cv2.bitwise_not(binary)
            dilated = cv2.dilate(inverted, np.ones([3, 2], np.uint8), iterations=1)
            ok, encoded = cv2.imencode(".png", dilated)
            if ok:
                code = ocr.classification(encoded.tobytes())
                if code:
                    return code
    except Exception:  # noqa: BLE001 - 预处理失败时回退到原图
        pass
    return ocr.classification(image)


def main():
    try:
        import ddddocr
    except Exception as exc:  # noqa: BLE001 - 需要把安装错误原样带回 Rust
        respond(
            {
                "error": (
                    f"导入 ddddocr 失败：{exc}。"
                    "请先安装：python -m pip install ddddocr"
                )
            }
        )
        return 1

    try:
        ocr = ddddocr.DdddOcr(show_ad=False)
    except Exception as exc:  # noqa: BLE001
        respond({"error": f"初始化 ddddocr 失败：{exc}"})
        return 1

    respond({"ready": True})

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
        except Exception as exc:  # noqa: BLE001
            respond({"error": f"请求不是有效 JSON：{exc}"})
            continue
        if request.get("op") == "exit":
            break
        try:
            image = base64.b64decode(request.get("image", ""), validate=True)
            code = recognize(ocr, image)
            respond({"code": code})
        except Exception as exc:  # noqa: BLE001
            respond({"error": f"识别失败：{exc}"})
    return 0


if __name__ == "__main__":
    sys.exit(main())
