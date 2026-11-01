# Données d'affinage du modèle maison

Générateur de données **synthétiques** pour affiner un petit modèle sur le contrat d'outils de Moli
(`contract/`, version `moli-v4`, 25 outils) : commander, lire, refuser (pièce protégée, réglage, « plus tard »),
poser une question de précision, configurer, programmer, et reconnaître ce qui sort du cadre de la maison.
Maisons et phrases fictives, aucune donnée de maison réelle. Bibliothèque standard Python seulement.

    python3 -m unittest discover -s tests -t .                 # tests
    python3 -m moli_model.build --binary <moli-os> --out data --houses 600 --per-house 40 --seed 7

Le binaire `moli-os` rend le **vrai** prompt : `moli-os model prompt < maison.json` imprime
`{"prompt": …, "aliases": {"d1": "<vrai identifiant>"}}`. Les réponses attendues parlent en numéros (`d3/state`) ;
les vrais identifiants ne sont jamais dans les données. `build` écrit `train`, `dev` et `test` (séparés par maison)
et une fiche (`DATA_CARD.md`). L'entraînement et l'export : `train/README.md`.

## Les corrections de la maison

« Pas ça » note chaque correction dans `assistant-corrections.jsonl`, dans la maison. Exporté **à la main**, ce
journal devient un jeu d'évaluation **réel**, sur le vrai prompt de la maison :

    python3 -m moli_model.corrections --binary <moli-os> \
        --journal assistant-corrections.1.jsonl assistant-corrections.jsonl \
        --devices devices.json --home home.json --out ~/moli-train/reel.jsonl

Le journal tourne à 1 Mo (le précédent devient `assistant-corrections.1.jsonl`) : donner les deux, le plus
ancien d'abord. `devices.json` est la réponse de `GET /api/devices`, `home.json` celui de la maison. Qui a corrigé et quand ne sont
pas repris. Ce fichier contient les pièces, les appareils et les phrases de la maison : il reste chez vous, **jamais
dans le dépôt**.

## Changer le contrat

Les outils (`contract/tools.json`) et le prompt sont figés par version : `contract/snapshots.txt` garde
l'empreinte de `moli-os model tools` et `moli-os model prompt`, et un test échoue s'ils changent sans nouvelle
version. Changer un outil ou le prompt : augmenter `version` dans `tools.json`, ajouter sa ligne
(`MOLI_BLESS=1 cargo test -p moli-assistant contract`), regénérer les données et réentraîner.
