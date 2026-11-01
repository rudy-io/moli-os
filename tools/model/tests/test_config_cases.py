import random
import unittest
from moli_model import config_cases, houses


def cases(seed, split="train"):
    h = houses.make_house(random.Random(seed), f"h{seed}", split)
    return h, config_cases.generate(random.Random(seed), h)


class ConfigCasesTest(unittest.TestCase):
    def test_every_configuration_tool_is_covered(self):
        tools = set()
        for s in range(40):
            tools |= {c["answer"]["tool"] for c in cases(s)[1] if c["cat"] == "configuration"}
        self.assertEqual(tools, {"rename_device", "move_device", "set_device_enabled", "rename_room", "add_person",
                                 "set_home_location", "refresh_weather"})

    def test_a_device_or_room_argument_exists_in_the_house(self):
        for s in range(40):
            h, cs = cases(s)
            ids = {d["id"] for d in h["devices"]}
            rooms = {r["name"] for r in h["layout"]["rooms"]}
            for c in cs:
                a = c["answer"].get("arguments", {})
                if "device" in a:
                    self.assertIn(a["device"], ids)
                if c["answer"].get("tool") == "rename_room":
                    self.assertIn(a["room"], rooms)
                if c["answer"].get("tool") == "move_device":
                    self.assertIn(a["room"], rooms)

    def test_a_driver_request_is_forwarded_not_executed(self):
        seen = [c for s in range(20) for c in cases(s)[1] if c["cat"] == "pilote"]
        self.assertTrue(seen)
        for c in seen:
            self.assertEqual(c["answer"]["tool"], "request_driver")
            self.assertTrue(c["answer"]["arguments"]["description"].strip())

    def test_a_home_location_is_a_city_never_an_address(self):
        places = {c["answer"]["arguments"]["place"] for s in range(40) for c in cases(s)[1] if c["answer"].get("tool") == "set_home_location"}
        self.assertTrue(places)
        for p in places:
            self.assertNotRegex(p, r"\d", "no street number, no postcode")

    def test_people_are_fictional_first_names(self):
        names = {c["answer"]["arguments"]["name"] for s in range(40) for c in cases(s)[1] if c["answer"].get("tool") == "add_person"}
        self.assertTrue(names)
        self.assertTrue(names <= set(config_cases.FIRST_NAMES))

    def test_a_new_device_name_fits_the_kind_of_device(self):
        for s in range(60):
            h, cs = cases(s)
            fam = {d["id"]: d["family"] for d in houses.devices(h)}
            for c in cs:
                if c["answer"].get("tool") == "rename_device":
                    a = c["answer"]["arguments"]
                    if fam[a["device"]] in ("shutter", "ac", "sensor", "door", "plug", "camera"):
                        self.assertNotIn(a["new_name"], config_cases.NEW_NAMES["light"], c)

    def test_asking_to_undo_is_an_undo_call_with_nothing_in_it(self):
        found = [c for s in range(60) for c in cases(s)[1] if c["cat"] == "annulation"]
        self.assertGreater(len(found), 15)
        for c in found:
            self.assertEqual(c["answer"], {"tool": "undo", "arguments": {}}, c)

    def test_the_held_out_ways_to_ask_for_an_undo_stay_in_test_houses(self):
        train = {c["base"] for s in range(120) for c in cases(s)[1] if c["cat"] == "annulation"}
        test = {c["base"] for s in range(120) for c in cases(s, "test")[1] if c["cat"] == "annulation"}
        self.assertFalse(train & config_cases.UNDO_HELD_OUT)
        self.assertTrue(test & config_cases.UNDO_HELD_OUT)

    def test_an_undo_never_asks_for_something_else_to_be_changed(self):
        for s in range(60):
            for c in cases(s)[1]:
                if c["cat"] == "annulation":
                    self.assertNotRegex(c["user"], r"(?i)\b(renomme|déplace|masque|allume|éteins|mets)\b")


if __name__ == "__main__":
    unittest.main()
