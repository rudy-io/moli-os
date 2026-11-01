import random
import unittest
from moli_model import contract, houses, leak, phrases, validate

TOOLS = contract.load()["tools"]


def house(seed=2):
    return houses.make_house(random.Random(seed), "h", "train")


def a_command(h):
    for c in phrases.generate(random.Random(2), h, 80):
        if c["cat"] == "commande":
            return c
    raise AssertionError("no command")


class ValidateTest(unittest.TestCase):
    def test_generated_cases_are_valid(self):
        for s in range(25):
            h = house(s)
            for c in phrases.generate(random.Random(s), h, 60):
                self.assertEqual(validate.validate(c, h, TOOLS), [], c)

    def test_an_unknown_point_is_rejected(self):
        h = house(); c = a_command(h)
        c["answer"]["arguments"]["point"] = "z2m:inconnu_1/state"
        self.assertTrue(any("inconnu" in p or "unknown" in p for p in validate.validate(c, h, TOOLS)))

    def test_a_value_out_of_bounds_or_of_the_wrong_type_is_rejected(self):
        h = house()
        for c in phrases.generate(random.Random(1), h, 200):
            if c["cat"] == "commande" and c["answer"]["arguments"]["point"].endswith("/brightness"):
                c["answer"]["arguments"]["value"] = 250
                self.assertTrue(validate.validate(c, h, TOOLS))
                c["answer"]["arguments"]["value"] = "beaucoup"
                self.assertTrue(validate.validate(c, h, TOOLS))
                return
        self.skipTest("no brightness command in this house")

    def test_an_enum_value_must_be_one_of_the_choices(self):
        for s in range(30):
            h = house(s)
            for c in phrases.generate(random.Random(s), h, 200):
                if c["cat"] == "commande" and c["answer"]["arguments"]["point"].endswith("/control"):
                    c["answer"]["arguments"]["value"] = "moitié"
                    self.assertTrue(validate.validate(c, h, TOOLS))
                    return
        self.skipTest("no shutter command")

    def test_a_say_or_an_unknown_tool_is_rejected(self):
        h = house(); c = a_command(h)
        c["answer"]["arguments"]["say"] = "Allumé."  # the contract has no `say`: Moli words the outcome
        self.assertTrue(validate.validate(c, h, TOOLS))
        c = a_command(h); c["answer"]["tool"] = "teleport"
        self.assertTrue(validate.validate(c, h, TOOLS))

    def test_commanding_a_protected_room_is_rejected(self):
        for s in range(60):
            h = house(s)
            if not h["protected"]:
                continue
            dev = next((d for d in houses.devices(h) if d["room"] in h["protected"] and any(p["access"]["write"] for p in d["points"])), None)
            if dev:
                key = next(p["key"] for p in dev["points"] if p["access"]["write"])
                c = {"cat": "commande", "user": "x", "answer": {"tool": "set", "arguments": {"point": f"{dev['id']}/{key}", "value": True}}}
                self.assertTrue(any("protég" in p for p in validate.validate(c, h, TOOLS)))
                return
        self.skipTest("no protected house")

    def test_an_unreachable_device_is_commanded_like_any_other(self):
        # The prompt no longer says who is reachable: the model cannot know, Moli does.
        for s in range(60):
            h = house(s)
            dev = next((d for d in houses.devices(h) if d.get("online") is False and d["room"] not in h["protected"]
                        and any(p["access"]["write"] for p in d["points"])), None)
            if dev:
                key = next(p["key"] for p in dev["points"] if p["access"]["write"])
                value = True if key in ("state", "on") else None
                if value is None:
                    continue
                c = {"cat": "commande", "user": "x", "answer": {"tool": "set", "arguments": {"point": f"{dev['id']}/{key}", "value": value}}}
                self.assertEqual(validate.validate(c, h, TOOLS), [])
                return
        self.skipTest("no unreachable device")

    def test_the_leak_scan_catches_addresses_macs_and_emails(self):
        self.assertTrue(leak.scan("rendez-vous sur 10.0.0.2"))
        self.assertTrue(leak.scan("mac aa:bb:cc:dd:ee:ff"))
        self.assertTrue(leak.scan("écris à moi@exemple.fr"))
        self.assertFalse(leak.scan("Allume le plafonnier du salon à 30 %."))


if __name__ == "__main__":
    unittest.main()


def first(h, family):
    return next(d for d in houses.devices(h) if d["family"] == family)


def with_family(family):
    for s in range(200):
        h = house(s)
        if any(m["family"] == family for m in h["meta"].values()):
            return h
    raise AssertionError(family)


class V4ValidateTest(unittest.TestCase):
    def case(self, tool, cat="v4", **args):
        return {"cat": cat, "user": "x", "answer": {"tool": tool, "arguments": args}}

    def test_each_call_of_several_is_checked(self):
        h = house()
        good = a_command(h)["answer"]
        bad = {"tool": "set", "arguments": {"point": "z2m:inconnu/state", "value": True}}
        c = {"cat": "multi", "user": "x", "answer": {"calls": [good, bad]}}
        self.assertTrue(validate.validate(c, h, TOOLS))
        c["answer"]["calls"] = [good, good]
        self.assertEqual(validate.validate(c, h, TOOLS), [])

    def test_set_all_takes_a_known_kind_and_room(self):
        h = house()
        room = h["layout"]["rooms"][0]["name"]
        self.assertEqual(validate.validate(self.case("set_all", kind="lumières", room=room, value=False), h, TOOLS), [])
        self.assertTrue(validate.validate(self.case("set_all", kind="fusées", value=False), h, TOOLS))
        self.assertTrue(validate.validate(self.case("set_all", kind="volets", room="Lune", value="close"), h, TOOLS))

    def test_adjust_moves_a_writable_number_only(self):
        h = with_family("light_dim")
        d = first(h, "light_dim")
        self.assertEqual(validate.validate(self.case("adjust", point=f"{d['id']}/brightness", by=-20), h, TOOLS), [])
        self.assertTrue(validate.validate(self.case("adjust", point=f"{d['id']}/state", by=1), h, TOOLS))

    def test_a_colour_is_said_by_its_name_on_a_colour_point(self):
        h = with_family("light_color")
        d = first(h, "light_color")
        ok = {"cat": "couleur", "user": "x", "answer": {"tool": "set", "arguments": {"point": f"{d['id']}/color", "value": "rouge"}}}
        self.assertEqual(validate.validate(ok, h, TOOLS), [])
        ok["answer"]["arguments"]["value"] = "#ff0000"
        self.assertTrue(validate.validate(ok, h, TOOLS), "the model never writes hex")

    def test_planned_orders_and_reminders_are_bounded(self):
        h = house()
        command = next(c for c in phrases.generate(random.Random(2), h, 80)
                       if c["cat"] == "commande" and isinstance(c["answer"]["arguments"]["value"], bool))
        point = command["answer"]["arguments"]["point"]
        self.assertEqual(validate.validate(self.case("schedule", minutes=10, point=point, value=True), h, TOOLS), [])
        self.assertTrue(validate.validate(self.case("schedule", minutes=0, point=point, value=True), h, TOOLS))
        self.assertTrue(validate.validate(self.case("remind", minutes=10, text=""), h, TOOLS))
        self.assertEqual(validate.validate(self.case("remind", minutes=10, text="Sortir le linge"), h, TOOLS), [])

    def test_find_and_weather_take_their_enums(self):
        h = house()
        self.assertEqual(validate.validate(self.case("find", state="allumé", kind="lumières"), h, TOOLS), [])
        self.assertTrue(validate.validate(self.case("find", state="joyeux"), h, TOOLS))
        self.assertEqual(validate.validate(self.case("weather", day=1), h, TOOLS), [])
        self.assertTrue(validate.validate(self.case("weather", day=9), h, TOOLS))
