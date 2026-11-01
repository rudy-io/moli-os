"""Basic configuration requests (rename, move, hide, people, location) and driver requests."""
from . import houses, phrases

FIRST_NAMES = ["Léa", "Hugo", "Camille", "Noé", "Inès", "Louis", "Jade", "Paul", "Zoé", "Théo"]
CITIES = ["Lyon", "Rennes", "Bordeaux", "Lille", "Nantes", "Strasbourg", "Dijon", "Tours", "Annecy"]
NEW_NAMES = {"light": ["Lampe de lecture", "Lampe du canapé", "Grande lampe", "Lumière d'ambiance", "Lampe de chevet"],
             "shutter": ["Volet baie vitrée", "Volet côté jardin", "Store de la fenêtre", "Grand volet"],
             "ac": ["Clim principale", "Clim d'appoint", "Climatisation du fond"],
             "plug": ["Prise télé", "Prise bureau", "Prise de la cafetière", "Prise du fond"],
             "sensor": ["Thermomètre", "Sonde de la pièce", "Capteur d'ambiance"],
             "door": ["Porte principale", "Fenêtre du fond", "Accès côté cour"],
             "camera": ["Caméra du fond", "Caméra de surveillance", "Caméra côté cour"]}
NEW_NAMES["media"] = ["Enceinte du salon", "Télé du fond", "Barre de son"]
NEW_NAMES["helper"] = ["Mode absence", "Mode soirée", "Mode calme"]
KIND = {"light_dim": "light", "strip": "light", "fan": "plug", "light_color": "light", "ac_z": "ac", "smoke": "sensor",
        "co": "sensor", "motion": "sensor", "button": "sensor", "speaker": "media", "tv": "media", "vacuum": "plug",
        "lock": "door", "printer": "plug", "purifier": "plug", "helper": "helper"}
NEW_ROOMS = ["Salon télé", "Cuisine ouverte", "Chambre d'amis", "Atelier", "Salle de jeux", "Espace détente"]
DRIVERS = {"Pilote pour un aspirateur robot Dreame": "mon aspirateur robot Dreame",
           "Intégration d'un thermostat Netatmo": "mon thermostat Netatmo",
           "Pilote pour une serrure connectée": "ma serrure connectée",
           "Pilote pour une imprimante 3D Bambu Lab": "mon imprimante 3D Bambu Lab",
           "Code pour lire un compteur d'eau": "mon compteur d'eau",
           "Intégration d'une sonnette vidéo": "ma sonnette vidéo"}
DRIVER_USER = ["Peux-tu écrire un pilote pour {d} ?", "Il faudrait un pilote pour {d}.",
               "Ajoute {d}, il n'y a pas de pilote.", "Écris le code pour {d}."]


# Asking to take the last confirmed change back: « fait l'inverse dsl je me suis trompé ». The model
# does not know what was changed (it has no memory of the dialogue): Moli does.
UNDO = ["Annule la dernière modification.", "Annule le minuteur.", "Annule le rappel.", "Laisse tomber le minuteur.", "Annule.", "Fais l'inverse.", "Reviens en arrière.", "Remets comme avant.",
        "Je me suis trompé, annule.", "Annule ce que tu viens de faire.", "Retour en arrière.", "Défais ça.",
        "Non, annule !", "Rétablis comme avant.", "Annule le dernier changement.", "Fait l'inverse dsl je me suis trompé",
        "Oups, remets comme c'était.", "Annul la dernière modif.", "Ce n'était pas ça, annule.", "Désolé, fais marche arrière.",
        "Annule ça s'il te plaît.", "Remets l'ancien nom.", "Reviens à avant.",
        "Non, n'éteins pas tout à l'heure, annule."]
UNDO_HELD_OUT = set(UNDO[int(len(UNDO) * 0.75):])


def generate(rnd, house):
    all_devs = houses.devices(house)
    devs = [d for d in all_devs if phrases.unique(all_devs, d) and d["family"] != "person"]
    rooms = [r["name"] for r in house["layout"]["rooms"]]
    out = []

    def cfg(user, tool, **args):
        out.append({"cat": "configuration", "user": user, "answer": {"tool": tool, "arguments": args}})

    for dev in rnd.sample(devs, min(2, len(devs))):
        new = rnd.choice(NEW_NAMES[KIND.get(dev["family"], dev["family"])])
        cfg(rnd.choice([f"Renomme {phrases.ref(dev)} en {new}.", f"Appelle {phrases.ref(dev)} « {new} »."]),
            "rename_device", device=dev["id"], new_name=new)
    for dev in rnd.sample(devs, min(2, len(devs))):
        target = rnd.choice([r for r in rooms if r != dev["room"]] or rooms)
        cfg(rnd.choice([f"Déplace {phrases.ref(dev)} {phrases.dans(target)}.", f"Mets {phrases.ref(dev)} dans la pièce {target}."]),
            "move_device", device=dev["id"], room=target)
    for dev in rnd.sample(devs, min(2, len(devs))):
        on = rnd.random() < 0.4
        ask = rnd.choice(["Réactive {r}.", "Remets {r} en service.", "Affiche de nouveau {r}."] if on
                         else ["Masque {r}.", "Cache {r}.", "Retire {r} de l'affichage."])
        cfg(ask.format(r=phrases.ref(dev)), "set_device_enabled", device=dev["id"], enabled=on)
    room = rnd.choice(rooms)
    new_room = rnd.choice(NEW_ROOMS)
    cfg(rnd.choice([f"Renomme la pièce {room} en {new_room}.", f"La pièce « {room} » s'appelle maintenant {new_room}."]),
        "rename_room", room=room, new_name=new_room)
    name = rnd.choice(FIRST_NAMES)
    cfg(rnd.choice([f"Ajoute {name} au foyer.", f"Il y a une nouvelle personne à la maison : {name}."]), "add_person", name=name)
    city = rnd.choice(CITIES)
    cfg(rnd.choice([f"J'habite à {city}.", f"Règle la maison sur {city} pour la météo."]), "set_home_location", place=city)
    cfg(rnd.choice(["Mets la météo à jour.", "Actualise la météo."]), "refresh_weather")
    desc = rnd.choice(list(DRIVERS))
    out.append({"cat": "pilote", "user": rnd.choice(DRIVER_USER).format(d=DRIVERS[desc]),
                "answer": {"tool": "request_driver", "arguments": {"description": desc}}})
    if rnd.random() < 0.6:
        pool = UNDO if house["split"] == "test" else [u for u in UNDO if u not in UNDO_HELD_OUT]
        base = rnd.choice(pool)
        out.append({"cat": "annulation", "base": base, "user": base, "answer": {"tool": "undo", "arguments": {}}})
    return out
