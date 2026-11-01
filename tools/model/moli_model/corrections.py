"""The house's own corrections (« Pas ça ») as evaluation records: what the next model is measured on.

Run at home, on files exported by hand from the house; the output is the house's (its rooms, its devices,
what was said there) and never goes in the repository.

    python3 -m moli_model.corrections --binary <moli-os> \\
        --journal assistant-corrections.1.jsonl assistant-corrections.jsonl \\
        --devices devices.json --home home.json --out ~/moli-train/reel.jsonl

`devices.json` is what `GET /api/devices` answers (its devices and its guard), `home.json` the house's layout:
the prompt is rendered from them by Moli's own code, so the numbers (`d1`…) are the ones the model will see.
Who corrected and when are left out.
"""
import argparse
import json
from pathlib import Path

from . import build, contract, render

# A correction that says « ne rien faire »: the right answer is words, not a call.
NOTHING = "D'accord, je ne fais rien pour ça."


def house_of(snapshot, home):
    """What `moli-os model prompt` reads, from `GET /api/devices` and `home.json`."""
    return {
        "layout": home,
        "protected": snapshot.get("guard", {}).get("protected_rooms", []),
        "devices": snapshot["devices"],
    }


def _context(pairs):
    """The journal's [(role, text)…] as the records' [(said, answered)…]."""
    out, said = [], None
    for role, text in pairs:
        if role == "user":
            said = text
        elif role == "assistant" and said is not None:
            out.append((said, text))
            said = None
    return out


def records(lines, prompt, aliases, tools):
    """(records, skipped): one record per correction; skipped when it names a device the prompt no longer
    lists (gone, hidden, renamed out of the house)."""
    number = {real: alias for alias, real in aliases.items()}
    out, skipped = [], 0
    for line in lines:
        if line.get("kind") != "correct" or not line.get("question"):
            continue
        calls = [{"tool": c["tool"], "arguments": c.get("arguments", {})}
                 for c in (line.get("choices") or [line["choice"]])]
        if any(c["tool"] == "nothing" for c in calls):
            answer = {"text": NOTHING}
        elif len(calls) == 1:
            answer = calls[0]
        else:
            answer = {"calls": calls}
        case = {"user": line["question"], "context": _context(line.get("context", [])), "answer": answer}
        record = build.record_of(case, prompt, number, tools, {"cat": "reel", "source": "correction"})
        if record is None:
            skipped += 1
        else:
            out.append(record)
    return out, skipped


def main():
    a = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    a.add_argument("--binary", required=True, help="moli-os, to render the house's prompt")
    a.add_argument("--journal", required=True, nargs="+",
                   help="assistant-corrections.jsonl (and the older .1.jsonl, oldest first)")
    a.add_argument("--devices", required=True, help="what GET /api/devices answers")
    a.add_argument("--home", required=True, help="home.json")
    a.add_argument("--out", required=True, help="where the evaluation records go (outside the repository)")
    args = a.parse_args()
    snapshot = json.loads(Path(args.devices).read_text(encoding="utf-8"))
    home = json.loads(Path(args.home).read_text(encoding="utf-8"))
    prompt, aliases = render.render_prompt(args.binary, house_of(snapshot, home))
    lines = [json.loads(raw) for journal in args.journal
             for raw in Path(journal).read_text(encoding="utf-8").splitlines() if raw.strip()]
    out, skipped = records(lines, prompt, aliases, contract.load()["tools"])
    Path(args.out).write_text("".join(json.dumps(r, ensure_ascii=False) + "\n" for r in out), encoding="utf-8")
    print(f"{len(out)} corrections → {args.out} ({skipped} sur un appareil que la maison n'a plus)")


if __name__ == "__main__":
    main()
