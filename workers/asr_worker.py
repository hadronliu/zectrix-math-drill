"""数字口算 ASR worker（JSON-lines 协议，与背单词应用同模式）。

协议（stdin/stdout 各一行一请求/响应）：
  请求: {"id": "<请求标识>", "path": "<WAV 路径>"}
  响应: {"id": "...", "ok": true, "text": "<识别文本>"}
       {"id": "...", "ok": false, "error": "<原因>"}
启动完成后先输出一行 {"ready": true}。

防偏向原则（开发方案 §8.4）：系统提示词只描述任务形态，
**不含任何标准答案或候选数字**；识别文本原样返回，由宿主做数字解析。
所有第三方库输出重定向到 stderr，stdout 只承载协议行。
"""
import contextlib
import json
import os
import sys
import traceback

# 模型缓存沿用本机 HuggingFace 缓存；huggingface.co 直连不可达时走镜像（可用环境变量覆盖）
os.environ.setdefault("HF_ENDPOINT", "https://hf-mirror.com")
os.environ.setdefault("TOKENIZERS_PARALLELISM", "false")

MODEL_ID = "mlx-community/Qwen3-ASR-1.7B-8bit"
# 只描述「说了什么」，不提示任何数字或答案
NUMBER_CONTEXT = (
    "The recording contains a person saying a single integer in Chinese, "
    "as their spoken answer to an arithmetic question. "
    "Transcribe verbatim what was said, in Chinese characters or digits as spoken. "
    "Do not calculate, correct, complete, expand or normalize the number. "
    "If speech is unclear, transcribe what is audible without guessing a completion."
)


def main() -> None:
    wire = sys.stdout
    with contextlib.redirect_stdout(sys.stderr):
        from mlx_audio.stt import load

        model = load(MODEL_ID)
    print(json.dumps({"ready": True, "model": MODEL_ID}), file=wire, flush=True)

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
            req_id = req.get("id", "")
            with contextlib.redirect_stdout(sys.stderr):
                try:
                    result = model.generate(
                        req["path"],
                        language="Chinese",
                        max_tokens=64,
                        system_prompt=NUMBER_CONTEXT,
                    )
                except (TypeError, ValueError):
                    # 语言参数不被当前版本支持时退化为无参调用
                    result = model.generate(req["path"], max_tokens=64)
            print(
                json.dumps(
                    {"id": req_id, "ok": True, "text": result.text.strip()},
                    ensure_ascii=False,
                ),
                file=wire,
                flush=True,
            )
        except Exception:
            traceback.print_exc(file=sys.stderr)
            print(
                json.dumps(
                    {"id": req.get("id", ""), "ok": False, "error": "识别失败"},
                    ensure_ascii=False,
                ),
                file=wire,
                flush=True,
            )


if __name__ == "__main__":
    main()
