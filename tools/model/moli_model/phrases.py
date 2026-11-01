"""French requests and their expected answers, for a given house."""
from . import houses

CATEGORIES = ("commande", "configuration", "pilote", "lecture", "énergie", "historique", "protégée", "ambigu", "inexistant", "bavardage",
              "hors_cadre", "vague", "annulation", "groupe", "relatif", "état", "problèmes", "heure", "dernier_changement", "météo",
              "ambiance", "couleur", "programmé", "rappel", "automatisme", "multi", "suite", "anglais")
SMALL_TALK = [("Bonjour Moli, ça va ?", "Bonjour ! Je suis là, dis-moi ce que je peux faire."),
              ("Merci beaucoup !", "Avec plaisir."),
              ("Tu peux faire quoi pour moi ?", "Je commande la maison, je lis ses valeurs et je règle quelques options."),
              ("Salut", "Salut ! Que veux-tu faire ?"), ("Bonne nuit", "Bonne nuit !")]
REFUSALS = ["Cette pièce est protégée : je ne peux pas la commander.", "{room} est une pièce protégée, je ne la commande pas.",
            "Je ne commande pas les pièces protégées, et {room} en est une."]
ABSENT = {"Piscine": "la piscine", "Portail": "le portail", "Four": "le four", "Télévision": "la télévision", "Alarme": "l'alarme"}

# Templates per category are ordered; the last quarter is held out (test houses only).
ON = ["Allume {r}.", "Peux-tu allumer {r} ?", "Mets en marche {r}.", "Active {r}.", "Active {r}, merci.", "Lance {r}.",
      "Démarre {r}.", "{R}, peux-tu l'allumer ?"]
OFF = ["Éteins {r}.", "Coupe {r}.", "Tu peux éteindre {r} ?", "Arrête {r}.", "Éteins {r}, merci.", "Coupe {r}, merci.",
       "Stoppe {r}, s'il te plaît.", "Mets {r} à l'arrêt."]
LEVEL = ["Mets {r} à {n} %.", "Règle {r} sur {n} pour cent.", "Baisse {r} à {n} %.", "{R} à {n} % s'il te plaît.",
         "Passe {r} à {n} pour cent.", "Mets {r} sur {n} %.", "Éclaire {r} à {n} %.", "{R} : {n} %, merci."]
SHUT = {"close": ["Ferme {r}.", "Baisse {r}.", "Peux-tu fermer {r} ?", "Descends {r}.", "Abaisse {r}.", "Ferme {r}, s'il te plaît."],
        "open": ["Ouvre {r}.", "Monte {r}.", "Peux-tu ouvrir {r} ?", "Relève {r}.", "Remonte {r}.", "Ouvre {r}, merci."]}
TARGET = ["Mets {r} à {n} degrés.", "Règle {r} sur {n} °C.", "{R} à {n} degrés, s'il te plaît.", "Programme {r} à {n}.",
          "Mets {r} sur {n} degrés.", "Fixe {r} à {n} °C.", "Passe {r} à {n} °C.", "{R} sur {n} °C, merci."]
HELD = 0.75
CONTEXT_SHARE = 0.25
NEUTRAL_TURNS = [("Bonjour", "Bonjour ! Que veux-tu faire ?"), ("Merci", "Avec plaisir."), ("Salut", "Salut ! Que veux-tu faire ?"),
                 ("Quelle heure est-il ?", "Il est 20 h 12, samedi 24 octobre."),
                 ("Raconte-moi une blague.", "Ça, je ne sais pas le faire seul. Je commande la maison, je lis ses valeurs et je règle quelques options."),
                 ("Quel temps fait-il demain ?", "Demain : nuageux, de 9 à 15 °C, 30 % de risque de pluie."),
                 ("Y a-t-il un problème ?", "Tout va bien : tout répond, aucune batterie faible, aucune alerte."),
                 ("Il fait froid.", "Que dois-je faire : régler la température ou la lumière ? Et dans quelle pièce ?")]
FIXED_SHARE = 1 / 8


def ref(dev):
    """« le plafonnier du salon » for a device (name and room)."""
    art = houses.ART.get(dev["name"], "le")
    if dev["room"] == houses.NO_ROOM:
        return f"{art}{'' if art.endswith(chr(39)) else ' '}{dev['name'].lower()}"
    de = houses.DE_ROOM.get(dev["room"], f"de {dev['room'].lower()}")
    if art.endswith("clim"):
        return f"{art} {de}"
    return f"{art}{'' if art.endswith(chr(39)) else ' '}{dev['name'].lower()} {de}"


def de_ref(dev):
    """« du plafonnier du salon », « de la clim de la cuisine »: `de` and the article, contracted."""
    r = ref(dev)
    for article, de in (("le ", "du "), ("les ", "des "), ("la ", "de la "), ("l'", "de l'")):
        if r.startswith(article):
            return de + r[len(article):]
    return "de " + r


def dans(room):
    """« dans le salon » for a room."""
    de = houses.DE_ROOM.get(room, f"de {room.lower()}")
    for prefix, repl in (("du ", "dans le "), ("de la ", "dans la "), ("de l'", "dans l'"), ("des ", "dans les ")):
        if de.startswith(prefix):
            return repl + de[len(prefix):]
    return f"dans {room.lower()}"


def unique(devs, dev):
    """True when nothing else could be meant: no other device of the same name in the same room
    (or, for a device without a room, in the whole house)."""
    if dev["room"] == houses.NO_ROOM:
        return sum(1 for d in devs if d["name"] == dev["name"]) == 1
    return sum(1 for d in devs if d["name"] == dev["name"] and d["room"] == dev["room"]) == 1


def _pick(rnd, templates, test):
    cut = max(1, int(len(templates) * HELD))
    pool = templates[cut:] if test and rnd.random() < 0.6 else templates[:cut]
    return rnd.choice(pool)


def _fmt(template, dev, **kw):
    r = ref(dev)
    return template.format(r=r, R=r[0].upper() + r[1:], dr=de_ref(dev), **kw)


def _set(user, dev, key, value):
    return {"cat": "commande", "user": user,
            "answer": {"tool": "set", "arguments": {"point": f"{dev['id']}/{key}", "value": value}}}


def _request(rnd, dev, test):
    """A request to switch `dev` on (or open it, for a shutter), in the phrasing fit for its kind."""
    if dev["family"] == "shutter":
        return _fmt(_pick(rnd, SHUT[rnd.choice(["close", "open"])], test), dev)
    return _fmt(_pick(rnd, ON, test), dev)


def _both_ways(rnd, dev, test):
    """Two different requests for `dev`: on and off, or open and close for a shutter."""
    if dev["family"] == "shutter":
        return [_fmt(_pick(rnd, SHUT[way], test), dev) for way in ("open", "close")]
    return [_fmt(_pick(rnd, ON, test), dev), _fmt(_pick(rnd, OFF, test), dev)]


def _commands(rnd, h, devs, test):
    out = []
    ok = [d for d in devs if d["room"] not in h["protected"]]
    for dev in ok:
        if not unique(devs, dev):
            continue
        keys = {p["key"] for p in dev["points"] if p["access"]["write"]}
        fam = dev["family"]
        if "state" in keys:
            on = rnd.random() < 0.6
            out.append(_set(_fmt(_pick(rnd, ON if on else OFF, test), dev), dev, "state", on))
            if "brightness" in keys and rnd.random() < 0.7:
                n = rnd.choice([10, 20, 30, 40, 50, 70, 80])
                out.append(_set(_fmt(_pick(rnd, LEVEL, test), dev, n=n), dev, "brightness", n))
        if "on" in keys and fam == "ac":
            on = rnd.random() < 0.6
            out.append(_set(_fmt(_pick(rnd, ON if on else OFF, test), dev), dev, "on", on))
            if rnd.random() < 0.7:
                n = rnd.choice([19, 20, 21, 22, 23, 24, 26])
                out.append(_set(_fmt(_pick(rnd, TARGET, test), dev, n=n), dev, "target_temperature", n))
        if "control" in keys:
            way = rnd.choice(["close", "open"])
            out.append(_set(_fmt(_pick(rnd, SHUT[way], test), dev), dev, "control", way))
    return out


READ_TEMPERATURE = ["Quelle température fait-il {where} ?", "Il fait combien {where} ?", "Quelle est la température {where} ?",
                    "Il fait quelle température {where} ?"]
READ_HUMIDITY = ["Quel est le taux d'humidité {where} ?", "Il y a quelle humidité {where} ?", "Combien d'humidité {where} ?",
                 "Quelle humidité {where} ?"]
READ_OPEN = ["{R}, c'est ouvert ou fermé ?", "Dis-moi l'état {dr}.", "{R}, c'est fermé ?", "Où en est {r} ?"]
READ_ONOFF = ["{R}, c'est allumé ou éteint ?", "Dis-moi l'état {dr}.", "{R}, c'est en marche ?", "Où en est {r} ?"]


def _get(user, dev):
    return {"cat": "lecture", "user": user, "answer": {"tool": "get_device", "arguments": {"device": dev["id"]}}}


def _reads(rnd, h, devs, test):
    """A reading is a `get_device` call: Moli reads the values and says them, the model never guesses."""
    out = []
    for dev in devs:
        if dev["room"] in h["protected"] or not unique(devs, dev):
            continue
        keys = {p["key"] for p in dev["points"]}
        fam = dev["family"]
        # « the temperature in the room » must name one device only
        alone = sum("temperature" in {p["key"] for p in d["points"]} for d in devs if d["room"] == dev["room"]) == 1
        if fam == "sensor" and alone and dev["room"] != houses.NO_ROOM:
            out.append(_get(_fmt(_pick(rnd, READ_TEMPERATURE, test), dev, where=dans(dev["room"])), dev))
            out.append(_get(_fmt(_pick(rnd, READ_HUMIDITY, test), dev, where=dans(dev["room"])), dev))
        elif fam == "door":
            out.append(_get(_fmt(_pick(rnd, READ_OPEN, test), dev), dev))
        elif fam == "shutter":
            out.append(_get(_fmt(_pick(rnd, READ_OPEN, test), dev), dev))
        elif fam in ("light", "light_dim", "strip", "plug", "ac", "fan") and rnd.random() < 0.5:
            out.append(_get(_fmt(_pick(rnd, READ_ONOFF, test), dev), dev))
    return out


def _protected(rnd, h, devs, test):
    out = []
    for dev in devs:
        if dev["room"] in h["protected"] and any(p["access"]["write"] for p in dev["points"]) and unique(devs, dev):
            out.append({"cat": "protégée", "user": _request(rnd, dev, test),
                        "answer": {"text": rnd.choice(REFUSALS).format(room=dev["room"])}})
    return out


def _what(name):
    art = houses.ART.get(name, "le")
    return f"{art}{'' if art.endswith(chr(39)) else ' '}{name.lower()}"


def _ambiguous(rnd, h, devs):
    out = []
    by_name = {}
    for d in devs:
        if d["room"] not in h["protected"] and any(p["access"]["write"] for p in d["points"]):
            by_name.setdefault(d["name"], []).append(d)
    for name, group in by_name.items():
        rooms = sorted({d["room"] for d in group if d["room"] != houses.NO_ROOM})
        if len(rooms) >= 2 and name in ("Plafonnier", "Lampe", "Volet", "Suspension", "Store", "Prise"):
            verb = rnd.choice(["Ferme", "Ouvre"]) if name in ("Volet", "Store") else rnd.choice(["Allume", "Éteins"])
            choices = " ou ".join(f"{_what(name)} {houses.DE_ROOM.get(r, 'de ' + r.lower())}" for r in rooms[:3])
            which = "Laquelle" if houses.ART.get(name, "le") == "la" else "Lequel"
            out.append({"cat": "ambigu", "user": f"{verb} {_what(name)}.", "answer": {"text": f"{which} : {choices} ?"}})
    return out


def _missing(rnd, devs, fixed):
    """Things the house does not have (the model answers it, and does not invent a device)."""
    out = []
    names = {d["name"] for d in devs}
    for absent, art in ABSENT.items():
        if fixed and absent not in names:
            out.append({"cat": "inexistant", "user": rnd.choice([f"Allume {art}.", f"Mets en marche {art}."]),
                        "answer": {"text": f"Je ne trouve pas {art} dans la maison."}})
    return out


def _energy():
    return [{"cat": "énergie", "user": u, "answer": {"tool": "energy", "arguments": a}} for u, a in [
        ("Combien consomme la maison en ce moment ?", {}), ("Je consomme beaucoup d'électricité là ?", {}),
        ("Quelle est la consommation d'aujourd'hui ?", {"step": "day"}), ("Et sur le mois ?", {"step": "month"}),
        ("La consommation heure par heure ?", {"step": "hour"})]]


def _history(rnd, h, devs):
    out = []
    for dev in devs:
        if dev["family"] == "sensor" and dev["room"] not in h["protected"] and dev["room"] != houses.NO_ROOM and unique(devs, dev):
            where = houses.DE_ROOM.get(dev["room"], f"de {dev['room'].lower()}")
            n = rnd.choice([6, 12, 24])
            out.append({"cat": "historique", "user": f"Quelle était la température {where} ces {n} dernières heures ?",
                        "answer": {"tool": "history", "arguments": {"point": f"{dev['id']}/temperature", "hours": n}}})
            out.append({"cat": "historique", "user": f"Montre-moi l'humidité {where}.",
                        "answer": {"tool": "history", "arguments": {"point": f"{dev['id']}/humidity"}}})
    return out


def generate(rnd, house, n):
    """Up to `n` cases for `house`, a mix of the categories."""
    devs = houses.devices(house)
    test = house["split"] == "test"
    cases = []
    cases += _commands(rnd, house, devs, test)
    cases += _reads(rnd, house, devs, test)
    fixed = rnd.random() < FIXED_SHARE  # the same few sentences in every house would swamp the set and leak into dev/test
    cases += _protected(rnd, house, devs, test)
    cases += _ambiguous(rnd, house, devs)
    cases += _missing(rnd, devs, fixed)
    cases += _energy() if fixed else []
    from . import config_cases, outofscope  # late: config_cases imports this module
    cases += config_cases.generate(rnd, house)
    cases += outofscope.generate(rnd, test)
    cases += _history(rnd, house, devs)
    from . import v4_cases
    cases += v4_cases.generate(rnd, house, test)
    cases += [{"cat": "bavardage", "user": u, "answer": {"text": a}} for u, a in SMALL_TALK] if fixed else []
    # In service every turn after the first comes with the exchanges before it: most of them unrelated.
    # A share of every category gets such a turn before, so that the model learns to ignore it.
    for c in cases:
        if "context" not in c and rnd.random() < CONTEXT_SHARE:
            c["context"] = [rnd.choice(NEUTRAL_TURNS)]
    rnd.shuffle(cases)
    seen, unique_cases = set(), []
    for c in cases:
        if c["user"] not in seen:
            seen.add(c["user"])
            unique_cases.append(c)
    return unique_cases[:n]
