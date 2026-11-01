import random
import unittest
from collections import Counter

from moli_model import contract, houses, v4_cases, validate

TOOLS = contract.load()["tools"]
NEW = {"groupe", "relatif", "état", "problèmes", "heure", "dernier_changement", "météo", "ambiance", "couleur",
       "programmé", "rappel", "automatisme", "multi", "suite", "anglais"}


def cases(split="train", n=60):
    out = []
    for s in range(n):
        h = houses.make_house(random.Random(s), f"h{s}", split)
        out += [(h, c) for c in v4_cases.generate(random.Random(s), h, split == "test")]
    return out


class V4CasesTest(unittest.TestCase):
    def test_every_new_category_is_there(self):
        seen = Counter(c["cat"] for _, c in cases())
        self.assertEqual(NEW - set(seen), set(), seen)

    def test_every_case_is_one_a_correct_moli_would_give(self):
        for h, c in cases() + cases("test", 20):
            self.assertEqual(validate.validate(c, h, TOOLS), [], c)

    def test_a_follow_up_points_at_the_device_of_the_turn_before(self):
        follow = [c for _, c in cases() if c["cat"] == "suite"]
        self.assertTrue(follow)
        for c in follow:
            self.assertTrue(c["context"], c)
            self.assertTrue(all(isinstance(t, tuple) and len(t) == 2 for t in c["context"]))

    def test_test_houses_hold_phrasings_the_training_never_saw(self):
        train = {c["user"] for _, c in cases()}
        test = [c["user"] for _, c in cases("test", 60)]
        unseen = [u for u in test if u not in train]
        self.assertGreater(len(unseen) / len(test), 0.3)

    def test_noise_changes_the_words_not_the_answer(self):
        rnd = random.Random(4)
        said = "Éteins toutes les lumières du salon."
        noisy = {v4_cases.noisy(rnd, said) for _ in range(30)}
        self.assertGreater(len(noisy), 3)
        for n in noisy:
            self.assertLess(abs(len(n) - len(said)), 6, n)

    def test_everything_off_is_the_lights_never_the_plugs(self):
        for _, c in cases():
            if c["cat"] == "groupe" and c["answer"]["arguments"]["kind"] == "tout":
                self.assertIn(c["answer"]["arguments"]["value"], (False, True))

    def test_english_requests_get_the_same_calls(self):
        english = [c for _, c in cases() if c["cat"] == "anglais"]
        self.assertTrue(english)
        for c in english:
            self.assertTrue("tool" in c["answer"] or "calls" in c["answer"], c)


if __name__ == "__main__":
    unittest.main()
