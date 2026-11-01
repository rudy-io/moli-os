"""Contract v4 requests: several devices at once, relative settings, the state of the house, the time,
the weather, ambiances and colours, orders and reminders for later, automations, several calls, follow-ups,
English, and spoken or typed noise. Phrasings are lists (« piles ») written once by a large model and read
over; the last quarter of each list is held out for the test houses (`phrases._pick`)."""
import unicodedata

from . import houses, phrases
from .phrases import _pick, dans, de_ref, ref, unique

LIGHTS = {"light", "light_dim", "strip", "light_color"}
DIMMABLE = {"light_dim", "strip", "light_color"}
CLIMS = {"ac", "ac_z"}
MEDIA = {"speaker", "tv"}
SWITCHABLE = LIGHTS | {"plug", "tv", "helper", "purifier", "fan"}

# ---- piles -------------------------------------------------------------------------------------------
LIGHTS_OFF = ["Éteins toutes les lumières{w}.", "Coupe toutes les lumières{w}.", "Éteins les lumières{w}.",
              "Tu peux éteindre toutes les lampes{w} ?", "Éteins tout l'éclairage{w}.", "Plus aucune lumière{w}, s'il te plaît.",
              "J'aimerais que tu éteignes les lumières{w}.", "Toutes les lumières{w} : éteintes.", "Éteins-moi les lumières{w}.",
              "Coupe l'éclairage{w}.", "Il faudrait éteindre toutes les lumières{w}.", "On éteint les lumières{w} ?"]
LIGHTS_ON = ["Allume toutes les lumières{w}.", "Allume les lumières{w}.", "Mets toutes les lampes{w} en marche.",
             "Tu peux allumer toutes les lumières{w} ?", "Allume tout l'éclairage{w}.", "Éclaire{w}, s'il te plaît.",
             "J'aimerais de la lumière partout{w}.", "Toutes les lumières{w}, allumées.", "Allume-moi les lumières{w}.",
             "Rallume les lumières{w}.", "Il faudrait allumer les lumières{w}.", "On allume tout l'éclairage{w} ?"]
LIGHTS_LEVEL = ["Mets toutes les lumières{w} à {n} %.", "Toutes les lampes{w} à {n} %.", "Règle les lumières{w} sur {n} %.",
                "Baisse toutes les lumières{w} à {n} %.", "Lumières{w} à {n} pour cent.", "Passe l'éclairage{w} à {n} %.",
                "Mets l'éclairage{w} sur {n} %.", "Toutes les lumières{w} sur {n} pour cent, merci."]
SHUT_CLOSE = ["Ferme tous les volets{w}.", "Baisse les volets{w}.", "Descends tous les volets{w}.", "Ferme les volets{w}.",
              "Tu peux fermer tous les volets{w} ?", "Ferme les stores{w}.", "Volets{w} : fermés, s'il te plaît.",
              "Il faudrait fermer les volets{w}."]
SHUT_OPEN = ["Ouvre tous les volets{w}.", "Monte les volets{w}.", "Lève tous les volets{w}.", "Ouvre les volets{w}.",
             "Tu peux ouvrir tous les volets{w} ?", "Ouvre les stores{w}.", "Volets{w} : ouverts, s'il te plaît.",
             "Il faudrait ouvrir les volets{w}."]
PLUGS_OFF = ["Coupe toutes les prises{w}.", "Éteins les prises{w}.", "Toutes les prises{w} à l'arrêt.",
             "Coupe le courant des prises{w}."]
CLIMS_OFF = ["Éteins toutes les clims.", "Coupe la climatisation partout.", "Arrête toutes les clims.", "Plus de clim nulle part."]
MEDIA_OFF = ["Coupe la musique et la télé partout.", "Éteins tous les écrans et les enceintes.", "Arrête tous les médias.",
             "Plus de son nulle part."]
ALL_OFF = ["Éteins tout{w}.", "Coupe tout{w}.", "Je vais me coucher, éteins tout.", "Je pars, éteins tout.",
           "Éteins tout ce qui est allumé{w}.", "On éteint tout{w}.", "Tu peux tout éteindre{w} ?", "Extinction des feux !"]

WARMER = [("Monte un peu {r}.", 1), ("Augmente {r} d'un degré.", 1), ("Monte {r} de deux degrés.", 2),
          ("{R}, un degré de plus.", 1), ("Monte {r} de 3 degrés.", 3), ("Plus chaud avec {r}, s'il te plaît.", 1),
          ("Augmente un peu {r}.", 1), ("Rajoute deux degrés sur {r}.", 2)]
COOLER = [("Baisse un peu {r}.", -1), ("Diminue {r} d'un degré.", -1), ("Baisse {r} de deux degrés.", -2),
          ("{R}, un degré de moins.", -1), ("Baisse {r} de 3 degrés.", -3), ("Moins chaud avec {r}.", -1),
          ("Réduis un peu {r}.", -1), ("Enlève deux degrés sur {r}.", -2)]
WARMER_ROOM = ["Un peu plus chaud {d}, s'il te plaît.", "Il fait frais {d}, monte un peu.", "Réchauffe un peu {d}.",
               "Un degré de plus {d}."]
COOLER_ROOM = ["Un peu moins chaud {d}.", "Il fait trop chaud {d}, baisse un peu.", "Rafraîchis un peu {d}.",
               "Un degré de moins {d}."]
BRIGHTER = [("Plus de lumière avec {r}.", 20), ("Augmente {r}.", 20), ("Monte un peu {r}.", 10), ("{R}, plus fort.", 20),
            ("Éclaire plus avec {r}.", 30), ("Pousse {r} un peu.", 10), ("Augmente beaucoup {r}.", 40), ("{R}, un peu plus fort.", 10)]
DARKER = [("Baisse un peu {r}.", -20), ("Tamise {r}.", -30), ("Moins fort {r}.", -20), ("{R}, plus doux.", -20),
          ("Diminue {r}.", -20), ("Baisse beaucoup {r}.", -40), ("{R}, un peu moins fort.", -10), ("Atténue {r}.", -30)]
LOUDER = [("Monte le son {dr}.", 10), ("Plus fort {dr}.", 10), ("Augmente le volume {dr}.", 10), ("Monte beaucoup le son {dr}.", 25),
          ("Un peu plus fort {dr}.", 5), ("Pousse le volume {dr}.", 15)]
QUIETER = [("Baisse le son {dr}.", -10), ("Moins fort {dr}.", -10), ("Diminue le volume {dr}.", -10), ("Baisse beaucoup le son {dr}.", -25),
           ("Un peu moins fort {dr}.", -5), ("Baisse un peu le volume {dr}.", -5)]

FIND = [("Quelles lumières sont allumées ?", "lumières", "allumé", False), ("Il reste des lumières allumées ?", "lumières", "allumé", False),
        ("Qu'est-ce qui est allumé {d} ?", None, "allumé", True), ("Y a-t-il une fenêtre ouverte ?", None, "ouvert", False),
        ("Qu'est-ce qui est ouvert ?", None, "ouvert", False), ("Quels volets sont encore ouverts ?", "volets", "ouvert", False),
        ("Quels appareils ne répondent pas ?", None, "injoignable", False), ("Il y a des piles à changer ?", None, "batterie_faible", False),
        ("Quelles prises sont allumées ?", "prises", "allumé", False), ("Quelles lumières sont allumées {d} ?", "lumières", "allumé", True),
        ("Il y a une lumière allumée {d} ?", "lumières", "allumé", True), ("Qu'est-ce qui est éteint {d} ?", None, "éteint", True),
        ("Des appareils hors ligne ?", None, "injoignable", False), ("Des batteries faibles ?", None, "batterie_faible", False),
        ("Tout est fermé ?", None, "ouvert", False), ("Quelles lumières ai-je oubliées ?", "lumières", "allumé", False)]
PROBLEMS = ["Y a-t-il un problème ?", "Tout va bien à la maison ?", "Quelque chose ne va pas ?", "Comment va la maison ?",
            "Des alertes ?", "Il y a un souci ?", "Fais-moi le point sur la maison.", "Tout est en ordre ?",
            "Comment ça se passe à la maison ?", "Quelque chose à signaler ?", "La maison va bien ?", "Rien d'anormal ?"]
NOW = ["Quelle heure est-il ?", "Il est quelle heure ?", "On est quel jour ?", "Quelle est la date d'aujourd'hui ?",
       "Tu as l'heure ?", "C'est quel jour aujourd'hui ?", "Donne-moi l'heure.", "On est le combien ?",
       "Quel jour sommes-nous ?", "Dis-moi l'heure.", "Il est quelle heure là ?", "On est quelle date ?"]
LAST_CHANGE = ["Depuis quand {r} est dans cet état ?", "Qui a modifié {r} ?", "{R} a changé quand ?",
               "C'est quand la dernière fois que {r} a changé ?", "Qui a commandé {r} ?", "Depuis combien de temps {r} est comme ça ?",
               "Quand est-ce que {r} a bougé ?", "Qui s'est servi {dr} ?"]
WEATHER = {0: ["Quel temps fait-il aujourd'hui ?", "Il va pleuvoir aujourd'hui ?", "Quelle météo aujourd'hui ?",
               "Il fait beau aujourd'hui ?", "Je dois sortir, il va faire quel temps ?", "Il va pleuvoir cet après-midi ?",
               "Quel temps pour la journée ?", "La météo du jour ?"],
           1: ["Quel temps fait-il demain ?", "Il va pleuvoir demain ?", "Quelle météo pour demain ?", "Demain, il fera beau ?",
               "Je prends un parapluie demain ?", "Il fera chaud demain ?", "Quel temps demain matin ?", "Qu'annonce la météo pour demain ?"],
           2: ["Et après-demain, quel temps ?", "La météo d'après-demain ?", "Il va pleuvoir après-demain ?", "Quel temps dans deux jours ?"]}
LOOKS = {"cinema": "cinéma", "feu": "feu de cheminée", "bougies": "bougies", "lecture": "lecture", "veilleuse": "veilleuse",
         "fete": "fête", "romantique": "romantique", "cocooning": "cocooning", "ocean": "océan", "foret": "forêt",
         "aurore": "aurore", "fin-de-journee": "fin de journée"}
AMBIANCE = ["Mets une ambiance {look} {d}.", "Ambiance {look} {d}, s'il te plaît.", "Je voudrais une lumière {look} {d}.",
            "Lance l'ambiance {look} {d}.", "Une ambiance {look} {d}, merci.", "Passe {d} en ambiance {look}.",
            "Fais-moi une ambiance {look} {d}.", "Ambiance {look} {d}."]
COLOURS = ["rouge", "bleu", "vert", "jaune", "orange", "violet", "rose", "blanc chaud", "blanc froid", "cyan"]
COLOUR_ROOM = ["Mets du {c} {d}.", "Lumière {c} {d}, s'il te plaît.", "Passe les lumières {d} en {c}.", "Tout en {c} {d}."]
COLOUR_DEVICE = ["Mets {r} en {c}.", "Passe {r} en {c}.", "{R} en {c}, s'il te plaît.", "Change la couleur {dr} en {c}.",
                 "Je veux {r} en {c}.", "Colore {r} en {c}.", "{R} : {c}.", "Mets une lumière {c} sur {r}."]
DURATIONS = [("5 minutes", 5), ("dix minutes", 10), ("un quart d'heure", 15), ("20 minutes", 20), ("une demi-heure", 30),
             ("une heure", 60), ("deux heures", 120), ("45 minutes", 45), ("3 minutes", 3), ("une minute", 1),
             ("trois quarts d'heure", 45), ("1 h 30", 90)]
LATER_ON = ["Allume {r} dans {t}.", "Dans {t}, allume {r}.", "Mets {r} en marche dans {t}.", "Allume-moi {r} d'ici {t}."]
LATER_OFF = ["Éteins {r} dans {t}.", "Dans {t}, éteins {r}.", "Coupe {r} dans {t}.", "Éteins-moi {r} d'ici {t}."]
LATER_CLOSE = ["Ferme {r} dans {t}.", "Dans {t}, ferme {r}.", "Baisse {r} dans {t}.", "Descends {r} d'ici {t}."]
TASKS = ["sortir le linge", "retirer les pâtes", "sortir les poubelles", "arroser les plantes", "éteindre le four",
         "aller chercher les enfants", "prendre mes médicaments", "rentrer le linge", "appeler le plombier", "sortir le chien",
         "vérifier le gâteau", "couper le four"]
REMIND = ["Rappelle-moi de {task} dans {t}.", "Dans {t}, rappelle-moi de {task}.", "Préviens-moi dans {t} pour {task}.",
          "N'oublie pas de me dire de {task} dans {t}.", "Fais-moi penser à {task} dans {t}.", "Rappel dans {t} : {task}.",
          "Tu me rappelles de {task} dans {t} ?", "Dans {t}, dis-moi de {task}."]
TIMER = ["Mets un minuteur de {t}.", "Minuteur {t}.", "Lance un minuteur de {t}.", "Un minuteur de {t}, s'il te plaît.",
         "Chrono de {t}.", "Compte {t} et préviens-moi.", "Minuteur de {t} pour la cuisson.", "Déclenche un minuteur de {t}."]
AUTOMATIONS = ["Quand j'arrive à la maison, allume l'entrée.", "Tous les soirs à 22 h, ferme les volets.",
               "Si une fenêtre reste ouverte plus de dix minutes, préviens-moi.", "Allume le salon au coucher du soleil.",
               "Quand il fait plus de 26 degrés, allume la clim.", "Chaque matin à 7 h, allume la cuisine.",
               "Crée un automatisme qui éteint tout quand je pars.", "Fais une routine pour le réveil.",
               "Quand il y a du mouvement dans le couloir la nuit, allume la veilleuse.", "Préviens-moi si la porte de garage reste ouverte.",
               "Tous les jours à 18 h, ferme les volets du salon.", "Quand le dernier part, coupe toutes les lumières.",
               "Programme la clim à 21 degrés tous les soirs.", "Si de la fumée est détectée, allume toutes les lumières.",
               "Je voudrais que les volets s'ouvrent au lever du soleil.", "Fais en sorte que la lumière de l'entrée s'allume quand on sonne."]
MULTI_ON = ["Allume {r}", "Active {r}", "Mets en marche {r}", "Rallume {r}"]
MULTI_OFF = ["Éteins {r}", "Coupe {r}", "Arrête {r}", "Mets {r} à l'arrêt"]
MULTI = ["{A} et {b}.", "{A}, puis {b}.", "{A}, et aussi {b}.", "{A} ; {b}."]
EN_ON = ["Turn on the {name}{w}.", "Switch on the {name}{w}.", "Please turn the {name}{w} on.", "Can you turn on the {name}{w}?"]
EN_OFF = ["Turn off the {name}{w}.", "Switch off the {name}{w}.", "Please turn the {name}{w} off.", "Can you turn off the {name}{w}?"]
EN_FIXED = [("Turn off all the lights.", {"tool": "set_all", "arguments": {"kind": "lumières", "value": False}}),
            ("Close all the shutters.", {"tool": "set_all", "arguments": {"kind": "volets", "value": "close"}}),
            ("What time is it?", {"tool": "now", "arguments": {}}),
            ("What's the weather like tomorrow?", {"tool": "weather", "arguments": {"day": 1}}),
            ("Is anything wrong in the house?", {"tool": "problems", "arguments": {}}),
            ("Which lights are on?", {"tool": "find", "arguments": {"kind": "lumières", "state": "allumé"}}),
            ("Remind me to take the bins out in 10 minutes.", {"tool": "remind", "arguments": {"minutes": 10, "text": "Take the bins out"}}),
            ("Set a timer for five minutes.", {"tool": "remind", "arguments": {"minutes": 5, "text": "Timer: 5 minutes"}}),
            ("Undo that.", {"tool": "undo", "arguments": {}}),
            ("Turn everything off.", {"tool": "set_all", "arguments": {"kind": "tout", "value": False}})]


def _elide(text):
    """« de appeler » → « d'appeler », « que il » never written."""
    import re
    return re.sub(r"\b([Dd])e ([aeiouyéèêàâîôûh])", r"\1'\2", text)


def _cap(text):
    return text[0].upper() + text[1:]


def _w(room):
    return "" if room is None else " " + dans(room)


def _call(tool, **args):
    return {"tool": tool, "arguments": args}


def _case(cat, user, answer, **extra):
    return {"cat": cat, "user": user, "answer": answer, **extra}


def _fmt(template, dev, **kw):
    r = ref(dev)
    return template.format(r=r, R=_cap(r), dr=de_ref(dev), **kw)


def _off_point(dev):
    keys = {p["key"]: p for p in dev["points"] if p["access"]["write"]}
    if "state" in keys:
        return "state", True, False
    if "on" in keys:
        return "on", True, False
    return None


def noisy(rnd, text):
    """A typed or heard sentence: accents dropped, a letter doubled, dropped or swapped, no capital, no final mark."""
    out = text
    for _ in range(rnd.randint(1, 2)):
        kind = rnd.choice(["accents", "double", "drop", "swap", "lower", "end"])
        letters = [i for i, c in enumerate(out) if c.isalpha()]
        if kind == "accents":
            out = "".join(c for c in unicodedata.normalize("NFD", out) if unicodedata.category(c) != "Mn")
        elif kind == "lower":
            out = out.lower()
        elif kind == "end":
            out = out.rstrip(" .?!")
        elif letters:
            i = rnd.choice(letters)
            if kind == "double":
                out = out[:i] + out[i] + out[i:]
            elif kind == "drop":
                out = out[:i] + out[i + 1:]
            elif kind == "swap" and i + 1 < len(out) and out[i + 1].isalpha():
                out = out[:i] + out[i + 1] + out[i] + out[i + 2:]
    return out


def generate(rnd, house, test):
    """Contract v4 cases for `house` (devices of protected rooms are never the target of an order)."""
    devs = [d for d in houses.devices(house) if d["family"] != "person"]
    free = [d for d in devs if d["room"] not in house["protected"] and unique(devs, d)]
    rooms_with = lambda fams: sorted({d["room"] for d in devs if d["family"] in fams and d["room"] not in house["protected"]
                                      and d["room"] != houses.NO_ROOM})
    pick = lambda pool: _pick(rnd, pool, test)
    out = []

    # several devices at once
    for pool, fams, kind, value in ((LIGHTS_OFF, LIGHTS, "lumières", False), (LIGHTS_ON, LIGHTS, "lumières", True),
                                    (SHUT_CLOSE, {"shutter"}, "volets", "close"), (SHUT_OPEN, {"shutter"}, "volets", "open"),
                                    (PLUGS_OFF, {"plug"}, "prises", False)):
        rooms = rooms_with(fams)
        if rooms and rnd.random() < 0.7:
            room = rnd.choice(rooms + [None])
            args = {"kind": kind, "value": value} if room is None else {"kind": kind, "room": room, "value": value}
            out.append(_case("groupe", pick(pool).format(w=_w(room)), _call("set_all", **args)))
    if rooms_with(LIGHTS) and rnd.random() < 0.4:
        room, n = rnd.choice(rooms_with(LIGHTS) + [None]), rnd.choice([10, 20, 30, 50, 70])
        args = {"kind": "lumières", "value": n} if room is None else {"kind": "lumières", "room": room, "value": n}
        out.append(_case("groupe", pick(LIGHTS_LEVEL).format(w=_w(room), n=n), _call("set_all", **args)))
    if rnd.random() < 0.3:
        room = rnd.choice(rooms_with(LIGHTS) + [None, None])
        args = {"kind": "tout", "value": False} if room is None else {"kind": "tout", "room": room, "value": False}
        user = pick(ALL_OFF)
        if "{w}" not in user:
            args = {"kind": "tout", "value": False}
        out.append(_case("groupe", user.format(w=_w(room)), _call("set_all", **args)))
    if any(d["family"] in CLIMS for d in devs) and rnd.random() < 0.2:
        out.append(_case("groupe", pick(CLIMS_OFF), _call("set_all", kind="climatisation", value=False)))
    if any(d["family"] in MEDIA for d in devs) and rnd.random() < 0.2:
        out.append(_case("groupe", pick(MEDIA_OFF), _call("set_all", kind="médias", value=False)))

    # relative settings, from the current value (Moli reads it)
    for dev in free:
        fam, keys = dev["family"], {p["key"] for p in dev["points"] if p["access"]["write"]}
        if fam in CLIMS:
            key = "target_temperature" if "target_temperature" in keys else "occupied_heating_setpoint"
            template, by = pick(WARMER if rnd.random() < 0.5 else COOLER)
            out.append(_case("relatif", _fmt(template, dev), _call("adjust", point=f"{dev['id']}/{key}", by=by)))
            alone = sum(d["family"] in CLIMS for d in devs if d["room"] == dev["room"]) == 1
            if alone and dev["room"] != houses.NO_ROOM and rnd.random() < 0.5:
                warm = rnd.random() < 0.5
                said = pick(WARMER_ROOM if warm else COOLER_ROOM).format(d=dans(dev["room"]))
                out.append(_case("relatif", said, _call("adjust", point=f"{dev['id']}/{key}", by=1 if warm else -1)))
        elif fam in DIMMABLE and "brightness" in keys and rnd.random() < 0.6:
            template, by = pick(BRIGHTER if rnd.random() < 0.5 else DARKER)
            out.append(_case("relatif", _fmt(template, dev), _call("adjust", point=f"{dev['id']}/brightness", by=by)))
        elif fam in MEDIA and "volume" in keys:
            template, by = pick(LOUDER if rnd.random() < 0.5 else QUIETER)
            out.append(_case("relatif", _fmt(template, dev), _call("adjust", point=f"{dev['id']}/volume", by=by)))

    # the state of the house
    for _ in range(3):
        said, kind, state, roomed = pick(FIND)
        args = {"state": state} if kind is None else {"kind": kind, "state": state}
        if roomed:
            room = rnd.choice([r["name"] for r in house["layout"]["rooms"]])
            args["room"] = room
            said = said.format(d=dans(room))
        out.append(_case("état", said, _call("find", **args)))
    out.append(_case("problèmes", pick(PROBLEMS), _call("problems")))
    out.append(_case("heure", pick(NOW), _call("now")))
    for dev in rnd.sample(free, min(2, len(free))):
        if any(p["access"]["write"] for p in dev["points"]) or dev["family"] == "door":
            out.append(_case("dernier_changement", _fmt(pick(LAST_CHANGE), dev), _call("last_change", device=dev["id"])))
    day = rnd.choice([0, 1, 1, 2])
    out.append(_case("météo", pick(WEATHER[day]), _call("weather", day=day)))

    # ambiances and colours
    lit = rooms_with(LIGHTS)
    if lit:
        room, look = rnd.choice(lit), rnd.choice(list(LOOKS))
        out.append(_case("ambiance", pick(AMBIANCE).format(look=LOOKS[look], d=dans(room)), _call("ambiance", room=room, look=look)))
        if rnd.random() < 0.5:
            colour = rnd.choice(COLOURS)
            out.append(_case("ambiance", pick(COLOUR_ROOM).format(c=colour, d=dans(room)), _call("ambiance", room=room, color=colour)))
    for dev in free:
        if dev["family"] == "light_color" and rnd.random() < 0.7:
            colour = rnd.choice(COLOURS)
            out.append(_case("couleur", _fmt(pick(COLOUR_DEVICE), dev, c=colour), _call("set", point=f"{dev['id']}/color", value=colour)))

    # later: orders and reminders
    for dev in rnd.sample(free, min(3, len(free))):
        said_t, minutes = rnd.choice(DURATIONS)
        keys = {p["key"] for p in dev["points"] if p["access"]["write"]}
        if dev["family"] == "shutter" and "control" in keys:
            out.append(_case("programmé", _fmt(pick(LATER_CLOSE), dev, t=said_t),
                             _call("schedule", minutes=minutes, point=f"{dev['id']}/control", value="close")))
        elif dev["family"] in SWITCHABLE and _off_point(dev):
            key, _, _ = _off_point(dev)
            on = rnd.random() < 0.5
            out.append(_case("programmé", _fmt(pick(LATER_ON if on else LATER_OFF), dev, t=said_t),
                             _call("schedule", minutes=minutes, point=f"{dev['id']}/{key}", value=on)))
    said_t, minutes = rnd.choice(DURATIONS)
    task = rnd.choice(TASKS)
    out.append(_case("rappel", _elide(pick(REMIND).format(task=task, t=said_t)), _call("remind", minutes=minutes, text=_cap(task))))
    said_t, minutes = rnd.choice(DURATIONS)
    out.append(_case("rappel", _elide(pick(TIMER).format(t=said_t)), _call("remind", minutes=minutes, text=_elide(f"Minuteur de {said_t} terminé"))))
    if rnd.random() < 0.5:
        said = pick(AUTOMATIONS)
        out.append(_case("automatisme", said, _call("draft_automation", description=said)))

    # several calls in one answer
    switch = [d for d in free if d["family"] in SWITCHABLE and _off_point(d)]
    if len(switch) >= 2 and rnd.random() < 0.7:
        a, b = rnd.sample(switch, 2)
        on_a, on_b = rnd.random() < 0.5, rnd.random() < 0.5
        verb = lambda dev, on: _fmt(rnd.choice(MULTI_ON if on else MULTI_OFF), dev)
        first, second = verb(a, on_a), verb(b, on_b)
        user = pick(MULTI).format(A=_cap(first), b=second[0].lower() + second[1:])
        out.append(_case("multi", user, {"calls": [_call("set", point=f"{a['id']}/{_off_point(a)[0]}", value=on_a),
                                                   _call("set", point=f"{b['id']}/{_off_point(b)[0]}", value=on_b)]}))

    # follow-ups: the turn before says what « la » or « et dans… » means
    if switch:
        dev = rnd.choice(switch)
        key = _off_point(dev)[0]
        pronoun = "la" if houses.ART.get(dev["name"], "le") == "la" or dev["name"] in houses.FEMININE else "le"
        context = [(_fmt(phrases.ON[0], dev), f"C’est fait : {dev['name']}.")]
        out.append(_case("suite", rnd.choice([f"Éteins-{pronoun} maintenant.", f"Éteins-{pronoun}.", f"Tu peux l'éteindre ?",
                                              "Coupe-le, finalement." if pronoun == "le" else "Coupe-la, finalement."]),
                         _call("set", point=f"{dev['id']}/{key}", value=False), context=context))
        if len(switch) >= 2:
            other = rnd.choice([d for d in switch if d is not dev])
            out.append(_case("suite", _cap(f"et {ref(other)} aussi."),
                             _call("set", point=f"{other['id']}/{_off_point(other)[0]}", value=True), context=context))
    sensors = [d for d in free if d["family"] == "sensor" and d["room"] != houses.NO_ROOM
               and sum(x["family"] == "sensor" for x in devs if x["room"] == d["room"]) == 1]
    if len(sensors) >= 2:
        a, b = rnd.sample(sensors, 2)
        context = [(f"Quelle température fait-il {dans(a['room'])} ?", f"{a['name']} : température 19,5 °C.")]
        out.append(_case("suite", rnd.choice([f"Et {dans(b['room'])} ?", f"Et {dans(b['room'])}, il fait combien ?"]),
                         _call("get_device", device=b["id"]), context=context))
    if free and rnd.random() < 0.5:  # a turn before that has nothing to do with it: no carry-over
        dev = rnd.choice([d for d in switch] or free)
        if _off_point(dev):
            out.append(_case("suite", _fmt(phrases.ON[0], dev), _call("set", point=f"{dev['id']}/{_off_point(dev)[0]}", value=True),
                             context=[("Bonjour", "Bonjour ! Que veux-tu faire ?")]))

    # English
    for dev in rnd.sample(switch, min(2, len(switch))):
        on = rnd.random() < 0.5
        w = "" if dev["room"] == houses.NO_ROOM else f" in the {dev['room']}"
        out.append(_case("anglais", pick(EN_ON if on else EN_OFF).format(name=dev["name"], w=w),
                         _call("set", point=f"{dev['id']}/{_off_point(dev)[0]}", value=on)))
    said, answer = rnd.choice(EN_FIXED)
    out.append(_case("anglais", said, answer))

    # typed or heard noise on some of the requests (never the English or the follow-ups' context)
    for c in out:
        if c["cat"] not in ("anglais", "automatisme") and rnd.random() < 0.15:
            c["user"] = noisy(rnd, c["user"])
    return out
