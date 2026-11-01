import random
import unittest
from moli_model import houses


class HousesTest(unittest.TestCase):
    def test_a_house_is_well_formed(self):
        for seed in range(40):
            h = houses.make_house(random.Random(seed), f"h{seed}", "train")
            ids = [d["id"] for d in h["devices"]]
            self.assertEqual(len(ids), len(set(ids)), "ids are unique")
            rooms = {r["name"] for r in h["layout"]["rooms"]}
            self.assertTrue(set(h["protected"]) <= rooms, "protected rooms exist")
            for d in h["devices"]:
                self.assertTrue(d["native_room"] is None or d["native_room"] in rooms, "a room of the house, or none")
                self.assertTrue(d["points"])
                for p in d["points"]:
                    self.assertIn(p["key"], d["state"])

    def test_the_same_seed_gives_the_same_house(self):
        a = houses.make_house(random.Random(3), "h", "train")
        b = houses.make_house(random.Random(3), "h", "train")
        self.assertEqual(a, b)

    def test_unseen_families_and_rooms_are_for_the_test_split_only(self):
        for seed in range(60):
            train = houses.make_house(random.Random(seed), "t", "train")
            self.assertFalse({m["family"] for m in train["meta"].values()} & houses.TEST_ONLY_FAMILIES)
            self.assertFalse({r["name"] for r in train["layout"]["rooms"]} & houses.TEST_ONLY_ROOMS)
        seen = set()
        for seed in range(60):
            test = houses.make_house(random.Random(seed), "x", "test")
            seen |= {m["family"] for m in test["meta"].values()}
        self.assertTrue(seen & houses.TEST_ONLY_FAMILIES, "test houses do use the unseen families")

    def test_some_houses_have_the_same_name_in_two_rooms_and_some_unreachable_devices(self):
        twin = offline = 0
        for seed in range(80):
            h = houses.make_house(random.Random(seed), "h", "train")
            names = [m["name"] for m in h["meta"].values()]
            twin += len(names) != len(set(names))
            offline += any(d.get("online") is False for d in h["devices"])
        self.assertGreater(twin, 20)
        self.assertGreater(offline, 5)

    def test_a_house_can_have_no_protected_room(self):
        self.assertTrue(any(not houses.make_house(random.Random(s), "h", "train")["protected"] for s in range(60)))

    def test_ids_do_not_always_spell_the_device_name(self):
        ids = [d["id"] for s in range(20) for d in houses.make_house(random.Random(s), f"h{s}", "train")["devices"]]
        hexed = [i for i in ids if i.startswith("z2m:0x")]
        self.assertTrue(hexed and len(hexed) < len(ids))
        for s in range(20):
            h = houses.make_house(random.Random(s), f"h{s}", "train")
            self.assertEqual(len({d["id"] for d in h["devices"]}), len(h["devices"]))

    def test_some_houses_have_a_camera_and_a_device_without_a_room(self):
        cameras = loose = 0
        for s in range(80):
            h = houses.make_house(random.Random(s), f"h{s}", "train")
            cameras += any(d.get("camera") for d in h["devices"])
            loose += any(d.get("native_room") is None for d in h["devices"])
            for d in h["devices"]:
                if d.get("native_room") is None:
                    self.assertEqual(h["meta"][d["id"]]["room"], houses.NO_ROOM)
        self.assertGreater(cameras, 5)
        self.assertGreater(loose, 5)


if __name__ == "__main__":
    unittest.main()


class V4FamiliesTest(unittest.TestCase):
    def test_the_real_house_kinds_appear_and_the_purifier_only_in_test(self):
        seen, test_seen = set(), set()
        for i in range(300):
            for split, bucket in (("train", seen), ("test", test_seen)):
                h = houses.make_house(random.Random(i), f"h{i}", split)
                bucket.update(m["family"] for m in h["meta"].values())
        for family in ("light_color", "ac_z", "smoke", "motion", "person", "speaker", "tv", "vacuum", "lock", "printer", "helper"):
            self.assertIn(family, seen, family)
        self.assertNotIn("purifier", seen)
        self.assertIn("purifier", test_seen)

    def test_settings_and_noise_ride_along_as_on_real_devices(self):
        keys = set()
        for i in range(100):
            for d in houses.make_house(random.Random(i), f"h{i}", "train")["devices"]:
                keys.update(p["key"] for p in d["points"])
        self.assertIn("linkquality", keys)
        self.assertIn("power_on_behavior", keys)

    def test_people_have_no_room_and_no_position(self):
        for i in range(100):
            h = houses.make_house(random.Random(i), f"h{i}", "train")
            for d in houses.devices(h):
                if d["family"] == "person":
                    self.assertEqual(d["room"], houses.NO_ROOM)
                    self.assertFalse({"latitude", "longitude"} & {p["key"] for p in d["points"]})
