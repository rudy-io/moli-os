#!/usr/bin/env python3
"""LoRA bf16 fine-tune of a small chat model on the Moli house data (train.jsonl).
Loss on the assistant answer only. Meant for a ROCm or CUDA GPU:
    python3 train_lora.py --data data/train.jsonl --out runs/qwen3-1.7b --limit 2000 --epochs 1"""
import argparse
import json
import math
import random
import shutil
import time
from pathlib import Path

import torch
from peft import LoraConfig, get_peft_model
from transformers import AutoModelForCausalLM, AutoTokenizer


# Contract v4: the tools before the house's prompt (kept cached when the house changes).
TEMPLATE = (Path(__file__).resolve().parent / "qwen3_tools_first.jinja").read_text(encoding="utf-8")


def encode(tok, record, max_len):
    """(input_ids, labels): everything before the last assistant answer is masked with -100
    (earlier turns of a dialogue are context, not what is learned)."""
    msgs, tools = record["messages"], record["tools"]
    kw = dict(tools=tools, tokenize=False, enable_thinking=False, chat_template=TEMPLATE)
    prompt = tok.apply_chat_template(msgs[:-1], add_generation_prompt=True, **kw)
    full = tok.apply_chat_template(msgs, **kw)
    assert full.startswith(prompt), "the chat template changes the prompt once the answer is added"
    p = tok(prompt, add_special_tokens=False)["input_ids"]
    f = tok(full, add_special_tokens=False)["input_ids"]
    if len(f) > max_len:
        return None
    return f, [-100] * len(p) + f[len(p):]


def batches(rows, size, pad):
    for i in range(0, len(rows), size):
        chunk = rows[i:i + size]
        n = max(len(ids) for ids, _ in chunk)
        ids = torch.tensor([x + [pad] * (n - len(x)) for x, _ in chunk])
        lab = torch.tensor([y + [-100] * (n - len(y)) for _, y in chunk])
        yield ids, lab, (ids != pad) | (lab != -100)


def main():
    a = argparse.ArgumentParser()
    a.add_argument("--model", default="Qwen/Qwen3-1.7B")
    a.add_argument("--data", required=True)
    a.add_argument("--out", required=True)
    a.add_argument("--limit", type=int, default=0, help="use only N examples (pilot run)")
    a.add_argument("--epochs", type=float, default=2)
    a.add_argument("--max-steps", type=int, default=0)
    a.add_argument("--batch", type=int, default=2)
    a.add_argument("--accum", type=int, default=8)
    a.add_argument("--lr", type=float, default=2e-4)
    a.add_argument("--rank", type=int, default=16)
    a.add_argument("--max-len", type=int, default=3072)
    a.add_argument("--seed", type=int, default=1)
    a.add_argument("--save-every", type=int, default=0, help="save an adapter checkpoint every N steps")
    a.add_argument("--resume-every", type=int, default=100, help="save a resumable state every N steps (0 = never)")
    a.add_argument("--resume", action="store_true", help="continue from the last resumable state in --out")
    a.add_argument("--init-adapter", help="start from an earlier run's adapter (same rank) instead of zero")
    a.add_argument("--no-checkpointing", action="store_true", help="faster, needs more memory")
    a = a.parse_args()
    random.seed(a.seed)
    torch.manual_seed(a.seed)

    tok = AutoTokenizer.from_pretrained(a.model)
    records = [json.loads(line) for line in open(a.data, encoding="utf-8")]
    random.shuffle(records)
    if a.limit:
        records = records[:a.limit]
    rows = [r for r in (encode(tok, rec, a.max_len) for rec in records) if r]
    toks = sum(len(i) for i, _ in rows)
    print(f"{len(rows)}/{len(records)} examples kept, {toks / len(rows):.0f} tokens each, {toks / 1e6:.1f} M tokens per epoch", flush=True)

    model = AutoModelForCausalLM.from_pretrained(a.model, dtype=torch.bfloat16, attn_implementation="sdpa").cuda()
    if not a.no_checkpointing:
        model.gradient_checkpointing_enable()
        model.enable_input_require_grads()
    model = get_peft_model(model, LoraConfig(r=a.rank, lora_alpha=2 * a.rank, lora_dropout=0.05, task_type="CAUSAL_LM",
                                             target_modules=["q_proj", "k_proj", "v_proj", "o_proj", "gate_proj", "up_proj", "down_proj"]))
    if a.init_adapter and not a.resume:
        from peft import set_peft_model_state_dict
        from safetensors.torch import load_file
        missing = set_peft_model_state_dict(model, load_file(str(Path(a.init_adapter) / "adapter_model.safetensors")))
        print(f"started from {a.init_adapter} (unexpected keys: {len(missing.unexpected_keys)})", flush=True)
    model.print_trainable_parameters()
    opt = torch.optim.AdamW([p for p in model.parameters() if p.requires_grad], lr=a.lr, weight_decay=0.0)

    per_epoch = math.ceil(len(rows) / (a.batch * a.accum))
    total = a.max_steps or int(per_epoch * a.epochs)
    warm = max(1, total // 30)
    lr_at = lambda s: a.lr * (s + 1) / warm if s < warm else a.lr * 0.5 * (1 + math.cos(math.pi * (s - warm) / max(1, total - warm)))
    step, seen, t0, run = 0, 0, time.time(), []
    state_dir = Path(a.out) / "resume"
    if a.resume and (state_dir / "state.pt").exists():
        from peft import set_peft_model_state_dict
        from safetensors.torch import load_file
        set_peft_model_state_dict(model, load_file(str(state_dir / "adapter_model.safetensors")))
        # Only tensors and numbers: the optimizer's state and the step this script wrote itself.
        state = torch.load(state_dir / "state.pt", weights_only=True)
        opt.load_state_dict(state["opt"])
        step = state["step"]
        for _ in range(step // per_epoch):  # the shuffles of the epochs already done
            random.shuffle(rows)
        print(f"resumed at step {step}/{total}", flush=True)
    skip = (step % per_epoch) * a.accum  # micro-batches of the current epoch already trained on
    model.train()
    while step < total:
        random.shuffle(rows)
        for i, (ids, lab, mask) in enumerate(batches(rows, a.batch, tok.pad_token_id)):
            if i < skip:
                continue
            out = model(input_ids=ids.cuda(), attention_mask=mask.cuda(), labels=lab.cuda())
            (out.loss / a.accum).backward()
            run.append(out.loss.item())
            seen += int(mask.sum())
            if (i + 1) % a.accum:
                continue
            for g in opt.param_groups:
                g["lr"] = lr_at(step)
            torch.nn.utils.clip_grad_norm_(model.parameters(), 1.0)
            opt.step(); opt.zero_grad(set_to_none=True)
            step += 1
            if step % 5 == 0 or step == total:
                dt = time.time() - t0
                print(f"step {step}/{total} loss {sum(run) / len(run):.4f} lr {lr_at(step):.2e} {seen / dt:.0f} tok/s "
                      f"elapsed {dt / 60:.1f} min eta {dt / step * (total - step) / 60:.0f} min mem {torch.cuda.max_memory_allocated() / 2**30:.1f} GiB", flush=True)
                run = []
            if a.save_every and step % a.save_every == 0:
                model.save_pretrained(f"{a.out}/step{step}")
            if a.resume_every and step % a.resume_every == 0:
                tmp = Path(a.out) / "resume.tmp"  # written whole, then swapped in: a crash never leaves half a state
                model.save_pretrained(tmp)
                torch.save({"opt": opt.state_dict(), "step": step}, tmp / "state.pt")
                if state_dir.exists():
                    shutil.rmtree(state_dir)
                tmp.rename(state_dir)
            if step >= total:
                break
        skip = 0
    model.save_pretrained(a.out)
    tok.save_pretrained(a.out)
    print("saved", a.out)


if __name__ == "__main__":
    main()
