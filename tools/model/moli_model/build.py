"""Builds the fine-tuning set: houses -> cases -> validated chat records, split by house."""
import argparse
import hashlib
import json
import random
from collections import Counter
from pathlib import Path

from . import contract, houses, leak, phrases, validate


def _key(record):
    return json.dumps(record["messages"][1:-1], ensure_ascii=False), json.dumps(record["messages"][-1], sort_keys=True, ensure_ascii=False)


def to_numbers(answer, number):
    """The answer as the model must give it: device numbers (`d3/state`), never real identifiers.
    None when it names a device the prompt does not list."""
    if "tool" not in answer:
        return answer
    args = dict(answer["arguments"])
    for field in ("point", "device"):
        if field in args:
            device, slash, key = args[field].partition("/")
            if device not in number:
                return None
            args[field] = number[device] + slash + key
    return {**answer, "arguments": args}


def calls_of(answer):
    """The calls of an answer: one (`tool`), several (`calls`), or none (text)."""
    if "calls" in answer:
        return answer["calls"]
    return [answer] if "tool" in answer else []


def record_of(case, prompt, number, tools, meta):
    """The chat record of a case: the prompt, the dialogue before (`context`), the request and the answer
    in device numbers; None when a call names a device the prompt does not list."""
    calls = [to_numbers(c, number) for c in calls_of(case["answer"])]
    if any(c is None for c in calls):
        return None
    if calls:
        message = {"role": "assistant", "content": "", "tool_calls": [
            {"type": "function", "function": {"name": c["tool"], "arguments": c["arguments"]}} for c in calls]}
    else:
        message = {"role": "assistant", "content": case["answer"]["text"]}
    messages = [{"role": "system", "content": prompt}]
    for said, answered in case.get("context", []):
        messages += [{"role": "user", "content": said}, {"role": "assistant", "content": answered}]
    messages += [{"role": "user", "content": case["user"]}, message]
    return {"messages": messages, "tools": tools, "meta": meta}


def split_of(index):
    """80 % train, 10 % dev, 10 % test, par maison."""
    r = index % 10
    return "test" if r == 9 else "dev" if r == 8 else "train"


def build(out_dir, houses_count, per_house, seed, render):
    """Writes train/dev/test.jsonl and DATA_CARD.md in `out_dir`; returns the counts.
    `render(house) -> (prompt, aliases)` gives the system prompt and which device each number stands for
    (Moli's own code in production)."""
    out = Path(out_dir)
    out.mkdir(parents=True, exist_ok=True)
    spec = contract.load()
    tools, version = spec["tools"], spec["version"]
    rnd = random.Random(seed)
    files = {name: [] for name in ("train", "dev", "test")}
    rejected = Counter()
    by_cat = {name: Counter() for name in files}
    for index in range(houses_count):
        split = split_of(index)
        house = houses.make_house(random.Random(rnd.random()), f"h{index:05d}", split)
        prompt, aliases = render(house)
        number = {device: alias for alias, device in aliases.items()}
        families = sorted({m["family"] for m in house["meta"].values()})
        for case in phrases.generate(random.Random(rnd.random()), house, per_house):
            problems = validate.validate(case, house, tools) or leak.scan(case["user"] + json.dumps(case["answer"], ensure_ascii=False))
            if problems:
                rejected[str(problems[0])[:60]] += 1
                continue
            record = record_of(case, prompt, number, tools, {"house": house["id"], "cat": case["cat"], "families": families})
            if record is None:
                rejected["names a device the prompt does not list"] += 1
                continue
            files[split].append(record)
            by_cat[split][case["cat"]] += 1
    seen = {_key(r) for r in files["train"]}
    dropped = 0
    for name in ("dev", "test"):  # a question the training already holds would only measure memory
        kept = [r for r in files[name] if _key(r) not in seen]
        dropped += len(files[name]) - len(kept)
        files[name] = kept
    counts = {}
    for name, records in files.items():
        with (out / f"{name}.jsonl").open("w", encoding="utf-8") as handle:
            for record in records:
                handle.write(json.dumps(record, ensure_ascii=False) + "\n")
        counts[name] = len(records)
    counts["rejected"] = sum(rejected.values())
    counts["dropped_seen_in_train"] = dropped
    _datacard(out, version, seed, houses_count, per_house, counts, by_cat, rejected)
    return counts


def _datacard(out, version, seed, houses_count, per_house, counts, by_cat, rejected):
    digest = hashlib.sha256(b"".join((out / f"{n}.jsonl").read_bytes() for n in ("train", "dev", "test"))).hexdigest()[:16]
    lines = ["# Fiche de données : modèle maison de Moli", "",
             f"- Contrat des outils : version {version}", f"- Graine : seed = {seed} ; maisons : {houses_count} ; demandes par maison au plus : {per_house}",
             "- Données **entièrement synthétiques** : maisons, prénoms et villes fictifs, aucune donnée de maison réelle.",
             "- Découpage par maison : 80 % entraînement, 10 % validation, 10 % test ; le test contient des familles d'appareils (ventilateur) et des pièces (buanderie, cave) absentes de l'entraînement, et des formulations tenues à l'écart.",
             f"- Empreinte du jeu : {digest}", "", "## Effectifs", ""]
    for name in ("train", "dev", "test"):
        lines.append(f"- {name} : {counts[name]} exemples ({', '.join(f'{c} {n}' for c, n in sorted(by_cat[name].items()))})")
    lines += ["", f"Retirés de dev et test (déjà dans train) : {counts['dropped_seen_in_train']}", "", f"## Rejetés par la validation : {counts['rejected']}", ""] + [f"- {k} : {v}" for k, v in rejected.most_common(10)]
    (out / "DATA_CARD.md").write_text("\n".join(lines) + "\n", encoding="utf-8")


def main():
    from . import render
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--binary", required=True, help="chemin du binaire moli-os (rend le vrai prompt)")
    p.add_argument("--out", required=True)
    p.add_argument("--houses", type=int, default=600)
    p.add_argument("--per-house", type=int, default=40)
    p.add_argument("--seed", type=int, default=7)
    a = p.parse_args()
    counts = build(a.out, a.houses, a.per_house, a.seed, lambda h: render.render_prompt(a.binary, h))
    print(json.dumps(counts, ensure_ascii=False))


if __name__ == "__main__":
    main()
