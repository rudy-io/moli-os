#!/usr/bin/env python3
"""Runs a model (optionally with a LoRA adapter) on held-out records and scores it per category.
    python3 eval_model.py --data data/dev.jsonl --n 300 [--adapter runs/x] --out results/base.json"""
import argparse
import json
import random
import sys
import time
from pathlib import Path

import torch
from train_lora import TEMPLATE  # noqa: E402  (same template as training)
from transformers import AutoModelForCausalLM, AutoTokenizer

sys.path.insert(0, str(Path(__file__).resolve().parent))
from moli_model import score  # noqa: E402


def main():
    a = argparse.ArgumentParser()
    a.add_argument("--model", default="Qwen/Qwen3-1.7B")
    a.add_argument("--adapter")
    a.add_argument("--data", required=True)
    a.add_argument("--n", type=int, default=300)
    a.add_argument("--out", required=True)
    a.add_argument("--seed", type=int, default=1)
    a = a.parse_args()

    tok = AutoTokenizer.from_pretrained(a.model)
    model = AutoModelForCausalLM.from_pretrained(a.model, dtype=torch.bfloat16, attn_implementation="sdpa").cuda().eval()
    if a.adapter:
        from peft import PeftModel
        model = PeftModel.from_pretrained(model, a.adapter).eval()
    records = [json.loads(line) for line in open(a.data, encoding="utf-8")]
    random.Random(a.seed).shuffle(records)
    records = records[:a.n]

    results, t0 = [], time.time()
    for i, rec in enumerate(records):
        prompt = tok.apply_chat_template(rec["messages"][:-1], tools=rec["tools"], add_generation_prompt=True,
                                         tokenize=False, enable_thinking=False, chat_template=TEMPLATE)
        ids = tok(prompt, return_tensors="pt", add_special_tokens=False).to("cuda")
        with torch.no_grad():
            out = model.generate(**ids, max_new_tokens=160, do_sample=False, pad_token_id=tok.pad_token_id)
        text = tok.decode(out[0][ids["input_ids"].shape[1]:], skip_special_tokens=False)
        r = score.score(rec, text)
        r.update(cat=rec["meta"]["cat"], user=rec["messages"][-2]["content"], output=text.strip()[:400])
        results.append(r)
        if (i + 1) % 25 == 0:
            print(f"{i + 1}/{len(records)} ok so far {sum(x['ok'] for x in results) / len(results):.2f} ({(time.time() - t0) / 60:.1f} min)", flush=True)
    summary = score.summarize(results)
    Path(a.out).parent.mkdir(parents=True, exist_ok=True)
    json.dump({"summary": summary, "results": results}, open(a.out, "w", encoding="utf-8"), ensure_ascii=False, indent=1)
    print(json.dumps(summary, ensure_ascii=False, indent=1))


if __name__ == "__main__":
    main()
