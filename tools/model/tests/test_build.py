import json
import os
import random
import tempfile
import unittest
from pathlib import Path

from moli_model import build, contract, houses


def fake_render(house):
    """What `moli-os model prompt` gives: the prompt and the number of each device (d1, d2…)."""
    prompt = f"Contrat Moli v{contract.load()['version']}. (test)\n" + ",".join(f"d{i + 1}" for i, _ in enumerate(house["devices"]))
    return prompt, {f"d{i + 1}": d["id"] for i, d in enumerate(house["devices"])}


class BuildTest(unittest.TestCase):
    def run_build(self, houses_count=30, per_house=20, seed=3):
        out = Path(tempfile.mkdtemp())
        stats = build.build(out, houses_count, per_house, seed, fake_render)
        return out, stats

    def read(self, out, name):
        return [json.loads(line) for line in (out / name).read_text(encoding="utf-8").splitlines()]

    def test_it_writes_three_splits_and_a_data_card(self):
        out, stats = self.run_build()
        for name in ("train.jsonl", "dev.jsonl", "test.jsonl", "DATA_CARD.md"):
            self.assertTrue((out / name).exists(), name)
        self.assertGreater(stats["train"], 0)
        self.assertGreater(stats["test"], 0)
        self.assertIn("seed", (out / "DATA_CARD.md").read_text(encoding="utf-8"))

    def test_a_house_is_in_one_split_only(self):
        out, _ = self.run_build()
        owner = {}
        for name in ("train", "dev", "test"):
            for r in self.read(out, f"{name}.jsonl"):
                self.assertEqual(owner.setdefault(r["meta"]["house"], name), name, "a house in two splits")

    def test_a_record_is_a_chat_with_tools_and_one_answer(self):
        out, _ = self.run_build()
        r = self.read(out, "train.jsonl")[0]
        self.assertEqual([m["role"] for m in r["messages"]], ["system", "user", "assistant"])
        self.assertTrue(r["messages"][0]["content"].startswith("Contrat Moli v4"))
        self.assertEqual(len(r["tools"]), len(contract.load()["tools"]))
        a = r["messages"][-1]
        self.assertTrue(a.get("tool_calls") or a.get("content"))

    def test_answers_speak_in_device_numbers_never_real_identifiers(self):
        out, _ = self.run_build()
        seen = 0
        for r in self.read(out, "train.jsonl"):
            calls = r["messages"][-1].get("tool_calls")
            if not calls:
                continue
            args = calls[0]["function"]["arguments"]
            for field in ("point", "device"):
                if field in args:
                    seen += 1
                    self.assertRegex(args[field], r"^d\d+(/.+)?$", args)
                    self.assertNotIn("z2m:", args[field])
        self.assertGreater(seen, 100)

    def test_no_duplicate_house_and_request(self):
        out, _ = self.run_build()
        seen = set()
        for name in ("train", "dev", "test"):
            for r in self.read(out, f"{name}.jsonl"):
                key = (r["meta"]["house"], json.dumps(r["messages"][1:-1], ensure_ascii=False))
                self.assertNotIn(key, seen)
                seen.add(key)

    def test_the_test_split_has_unseen_families_and_the_training_has_none(self):
        out, _ = self.run_build(houses_count=60)
        train_fams = {f for r in self.read(out, "train.jsonl") for f in r["meta"]["families"]}
        test_fams = {f for r in self.read(out, "test.jsonl") for f in r["meta"]["families"]}
        self.assertFalse(train_fams & houses.TEST_ONLY_FAMILIES)
        self.assertTrue(test_fams & houses.TEST_ONLY_FAMILIES)

    def test_the_same_seed_gives_the_same_files(self):
        a, _ = self.run_build(seed=9)
        b, _ = self.run_build(seed=9)
        self.assertEqual((a / "train.jsonl").read_bytes(), (b / "train.jsonl").read_bytes())

    def test_dev_and_test_repeat_nothing_the_training_already_holds(self):
        out, _ = self.run_build(houses_count=60)
        def key(r):
            return (json.dumps(r["messages"][1:-1], ensure_ascii=False), json.dumps(r["messages"][-1], sort_keys=True, ensure_ascii=False))
        train = {key(r) for r in self.read(out, "train.jsonl")}
        for name in ("dev", "test"):
            for r in self.read(out, f"{name}.jsonl"):
                self.assertNotIn(key(r), train)

    @unittest.skipUnless(os.environ.get("MOLI_BIN"), "set MOLI_BIN to the moli-os binary to test against the real prompt")
    def test_with_the_real_binary_the_prompt_carries_the_real_inventory(self):
        from moli_model import render
        out = Path(tempfile.mkdtemp())
        build.build(out, 8, 10, 1, lambda h: render.render_prompt(os.environ["MOLI_BIN"], h))
        r = json.loads((out / "train.jsonl").read_text(encoding="utf-8").splitlines()[0])
        self.assertIn("Appareils :", r["messages"][0]["content"])
        self.assertIn("- d1 · ", r["messages"][0]["content"])


if __name__ == "__main__":
    unittest.main()


class V4RecordTest(unittest.TestCase):
    def test_several_calls_and_a_dialogue_become_one_record(self):
        h = houses.make_house(random.Random(1), "h1", "train")
        a, b = h["devices"][0]["id"], h["devices"][1]["id"]
        number = {a: "d1", b: "d2"}
        case = {"cat": "multi", "user": "Et la deuxième ?", "context": [("Allume la première.", "C’est fait : Lampe.")],
                "answer": {"calls": [{"tool": "set", "arguments": {"point": f"{a}/state", "value": True}},
                                     {"tool": "get_device", "arguments": {"device": b}}]}}
        record = build.record_of(case, "PROMPT", number, [], {"house": "h1", "cat": "multi", "families": []})
        roles = [m["role"] for m in record["messages"]]
        self.assertEqual(roles, ["system", "user", "assistant", "user", "assistant"])
        self.assertEqual(record["messages"][2]["content"], "C’est fait : Lampe.")
        calls = record["messages"][-1]["tool_calls"]
        self.assertEqual([c["function"]["arguments"] for c in calls], [{"point": "d1/state", "value": True}, {"device": "d2"}])

    def test_a_call_naming_an_unlisted_device_drops_the_case(self):
        case = {"cat": "multi", "user": "x", "answer": {"calls": [{"tool": "get_device", "arguments": {"device": "z2m:nope"}}]}}
        self.assertIsNone(build.record_of(case, "P", {}, [], {}))
