#!/usr/bin/env python3
"""End-to-end check through Ollama's chat API (the path Moli uses): accuracy per category and latency.
    python3 eval_ollama.py --data data/test.jsonl --n 100 --model moli-home-v4"""
import argparse
import json
import os
import random
import statistics
import sys
import time
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from moli_model import score  # noqa: E402

HOST = os.environ.get("OLLAMA_URL", "http://127.0.0.1:11434")


def chat(model, rec):
    body = {"model": model, "messages": rec["messages"][:-1], "tools": rec["tools"], "stream": False,
            "options": {"temperature": 0, "num_predict": 160}}
    req = urllib.request.Request(HOST + "/api/chat", json.dumps(body).encode(), {"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=300) as r:
        return json.load(r)


def as_text(message):
    """What the model said, in the form score.parse reads."""
    calls = message.get("tool_calls") or []
    if calls:  # every call: an answer of several is scored whole
        return "\n".join("<tool_call>\n" + json.dumps({"name": c["function"]["name"], "arguments": c["function"]["arguments"]},
                                                     ensure_ascii=False) + "\n</tool_call>" for c in calls)
    return message.get("content", "")


def main():
    a = argparse.ArgumentParser()
    a.add_argument("--data", required=True)
    a.add_argument("--model", default="moli-home-v4")
    a.add_argument("--n", type=int, default=100)
    a.add_argument("--seed", type=int, default=1)
    a.add_argument("--out")
    a.add_argument("--houses", type=int, default=0, help="N houses, --per questions each, asked one house at a time (prefix cache)")
    a.add_argument("--per", type=int, default=8)
    a = a.parse_args()
    records = [json.loads(line) for line in open(a.data, encoding="utf-8")]
    random.Random(a.seed).shuffle(records)
    if a.houses:
        by = {}
        for rec in records:
            by.setdefault(rec["meta"]["house"], []).append(rec)
        records = [r for h in list(by)[:a.houses] for r in by[h][:a.per]]
        a.n = len(records)
    results, times = [], []
    for i, rec in enumerate(records[:a.n]):
        t = time.time()
        resp = chat(a.model, rec)
        dt = time.time() - t
        if i:  # the first request loads the model
            times.append(dt)
        first_of_house = i == 0 or rec["meta"]["house"] != records[i - 1]["meta"]["house"]
        text = as_text(resp["message"])
        r = score.score(rec, text)
        r.update(cat=rec["meta"]["cat"], new_house=first_of_house, user=rec["messages"][-2]["content"], output=text[:300], seconds=round(dt, 1),
                 prompt_tokens=resp.get("prompt_eval_count"))
        results.append(r)
        if (i + 1) % 20 == 0:
            print(f"{i + 1}/{a.n} ok {sum(x['ok'] for x in results) / len(results):.2f} median {statistics.median(times):.1f}s", flush=True)
    summary = score.summarize(results)
    summary["latency_median_s"] = round(statistics.median(times), 2)
    summary["latency_p90_s"] = round(sorted(times)[int(len(times) * 0.9)], 2)
    warm = [x["seconds"] for x in results if not x["new_house"]]
    cold = [x["seconds"] for x in results if x["new_house"]]
    if warm and cold:
        summary["latency_same_house_median_s"] = round(statistics.median(warm), 2)
        summary["latency_new_house_median_s"] = round(statistics.median(cold), 2)
    print(json.dumps(summary, ensure_ascii=False, indent=1))
    if a.out:
        json.dump({"summary": summary, "results": results}, open(a.out, "w", encoding="utf-8"), ensure_ascii=False, indent=1)


if __name__ == "__main__":
    main()
