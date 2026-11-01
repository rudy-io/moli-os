#!/usr/bin/env python3
"""Fuse a LoRA adapter into its base model and save the full bf16 model (input of the GGUF export).
    python3 merge_lora.py --adapter runs/v4 --out merged/moli-home-1.7b-v4"""
import argparse

import torch
from peft import PeftModel
from transformers import AutoModelForCausalLM, AutoTokenizer

a = argparse.ArgumentParser()
a.add_argument("--model", default="Qwen/Qwen3-1.7B")
a.add_argument("--adapter", required=True)
a.add_argument("--out", required=True)
a = a.parse_args()
model = AutoModelForCausalLM.from_pretrained(a.model, dtype=torch.bfloat16)
model = PeftModel.from_pretrained(model, a.adapter).merge_and_unload()
model.save_pretrained(a.out, safe_serialization=True)
AutoTokenizer.from_pretrained(a.model).save_pretrained(a.out)
print("merged ->", a.out)
