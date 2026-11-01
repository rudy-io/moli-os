import random
import re
import unittest
from moli_model import contract, houses, outofscope, phrases, score, validate

TOOLS = contract.load()["tools"]
COMMAND_VERBS = re.compile(r"^(Allume|Éteins|Ferme|Ouvre|Mets|Règle|Monte|Baisse|Coupe|Active|Démarre|Renomme|Déplace|Masque|Cache|Descends|Relève|Remonte|Lance|Stoppe|Arrête)\b")


def cases(split="train", seeds=range(60)):
    out = []
    for s in seeds:
        out += outofscope.generate(random.Random(s), split == "test")
    return out


class OutOfScopeTest(unittest.TestCase):
    def test_a_question_outside_the_frame_is_delegated_word_for_word(self):
        found = [c for c in cases() if c["cat"] == "hors_cadre"]
        self.assertGreater(len(found), 50)
        for c in found:
            self.assertEqual(c["answer"]["tool"], "delegate")
            self.assertEqual(c["answer"]["arguments"], {"question": c["user"]})

    def test_a_feeling_gets_a_question_and_how_the_house_is_gets_its_summary(self):
        found = [c for c in cases() if c["cat"] == "vague"]
        self.assertTrue(found)
        for c in found:
            self.assertIn("text", c["answer"])
            self.assertTrue(c["answer"]["text"].strip())
        summary = [c for c in cases() if c["cat"] == "problèmes"]
        self.assertGreater(len(summary), 10)
        for c in summary:
            self.assertEqual(c["answer"]["tool"], "problems")

    def test_nothing_here_is_an_order_for_the_house(self):
        for c in cases("train") + cases("test"):
            self.assertIsNone(COMMAND_VERBS.match(c["user"]), c["user"])

    def test_held_out_questions_only_appear_in_test_houses(self):
        train = cases("train", range(120))
        test = {c["user"] for c in cases("test", range(120))}
        self.assertTrue(test - {c["user"] for c in train}, "test houses are asked things the training never saw")
        leaked = {c["base"] for c in train if c.get("base") in outofscope.HELD_OUT_BASES}
        self.assertFalse(leaked, f"held-out bases leaked into the training, wrapped or not: {leaked}")

    def test_a_house_gets_few_of_them_so_they_do_not_swamp_the_set(self):
        counts = [sum(c["cat"] == "hors_cadre" for c in outofscope.generate(random.Random(s), False)) for s in range(200)]
        self.assertLessEqual(max(counts), 2)
        self.assertLess(sum(counts) / len(counts), 1.8)

    def test_a_state_question_is_not_taught_as_vague(self):
        # Moli can read what is on or open: refusing to would be learned.
        users = {c["user"] for c in cases("train") + cases("test")}
        for state in ("Qu'est-ce qui est allumé ?", "Dis-moi ce qui est ouvert."):
            self.assertNotIn(state, users)

    def test_the_weather_is_never_delegated_to_a_model_that_cannot_know(self):
        for c in cases():
            if any(w in c["user"].lower() for w in ("quel temps fait", "pleuvoir", "faire beau")):
                self.assertNotEqual(c["answer"].get("tool"), "delegate", c)

    def test_the_same_seed_gives_the_same_cases(self):
        self.assertEqual(outofscope.generate(random.Random(4), False), outofscope.generate(random.Random(4), False))

    def test_the_generator_of_a_house_includes_them(self):
        seen = set()
        for s in range(40):
            h = houses.make_house(random.Random(s), f"h{s}", "train")
            seen |= {c["cat"] for c in phrases.generate(random.Random(s), h, 80)}
        self.assertTrue({"hors_cadre", "vague"} <= seen, seen)

    def test_validate_accepts_a_faithful_delegation_and_refuses_the_rest(self):
        h = houses.make_house(random.Random(1), "h", "train")
        good = {"cat": "hors_cadre", "user": "Raconte-moi une blague.", "answer": {"tool": "delegate", "arguments": {"question": "Raconte-moi une blague."}}}
        self.assertEqual(validate.validate(good, h, TOOLS), [])
        altered = {**good, "answer": {"tool": "delegate", "arguments": {"question": "Autre chose"}}}
        self.assertTrue(validate.validate(altered, h, TOOLS), "the question is copied, not rewritten")
        empty = {**good, "answer": {"tool": "delegate", "arguments": {"question": " "}}}
        self.assertTrue(validate.validate(empty, h, TOOLS))
        extra = {**good, "answer": {"tool": "delegate", "arguments": {"question": "Raconte-moi une blague.", "say": "x"}}}
        self.assertTrue(validate.validate(extra, h, TOOLS))

    def test_the_scorer_wants_a_delegation_with_some_question(self):
        record = {"messages": [{}, {}, {"role": "assistant", "content": "", "tool_calls": [{"type": "function", "function": {"name": "delegate", "arguments": {"question": "Raconte-moi une blague."}}}]}], "meta": {"cat": "hors_cadre"}}
        call = '<tool_call>\n{"name": "delegate", "arguments": {"question": "Raconte une blague"}}\n</tool_call>'
        self.assertTrue(score.score(record, call)["ok"])
        self.assertFalse(score.score(record, "Voilà une blague.")["ok"])
        self.assertFalse(score.score(record, '<tool_call>\n{"name": "delegate", "arguments": {}}\n</tool_call>')["ok"])


if __name__ == "__main__":
    unittest.main()
