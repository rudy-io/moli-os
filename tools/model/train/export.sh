#!/bin/sh
# LoRA adapter -> merged weights -> clean folder (the base model's own config and tokenizer) -> GGUF f16 ->
# Q4_K_M, with llama.cpp in its own venv (its requirements replace torch: never in the training venv).
#   RUN=runs/v4 NAME=moli-home-1.7b-v4 LLAMA=~/llama.cpp sh export.sh
set -e
cd "$(dirname "$0")"
RUN=${RUN:-runs/v4} NAME=${NAME:-moli-home-1.7b-v4} LLAMA=${LLAMA:-$HOME/llama.cpp}
here=$(pwd)
echo "=== fusion $(date +%H:%M)"
python3 merge_lora.py --adapter "$RUN" --out "merged/$NAME"
rm -rf "clean/$NAME" && mkdir -p "clean/$NAME" gguf
S=$(ls -d "$HOME"/.cache/huggingface/hub/models--Qwen--Qwen3-1.7B/snapshots/*/ | head -1)
cp "merged/$NAME/model.safetensors" "clean/$NAME/"
# Never the base folder on top of the merged one: the converter would read the base weights.
for f in config.json generation_config.json tokenizer.json tokenizer_config.json vocab.json merges.txt; do cp "$S$f" "clean/$NAME/"; done
python3 - "clean/$NAME/model.safetensors" "$S" <<'PY'
import glob, sys
from safetensors import safe_open
a = safe_open(sys.argv[1], "pt")
b = safe_open(sorted(glob.glob(sys.argv[2] + "model-00001-of-*.safetensors"))[0], "pt")
k = "model.layers.10.mlp.down_proj.weight"
assert (a.get_tensor(k).float() - b.get_tensor(k).float()).norm().item() > 0, "merged weights are the base's"
print("poids fusionnés ≠ base : oui")
PY
echo "=== conversion $(date +%H:%M)"
(cd "$LLAMA" && python3 convert_hf_to_gguf.py "$here/clean/$NAME" --outtype f16 --outfile "$here/gguf/$NAME-f16.gguf" 2>&1 | tail -1 \
  && ./build/bin/llama-quantize "$here/gguf/$NAME-f16.gguf" "$here/gguf/$NAME-q4_k_m.gguf" Q4_K_M > /dev/null 2>&1)
(cd gguf && sha256sum "$NAME-q4_k_m.gguf" | tee "$NAME-q4_k_m.gguf.sha256")
echo "=== fini $(date +%H:%M) : python3 make_modelfile.py pour le Modelfile"
