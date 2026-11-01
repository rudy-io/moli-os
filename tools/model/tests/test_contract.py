import unittest
from moli_model import contract


class ContractTest(unittest.TestCase):
    def test_the_frozen_tools_load(self):
        c = contract.load()
        self.assertEqual(c["version"], 4)
        names = [t["function"]["name"] for t in c["tools"]]
        self.assertEqual(len(names), 25)
        self.assertEqual(names[:3], ["set", "set_all", "adjust"])
        for name in ("find", "problems", "now", "last_change", "weather", "ambiance", "schedule", "remind", "draft_automation"):
            self.assertIn(name, names)
        self.assertEqual(names[-1], "delegate")

    def test_an_order_is_a_point_and_a_value_nothing_to_say(self):
        tools = {t["function"]["name"]: t["function"]["parameters"] for t in contract.load()["tools"]}
        self.assertEqual(sorted(tools["set"]["properties"]), ["point", "value"])
        self.assertEqual(tools["set"]["required"], ["point", "value"])

    def test_undo_takes_nothing_the_last_confirmed_change_is_Molis_to_know(self):
        tools = {t["function"]["name"]: t["function"]["parameters"] for t in contract.load()["tools"]}
        self.assertEqual(tools["undo"]["properties"], {})
        self.assertNotIn("required", tools["undo"])

    def test_energy_and_history_keep_their_optional_parameters(self):
        tools = {t["function"]["name"]: t["function"]["parameters"] for t in contract.load()["tools"]}
        self.assertNotIn("required", tools["energy"])
        self.assertEqual(tools["history"]["required"], ["point"])


if __name__ == "__main__":
    unittest.main()
