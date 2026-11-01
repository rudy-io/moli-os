#!/bin/sh
# One training run, then its evaluation on dev and test. From the training folder, venv active:
#   DATA=data RUN=runs/v4 INIT=runs/v3 LIMIT=6500 nohup sh run.sh > run.log 2>&1 &
# DATA: what `moli_model.build` wrote; RUN: where the adapter goes; INIT: an adapter to start from
# (optional); LIMIT: examples of train.jsonl used. The v4 model: 6 500 examples from the v3 adapter.
cd "$(dirname "$0")"
DATA=${DATA:-data} RUN=${RUN:-runs/v4} LIMIT=${LIMIT:-6500}
name=$(basename "$RUN")
mkdir -p "$(dirname "$RUN")"
# The exact versions this run trains with, to train it again the same way.
pip freeze > "$RUN.pip-freeze.txt"
echo "=== entraînement $name $(date +%H:%M)"
# the driver may fail under load: up to three tries, each resuming from the last saved state
ok=0
for attempt in 1 2 3; do
  python3 train_lora.py --model Qwen/Qwen3-1.7B --data "$DATA/train.jsonl" --out "$RUN" --limit "$LIMIT" \
    --epochs 1 --max-len 6144 ${INIT:+--init-adapter "$INIT"} --batch 1 --accum 8 --save-every 250 --resume \
    >> "logs-$name-train.log" 2>&1 && { ok=1; break; }
  echo "~~ tentative $attempt en échec $(date +%H:%M)"; sleep 60
done
[ "$ok" = 1 ] || { echo "!! entraînement en échec"; exit 1; }
grep -E "^(step|saved)" "logs-$name-train.log" | tail -2
mkdir -p results
for split in "dev 500" "test 600"; do
  set -- $split
  echo "=== évaluation $1 $(date +%H:%M)"
  python3 eval_model.py --adapter "$RUN" --data "$DATA/$1.jsonl" --n "$2" --out "results/$name-$1.json" \
    > "logs-$name-eval-$1.log" 2>&1 || { echo "!! évaluation $1 en échec"; continue; }
  python3 - "results/$name-$1.json" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))["summary"]
print(f"RESULT {sys.argv[1]} overall={s['overall']:.3f} unsafe={s['unsafe']} invalid={s['invalid']} ", {k: round(v, 2) for k, v in s["by_cat"].items()})
PY
done
echo "=== fini $(date +%H:%M)"
