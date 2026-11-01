import random
import re
import unittest
from moli_model import houses, phrases


def house(seed=1, split="train"):
    return houses.make_house(random.Random(seed), f"h{seed}", split)


class PhrasesTest(unittest.TestCase):
    def test_every_category_appears_over_a_few_houses(self):
        seen = set()
        for s in range(30):
            for c in phrases.generate(random.Random(s), house(s), 40):
                seen.add(c["cat"])
        self.assertTrue({"commande", "lecture", "énergie", "historique", "protégée", "ambigu", "inexistant", "bavardage"} <= seen, seen)

    def test_a_command_targets_a_writable_point_of_the_house(self):
        n = 0
        for s in range(30):
            h = house(s)
            for c in phrases.generate(random.Random(s), h, 40):
                if c["cat"] != "commande":
                    continue
                n += 1
                a = c["answer"]
                self.assertEqual(a["tool"], "set")
                dev_id, key = a["arguments"]["point"].split("/")
                dev = next(d for d in houses.devices(h) if d["id"] == dev_id)
                self.assertTrue(houses.point_spec(dev, key)["access"]["write"])
                self.assertEqual(sorted(a["arguments"]), ["point", "value"], "no `say`: Moli words the outcome")
                self.assertNotIn(dev["room"], h["protected"], "never commands a protected room")
        self.assertGreater(n, 100)

    def test_a_brightness_request_uses_the_brightness_key_with_the_asked_number(self):
        found = 0
        for s in range(40):
            h = house(s)
            for c in phrases.generate(random.Random(s), h, 60):
                if c["cat"] == "commande" and c["answer"]["arguments"]["point"].endswith("/brightness"):
                    found += 1
                    self.assertIn(str(c["answer"]["arguments"]["value"]), c["user"])
        self.assertGreater(found, 10)

    def test_a_protected_room_gets_a_refusal_never_a_call(self):
        found = 0
        for s in range(40):
            h = house(s)
            for c in phrases.generate(random.Random(s), h, 60):
                if c["cat"] == "protégée":
                    found += 1
                    self.assertIn("text", c["answer"])
                    self.assertIn("protégée", c["answer"]["text"])
        self.assertGreater(found, 5)

    def test_an_ambiguous_request_gets_a_question_naming_the_choices(self):
        found = 0
        for s in range(60):
            h = house(s)
            for c in phrases.generate(random.Random(s), h, 60):
                if c["cat"] == "ambigu":
                    found += 1
                    self.assertTrue(c["answer"]["text"].endswith("?"))
                    self.assertIn("ou", c["answer"]["text"])
        self.assertGreater(found, 10)

    def test_energy_and_history_do_not_force_the_optional_arguments(self):
        steps, plain, hist_without_hours = set(), 0, 0
        for s in range(40):
            h = house(s)
            for c in phrases.generate(random.Random(s), h, 60):
                if c["cat"] == "énergie":
                    args = c["answer"]["arguments"]
                    plain += not args
                    steps |= {args.get("step")}
                if c["cat"] == "historique" and "hours" not in c["answer"]["arguments"]:
                    hist_without_hours += 1
        self.assertGreater(plain, 0)
        self.assertTrue(steps - {None})
        self.assertGreater(hist_without_hours, 0)

    def test_held_out_phrasings_only_appear_in_test_houses(self):
        train, test = set(), set()
        for s in range(40):
            for split, bucket in (("train", train), ("test", test)):
                h = house(s, split)
                bucket |= {c["user"] for c in phrases.generate(random.Random(s), h, 60)}
        self.assertTrue(test - train, "test houses are asked things training never saw")

    def test_the_same_seed_gives_the_same_cases(self):
        a = phrases.generate(random.Random(5), house(5), 30)
        b = phrases.generate(random.Random(5), house(5), 30)
        self.assertEqual(a, b)

    def test_held_out_templates_stay_out_of_train_and_dev_whatever_the_category(self):
        for s in range(60):
            for split in ("train", "dev"):
                for c in phrases.generate(random.Random(s), house(s, split), 80):
                    self.assertNotIn("l'allumer", c["user"], c)
                    self.assertFalse(c["user"].startswith("Démarre"), c)
                    self.assertNotIn("Remonte", c["user"], c)

    def test_a_shutter_or_a_clim_is_never_asked_to_light_up(self):
        for s in range(60):
            for c in phrases.generate(random.Random(s), house(s), 80):
                u = c["user"].lower()
                if " volet" in u or " store" in u:
                    self.assertIsNone(re.search(r"\b(allume|allumer|active|mets en marche|éteins)\b", u), c)

    def test_a_feminine_ambiguity_says_laquelle(self):
        seen = 0
        for s in range(80):
            for c in phrases.generate(random.Random(s), house(s), 80):
                if c["cat"] == "ambigu":
                    fem = any(w in c["user"] for w in (" la ", "l'"))
                    seen += fem
                    self.assertTrue(c["answer"]["text"].startswith("Laquelle" if " la " in c["user"] else "Lequel"), c)
        self.assertGreater(seen, 0)

    def test_the_fixed_chatter_is_rare_so_it_does_not_drown_the_rest(self):
        total = fixed = 0
        for s in range(80):
            for c in phrases.generate(random.Random(s), house(s), 80):
                total += 1
                fixed += c["cat"] in ("bavardage", "énergie")
        self.assertLess(fixed / total, 0.12)

    def test_a_reading_is_a_get_device_call_on_the_device_asked_about(self):
        found = 0
        for s in range(60):
            h = house(s)
            ids = {d["id"] for d in houses.devices(h)}
            for c in phrases.generate(random.Random(s), h, 80):
                if c["cat"] == "lecture":
                    found += 1
                    self.assertEqual(c["answer"]["tool"], "get_device", c)
                    self.assertEqual(list(c["answer"]["arguments"]), ["device"])
                    self.assertIn(c["answer"]["arguments"]["device"], ids)
        self.assertGreater(found, 100)

    def test_the_model_is_never_taught_to_refuse_an_unreachable_device(self):
        # It cannot know (the prompt has no reachability) and Moli says so itself.
        for s in range(80):
            for c in phrases.generate(random.Random(s), house(s), 80):
                if c["cat"] == "inexistant":
                    self.assertNotIn("injoignable", c["answer"]["text"], c)

    def test_commands_may_target_an_unreachable_device(self):
        seen = 0
        for s in range(80):
            h = house(s)
            offline = {d["id"] for d in houses.devices(h) if d.get("online") is False}
            for c in phrases.generate(random.Random(s), h, 200):
                if c["cat"] == "commande" and c["answer"]["arguments"]["point"].split("/")[0] in offline:
                    seen += 1
        self.assertGreater(seen, 0)

    def test_no_request_has_a_broken_contraction(self):
        for s in range(80):
            for split in ("train", "test"):
                for c in phrases.generate(random.Random(s), house(s, split), 80):
                    padded = f" {c['user']} "
                    for bad in (" de le ", " de les ", " à le ", " à les "):
                        self.assertNotIn(bad, padded, c["user"])

    def test_a_device_without_a_room_is_named_without_one_and_only_when_its_name_is_unique(self):
        seen = 0
        for s in range(120):
            h = house(s)
            devs = houses.devices(h)
            loose = [d for d in devs if d["room"] == houses.NO_ROOM]
            for c in phrases.generate(random.Random(s), h, 200):
                self.assertNotIn("sans pièce", c["user"].lower(), c)
                if c["cat"] == "commande":
                    dev_id = c["answer"]["arguments"]["point"].split("/")[0]
                    if any(d["id"] == dev_id for d in loose):
                        seen += 1
                        name = next(d["name"] for d in loose if d["id"] == dev_id)
                        self.assertEqual(sum(d["name"] == name for d in devs), 1, c)
        self.assertGreater(seen, 0)


if __name__ == "__main__":
    unittest.main()
