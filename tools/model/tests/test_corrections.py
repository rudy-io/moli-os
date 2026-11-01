import unittest

from moli_model import contract, corrections, score

PROMPT = "Contrat Moli v4. (test)"
ALIASES = {"d1": "fake:lamp", "d2": "fake:shutter"}
TOOLS = contract.load()["tools"]


def line(question, *calls, context=()):
    calls = [{"tool": t, "arguments": a} for t, a in calls]
    return {"kind": "correct", "at": 1, "question": question, "context": list(context),
            "choice": calls[0], "choices": calls, "remembered": True, "by": "Personne"}


class CorrectionsTest(unittest.TestCase):
    def test_a_correction_becomes_a_record_in_device_numbers_without_who_or_when(self):
        out, skipped = corrections.records(
            [line("Allume la lampe", ("set", {"point": "fake:lamp/on", "value": True}))], PROMPT, ALIASES, TOOLS)
        self.assertEqual(skipped, 0)
        record = out[0]
        self.assertEqual(record["messages"][0], {"role": "system", "content": PROMPT})
        self.assertEqual(record["messages"][-2], {"role": "user", "content": "Allume la lampe"})
        call = record["messages"][-1]["tool_calls"][0]["function"]
        self.assertEqual(call, {"name": "set", "arguments": {"point": "d1/on", "value": True}})
        self.assertNotIn("Personne", str(record))
        self.assertEqual(record["meta"]["cat"], "reel")
        # The scorer reads it like the synthetic records.
        output = '<tool_call>{"name": "set", "arguments": {"point": "d1/on", "value": true}}</tool_call>'
        self.assertTrue(score.score(record, output)["ok"])

    def test_several_calls_nothing_and_the_dialogue_before(self):
        lines = [
            line("Ferme tout", ("set", {"point": "fake:lamp/on", "value": False}),
                 ("set", {"point": "fake:shutter/control", "value": "close"})),
            line("Éteins-la", ("nothing", {}), context=[["user", "Allume la lampe"], ["assistant", "C'est fait."]]),
        ]
        out, _ = corrections.records(lines, PROMPT, ALIASES, TOOLS)
        self.assertEqual(len(out[0]["messages"][-1]["tool_calls"]), 2)
        nothing = out[1]
        self.assertEqual(nothing["messages"][-1], {"role": "assistant", "content": corrections.NOTHING})
        self.assertEqual([m["content"] for m in nothing["messages"][1:3]], ["Allume la lampe", "C'est fait."])
        # Acting where nothing was due is what the scorer calls unsafe.
        acting = '<tool_call>{"name": "set", "arguments": {"point": "d1/on", "value": false}}</tool_call>'
        self.assertTrue(score.score(nothing, acting)["unsafe"])

    def test_a_device_the_house_no_longer_lists_is_skipped_and_other_lines_are_left(self):
        lines = [
            line("Allume le ruban", ("set", {"point": "fake:gone/on", "value": True})),
            {"kind": "undo", "put_back": 1},
            line("Quelle heure est-il ?", ("now", {})),
        ]
        out, skipped = corrections.records(lines, PROMPT, ALIASES, TOOLS)
        self.assertEqual(skipped, 1)
        self.assertEqual([r["messages"][-1]["tool_calls"][0]["function"]["name"] for r in out], ["now"])

    def test_the_house_is_what_get_api_devices_and_home_json_say(self):
        snapshot = {"devices": [{"id": "fake:lamp"}], "guard": {"protected_rooms": ["Chambre 1"]}, "drivers": []}
        house = corrections.house_of(snapshot, {"rooms": [{"name": "Salon"}]})
        self.assertEqual(house["protected"], ["Chambre 1"])
        self.assertEqual(house["devices"], [{"id": "fake:lamp"}])
        self.assertEqual(house["layout"], {"rooms": [{"name": "Salon"}]})


if __name__ == "__main__":
    unittest.main()
