"""Random fictional houses, in the format of `moli-os model prompt` (Rust `HouseIn`)."""
import random
import re
import unicodedata

ROOM_POOL = ["Salon", "Cuisine", "Salle à manger", "Chambre 1", "Chambre 2", "Chambre des enfants",
             "Bureau", "Entrée", "Couloir", "Salle de bain", "Garage", "Jardin", "Escaliers", "Véranda", "Terrasse",
             "Dressing", "Chambre 3", "Salle d'eau", "Mezzanine", "Grenier", "Vestibule"]
TEST_ONLY_ROOMS = {"Buanderie", "Cave"}
BEDROOMS = ["Chambre 1", "Chambre 2", "Chambre des enfants", "Chambre 3"]
DE_ROOM = {"Salon": "du salon", "Cuisine": "de la cuisine", "Salle à manger": "de la salle à manger",
           "Chambre 1": "de la chambre 1", "Chambre 2": "de la chambre 2",
           "Chambre des enfants": "de la chambre des enfants", "Bureau": "du bureau", "Entrée": "de l'entrée",
           "Couloir": "du couloir", "Salle de bain": "de la salle de bain", "Garage": "du garage",
           "Jardin": "du jardin", "Buanderie": "de la buanderie", "Cave": "de la cave", "Escaliers": "des escaliers",
           "Véranda": "de la véranda", "Terrasse": "de la terrasse", "Dressing": "du dressing", "Chambre 3": "de la chambre 3",
           "Salle d'eau": "de la salle d'eau", "Mezzanine": "de la mezzanine", "Grenier": "du grenier", "Vestibule": "du vestibule"}
# device name -> article used in a request
ART = {"Plafonnier": "le", "Lampe": "la", "Applique": "l'", "Spot": "le", "Lampadaire": "le", "Suspension": "la",
       "Ruban LED": "le", "Volet": "le", "Store": "le", "Climatisation": "la clim", "Prise": "la",
       "Machine à café": "la", "Bouilloire": "la", "Aspirateur": "l'", "Capteur température": "le",
       "Porte d'entrée": "la", "Fenêtre": "la", "Porte de garage": "la", "Ventilateur": "le", "Caméra": "la",
       "Ampoule": "l'", "Ruban LED couleur": "le", "Détecteur de fumée": "le", "Détecteur de CO": "le",
       "Détecteur de mouvement": "le", "Enceinte": "l'", "Télévision": "la", "Aspirateur robot": "l'",
       "Serrure": "la", "Imprimante 3D": "l'", "Purificateur": "le", "Bouton": "le",
       "Alarme": "l'", "Mode vacances": "le", "Mode nuit": "le", "Mode invités": "le"}
# Names after « l' » that are feminine (« éteins-la »).
FEMININE = {"Ampoule", "Applique", "Enceinte", "Imprimante 3D", "Alarme"}
TEST_ONLY_FAMILIES = {"fan", "purifier"}
# Fictional people (the phones' presence): never a real household's names.
PEOPLE = ["Alix", "Noé", "Maëlle", "Timéo", "Inès", "Sacha", "Elio", "Romy", "Lou", "Basile"]
HELPERS = ["Mode vacances", "Alarme", "Mode nuit", "Mode invités"]
NO_ROOM = "Sans pièce"
LOOSE_FAMILY = {"Lampe": "light", "Prise": "plug", "Capteur température": "sensor"}


def _spec(key, kind, write, unit=None, **kind_args):
    spec = {"key": key, "label": key, "kind": {"type": kind, **kind_args},
            "access": {"read": True, "write": write}, "semantic": "other"}
    if unit:
        spec["unit"] = unit
    return spec


def BINARY(key, write=True):
    return _spec(key, "binary", write)


def PERCENT(key):
    return _spec(key, "numeric", True, "%", min=0.0, max=100.0)


def ENUM(key, values):
    return _spec(key, "enum", True, values=values)


def READ(key, unit):
    return _spec(key, "numeric", False, unit)


def TEXT_READ(key):
    return _spec(key, "text", False)


MODES = ["auto", "déshumidification", "froid", "chaud", "ventilation"]


def CONFIG(key, values):
    spec = ENUM(key, values)
    spec["semantic"] = "config"
    return spec


def TEXT_WRITE(key, label):
    spec = _spec(key, "text", True)
    spec["label"] = label
    return spec

# family -> (names, builder(rnd) -> [(spec, value)])
FAMILIES = {
    "light": (["Plafonnier", "Lampe", "Applique", "Spot", "Lampadaire", "Suspension"],
              lambda r: [(BINARY("state"), r.random() < 0.4)]),
    "light_dim": (["Plafonnier", "Lampe", "Suspension", "Lampadaire"],
                  lambda r: [(BINARY("state"), r.random() < 0.4), (PERCENT("brightness"), r.choice([10, 30, 50, 80, 100]))]),
    "strip": (["Ruban LED"],
              lambda r: [(BINARY("state"), r.random() < 0.4), (PERCENT("brightness"), r.choice([20, 60, 100]))]),
    "shutter": (["Volet", "Store"],
                lambda r: [(ENUM("control", ["open", "stop", "close"]), r.choice(["open", "close"])),
                           (PERCENT("percent_control"), r.choice([0, 50, 100]))]),
    "ac": (["Climatisation"],
           lambda r: [(BINARY("on"), r.random() < 0.3), (ENUM("mode", MODES), r.choice(MODES)),
                      (_spec("target_temperature", "numeric", True, "°C", min=16.0, max=30.0, step=0.5), r.choice([21, 22, 24, 26])),
                      (READ("temperature", "°C"), round(r.uniform(19, 27), 1))]),
    "plug": (["Prise", "Machine à café", "Bouilloire", "Aspirateur"],
             lambda r: [(BINARY("state"), r.random() < 0.5), (READ("power", "W"), r.choice([0, 0, 12, 35, 120]))]),
    "sensor": (["Capteur température"],
               lambda r: [(READ("temperature", "°C"), round(r.uniform(17, 26), 1)), (READ("humidity", "%"), r.choice([45, 55, 63, 70]))]),
    "door": (["Porte d'entrée", "Fenêtre", "Porte de garage"],
             lambda r: [(TEXT_READ("contact"), r.choice(["fermée", "ouverte"]))]),
    "camera": (["Caméra"], lambda r: [(_spec("motion", "binary", False), r.random() < 0.2)]),
    "light_color": (["Ampoule", "Ruban LED couleur", "Lampe", "Suspension"],
                    lambda r: [(BINARY("state"), r.random() < 0.4), (PERCENT("brightness"), r.choice([20, 50, 100])),
                               (_spec("color", "text", True), "#ffa758"),
                               (_spec("color_temp", "numeric", True, "mired", min=150.0, max=500.0), 370),
                               (ENUM("effect", ["off", "breathing", "candlelight", "fading"]), "off")]),
    "ac_z": (["Climatisation"],
             lambda r: [(ENUM("system_mode", ["off", "heat", "cool", "auto"]), r.choice(["off", "heat"])),
                        (_spec("occupied_heating_setpoint", "numeric", True, "°C", min=5.0, max=30.0, step=0.5), 20),
                        (ENUM("fan_mode", ["auto", "low", "medium", "high"]), "auto"),
                        (READ("local_temperature", "°C"), round(r.uniform(17, 24), 1))]),
    "smoke": (["Détecteur de fumée"], lambda r: [(BINARY("smoke", write=False), False), (READ("battery", "%"), r.choice([15, 60, 90]))]),
    "co": (["Détecteur de CO"], lambda r: [(BINARY("carbon_monoxide", write=False), False)]),
    "motion": (["Détecteur de mouvement"], lambda r: [(BINARY("occupancy", write=False), r.random() < 0.3),
                                                     (READ("illuminance", "lx"), r.choice([3, 40, 300])), (READ("battery", "%"), 80)]),
    "button": (["Bouton"], lambda r: [(TEXT_READ("action"), "single"), (READ("battery", "%"), 70)]),
    "speaker": (["Enceinte"], lambda r: [(PERCENT("volume"), r.choice([10, 25, 40])), (TEXT_WRITE("announce", "Annonce"), "")]),
    "tv": (["Télévision"], lambda r: [(BINARY("state"), r.random() < 0.3), (PERCENT("volume"), 20),
                                       (ENUM("source", ["TV", "HDMI 1", "HDMI 2"]), "TV")]),
    "vacuum": (["Aspirateur robot"], lambda r: [(ENUM("command", ["start", "pause", "return"]), "return"),
                                               (READ("battery", "%"), r.choice([30, 100]))]),
    "lock": (["Serrure"], lambda r: [(ENUM("lock", ["lock", "unlock"]), "lock")]),
    "printer": (["Imprimante 3D"], lambda r: [(READ("progress", "%"), r.choice([0, 42, 100])), (TEXT_READ("status"), "printing")]),
    "helper": (HELPERS, lambda r: [(BINARY("state"), r.random() < 0.3)]),
    # only in test houses: device kinds the training never showed
    "purifier": (["Purificateur"], lambda r: [(BINARY("state"), r.random() < 0.3), (ENUM("mode", ["auto", "nuit", "turbo"]), "auto")]),
    "fan": (["Ventilateur"],
            lambda r: [(BINARY("state"), r.random() < 0.3), (ENUM("speed", ["low", "medium", "high"]), r.choice(["low", "medium", "high"]))]),
}
WEIGHTS = {"light": 4, "light_dim": 4, "strip": 1, "shutter": 3, "ac": 2, "plug": 3, "sensor": 2, "door": 1, "camera": 1,
           "light_color": 3, "ac_z": 1, "smoke": 1, "co": 0.3, "motion": 1, "button": 0.5, "speaker": 1, "tv": 0.7,
           "vacuum": 0.4, "lock": 0.4, "printer": 0.3, "helper": 0.5}
# Real devices carry settings and radio noise: Moli leaves them out of the prompt.
NOISE = [(_spec("linkquality", "numeric", False, None), 120)]
SETTINGS = [(CONFIG("power_on_behavior", ["off", "on", "previous"]), "previous")]


def _slug(text):
    text = unicodedata.normalize("NFD", text.lower())
    text = "".join(c for c in text if unicodedata.category(c) != "Mn")
    return re.sub(r"[^a-z0-9]+", "_", text).strip("_")


def make_house(rnd, house_id, split):
    """A fictional house; `split` is train, dev or test (test houses may use unseen
    families and rooms). Deterministic for a given `rnd`."""
    pool = ROOM_POOL + (sorted(TEST_ONLY_ROOMS) if split == "test" else [])
    big = rnd.random() < 0.35  # a real house runs to 40 devices: the long prompts must be seen too
    rooms = rnd.sample(pool, rnd.randint(9, 14) if big else rnd.randint(4, 8))
    bedrooms = [r for r in rooms if r in BEDROOMS]
    protected = rnd.sample(bedrooms, rnd.randint(0, min(2, len(bedrooms))))
    families = list(WEIGHTS)
    devices, meta, used = [], {}, set()
    for room in rooms:
        for _ in range(rnd.randint(2, 6) if big else rnd.randint(2, 4)):
            unseen = sorted(TEST_ONLY_FAMILIES)
            family = rnd.choice(unseen) if split == "test" and rnd.random() < 0.15 else rnd.choices(families, [WEIGHTS[f] for f in families])[0]
            names, build = FAMILIES[family]
            name = rnd.choice(names)
            base = f"{_slug(name)}_{_slug(room)}"
            hexed = rnd.random() < 0.4  # real ids are often IEEE addresses: the answer must copy them from the prompt
            n = 1
            did = f"z2m:0x00158d00{rnd.getrandbits(32):08x}" if hexed else f"z2m:{base}_{n}"
            while did in used:
                n += 1
                did = f"z2m:0x00158d00{rnd.getrandbits(32):08x}" if hexed else f"z2m:{base}_{n}"
            used.add(did)
            points = build(rnd)
            if family not in ("helper", "person") and rnd.random() < 0.5:
                points = points + NOISE + (SETTINGS if any(p["access"]["write"] for p, _ in points) else [])
            device = {"id": did, "instance": "z2m", "native_name": name, "native_room": room,
                      "points": [p for p, _ in points],
                      "state": {p["key"]: {"value": v, "ts": 1} for p, v in points}}
            if rnd.random() < 0.12 and family not in ("sensor", "door"):
                device["online"] = False
            if family == "camera":
                device["camera"] = True
            devices.append(device)
            meta[did] = {"family": family, "room": room, "name": name}
    if rnd.random() < 0.4:  # a device nobody put in a room: « Sans pièce » in the prompt
        name = rnd.choice(list(LOOSE_FAMILY))
        family = LOOSE_FAMILY[name]
        did = f"z2m:{_slug(name)}_sans_piece_1"
        points = FAMILIES[family][1](rnd)
        devices.append({"id": did, "instance": "z2m", "native_name": name, "native_room": None,
                        "points": [p for p, _ in points],
                        "state": {p["key"]: {"value": v, "ts": 1} for p, v in points}})
        meta[did] = {"family": family, "room": NO_ROOM, "name": name}
    for name in rnd.sample(PEOPLE, rnd.randint(0, 3)):  # the household's phones: home or not
        did = f"personnes:{_slug(name)}"
        points = [(BINARY("home", write=False), rnd.random() < 0.6), (TEXT_READ("zone"), "maison")]
        devices.append({"id": did, "instance": "personnes", "native_name": name, "native_room": None,
                        "points": [p for p, _ in points], "state": {p["key"]: {"value": v, "ts": 1} for p, v in points}})
        meta[did] = {"family": "person", "room": NO_ROOM, "name": name}
    return {"date": "2026-10-17", "layout": {"rooms": [{"name": r} for r in rooms]}, "protected": protected,
            "devices": devices, "meta": meta, "id": house_id, "split": split}


def devices(house):
    """Devices with their generator metadata merged in."""
    return [{**d, **house["meta"][d["id"]]} for d in house["devices"]]


def point_spec(device, key):
    return next((p for p in device["points"] if p["key"] == key), None)
