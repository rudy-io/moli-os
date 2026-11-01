# Affinage LoRA du modèle maison

`train_lora.py` : LoRA bf16 (r = 16, tous les modules linéaires) sur Qwen3-1.7B, perte sur la **réponse de
l'assistant seulement**, gabarit de conversation du modèle avec les outils du contrat **avant** le prompt de la
maison (`qwen3_tools_first.jinja`), réflexion désactivée. Pas de quantification à l'entraînement.

Installation : PyTorch pour votre carte (ROCm ou CUDA), puis `pip install -r requirements.txt`. Avec ROCm sous
WSL2 : `HSA_ENABLE_DXG_DETECTION=1`, `TORCH_ROCM_AOTRITON_ENABLE_EXPERIMENTAL=1`,
`PYTORCH_ALLOC_CONF=expandable_segments:True`.

## Entraîner et évaluer

    python3 -m moli_model.build --binary <moli-os> --out data --houses 600 --per-house 40 --seed 7   # depuis tools/model
    DATA=data RUN=runs/v4 INIT=runs/v3 LIMIT=6500 nohup sh run.sh > run.log 2>&1 &

`run.sh` entraîne (trois essais, chacun reprenant au dernier état sauvegardé), puis évalue sur `dev` et `test`
(`eval_model.py` : `RESULT … overall=… unsafe=… invalid=…`). Les versions exactes des bibliothèques de l'entraînement
sont écrites à côté de l'adaptateur (`runs/v4.pip-freeze.txt`). Le modèle v4 publié : 6 500 exemples à partir de
l'adaptateur v3, une époque.

## Exporter vers Ollama

    RUN=runs/v4 NAME=moli-home-1.7b-v4 LLAMA=~/llama.cpp sh export.sh
    python3 make_modelfile.py --gguf ./moli-home-1.7b-v4-q4_k_m.gguf > Modelfile
    ollama create moli-home-v4 -f Modelfile
    python3 eval_ollama.py --data data/test.jsonl --n 100 --model moli-home-v4   # même chemin que Moli (API de chat)

`export.sh` : fusion, dossier propre (`model.safetensors` + config et tokenizer **d'origine** de Qwen), GGUF f16 puis
Q4_K_M avec llama.cpp, et l'empreinte SHA-256 du fichier.

## Mesures (carte grand public de 16 Go, ROCm)
- Un exemple = environ 1 865 tokens (prompt, inventaire et outils ; la réponse n'en fait que quelques dizaines).
- 1 185 tokens/s, 8,2 Gio, lot de 1 avec checkpointing de gradient. Lot de 2, ou sans checkpointing : mémoire
  insuffisante (logits sur 151 000 mots).
- 16 000 exemples ≈ 7 h par époque ; 3 000 ≈ 80 min.

## Pièges
- Ne **jamais** copier tout le dossier du modèle de base par-dessus le dossier fusionné : le convertisseur lit alors
  les poids de base (GGUF = modèle non affiné). `export.sh` le vérifie.
- `pip install -r requirements` de llama.cpp **remplace torch, numpy et transformers** : un venv à part.
- **Garder le Modelfile généré** : avec le gabarit Qwen3 par défaut, Ollama réécrit les outils dans un autre format et
  la précision chute (79 % au lieu de 97 % mesurés en v1).
- Un seul modèle chargé à la fois dans Ollama (mémoire) : sinon échange disque et latences faussées.
