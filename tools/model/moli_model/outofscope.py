"""What is not the house: questions to hand to another assistant (`delegate`), and vague requests
about the house, answered with guidance instead of an improvisation."""

# Questions outside the frame of the house (conversation, explanation, opinion, culture, calculation).
# The last quarter is held out: only test houses are asked those (see HELD_OUT_BASES).
BASES = [
    "Raconte-moi une blague.", "Tu connais une devinette ?", "Dis-moi une phrase drôle.", "Raconte-moi une histoire courte.",
    "Peux-tu me réciter un poème ?", "Donne-moi une idée de cadeau pour un anniversaire.",
    "Explique-moi ce qu'est une pompe à chaleur.", "Comment fonctionne un panneau solaire ?", "C'est quoi l'inflation ?",
    "Quelle est la différence entre un virus et une bactérie ?", "Pourquoi le ciel est bleu ?", "Comment se forme un arc-en-ciel ?",
    "Qu'est-ce qu'un trou noir ?", "Comment les avions tiennent en l'air ?", "À quoi sert le sel dans l'eau des pâtes ?",
    "Quelle est la capitale de l'Australie ?", "Qui a peint la Joconde ?", "En quelle année a eu lieu la Révolution française ?",
    "Combien de pays y a-t-il en Europe ?", "Quel est le plus long fleuve du monde ?", "Qui a inventé l'imprimerie ?",
    "Donne-moi une recette de crêpes.", "Comment faire cuire un riz basmati ?", "Quelle est la meilleure façon de conserver du pain ?",
    "Que cuisiner ce soir avec des courgettes ?", "Comment faire une vinaigrette ?", "Combien de temps cuit un œuf dur ?",
    "Combien font 17 fois 23 ?", "Quelle est la racine carrée de 144 ?", "Combien y a-t-il de jours dans une année bissextile ?",
    "Convertis 50 miles en kilomètres.", "Combien font 15 pour cent de 240 ?",
    "Comment dit-on bonjour en japonais ?", "Traduis « bonne nuit » en espagnol.", "Comment s'écrit « oiseau » en anglais ?",
    "Donne-moi un conseil pour mieux dormir.", "Comment se débarrasser du hoquet ?", "Que faire pour avoir moins de stress ?",
    "Comment réviser efficacement pour un examen ?", "Quels sont les bienfaits de la marche ?",
    "Comment changer une ampoule grillée ?", "Comment détartrer une bouilloire ?", "Quelle peinture choisir pour une salle de bain ?",
    "Comment ôter une tache de vin sur une nappe ?", "Comment percer un mur en béton ?",
    "Qui a gagné la dernière coupe du monde de football ?", "Quelles sont les règles du tennis ?", "Quel animal court le plus vite ?",
    "Que penses-tu de l'intelligence artificielle ?", "Tu préfères les chats ou les chiens ?", "Quel est le sens de la vie ?",
    "Parle-moi du système solaire.", "Combien de temps faut-il à la lumière du soleil pour arriver sur Terre ?",
    "Pourquoi les feuilles tombent en automne ?", "D'où vient le mot « bureau » ?", "Quelle est la différence entre météo et climat ?",
    "Qu'est-ce que le jazz ?", "Qui était Napoléon ?", "Résume-moi l'histoire de Paris.", "Peux-tu me parler d'astronomie ?",
    "Comment choisir un bon melon ?", "Quel vin avec du poisson ?", "Comment planter des tomates ?", "Quand tailler les rosiers ?",
    "Écris-moi un petit mot pour souhaiter bon anniversaire.", "Aide-moi à écrire un message pour remercier mes voisins.",
    "Propose-moi un nom pour un chat noir.", "Donne-moi trois idées de sortie pour dimanche.",
    "C'est quoi la différence entre un crocodile et un alligator ?", "Pourquoi la mer est salée ?",
    "Comment fonctionne une montre mécanique ?", "Quelle est la hauteur de la tour Eiffel ?",
]
HELD_OUT_BASES = set(BASES[int(len(BASES) * 0.75):])

PREFIXES = ["", "", "", "Dis-moi, ", "Hé Moli, ", "Question : ", "Juste une question : "]
HELD_OUT_PREFIXES = ["Au fait, ", "Moli, "]
SUFFIXES = ["", "", "", " S'il te plaît.", " Merci."]

# Vague requests about the house: no tool can answer them well, guidance can.
VAGUE = [
    "Comment ça se passe à la maison ?", "Y a-t-il un problème ?", "Qu'est-ce qui se passe ?", "Dis-moi tout.",
    "Résume-moi la maison.", "Il y a du nouveau ?", "Tout va bien ?", "Fais-moi un point.", "Que se passe-t-il chez moi ?",
    "Donne-moi des nouvelles de la maison.", "Alors, quoi de neuf ?", "Tout est en ordre ?", "Montre-moi ce qui compte.",
]
VAGUE_HELD_OUT = ["Raconte-moi la journée de la maison.", "Un petit résumé ?", "Où en est-on ?"]
GUIDANCE = [
    "Demande-moi une valeur précise : la température d'une pièce, l'état d'un appareil ou la consommation.",
    "Je réponds mieux à une question précise : l'état d'un appareil, la température d'une pièce, la consommation.",
    "Dis-moi ce qui t'intéresse : un appareil, une pièce, la consommation ?",
]
# The weather is on the dashboard, not in the conversation: said, not delegated to a model
# that knows neither the place nor the day.
WEATHER = ["Quel temps fait-il demain ?", "Va-t-il pleuvoir ce week-end ?", "Quelle météo pour demain ?", "Quel temps fait-il dehors ?"]
WEATHER_HELD_OUT = ["Il va faire beau cet après-midi ?", "Qu'annonce la météo ?"]
WEATHER_ANSWERS = [
    "Je ne donne pas la météo dans la conversation : tu la trouves sur le tableau de bord.",
    "La météo est sur le tableau de bord, je ne la lis pas encore ici.",
]
# Feelings that hide an order: the person is asked what to do, never guessed.
FEELINGS = ["Il fait froid.", "J'ai chaud.", "Il fait sombre ici.", "Il fait trop chaud.", "On n'y voit rien."]
FEELINGS_HELD_OUT = ["Je gèle.", "Il fait lourd."]
ASK_WHAT = [
    "Tu veux que je règle le chauffage, la clim ou la lumière ? Dis-moi la pièce.",
    "Que dois-je faire : régler la température ou la lumière ? Et dans quelle pièce ?",
]


def _ask(base, rnd, test):
    """A base question with a light wrapper; test houses also get the held-out wrappers."""
    prefixes = PREFIXES + (HELD_OUT_PREFIXES if test else [])
    prefix = rnd.choice(prefixes)
    text = base if not prefix else prefix + base[0].lower() + base[1:]
    return text + rnd.choice(SUFFIXES)


def generate(rnd, test):
    """A few out-of-frame cases for one house (few: the same sentences in every house would swamp the
    set). Test houses may be asked the held-out bases. `base` is the question before its wrapper."""
    pool = [b for b in BASES if test or b not in HELD_OUT_BASES]
    out = []
    for base in rnd.sample(pool, 1 + (rnd.random() < 0.5)):
        user = _ask(base, rnd, test)
        out.append({"cat": "hors_cadre", "base": base, "user": user,
                    "answer": {"tool": "delegate", "arguments": {"question": user}}})
    # Contract v4: « how is the house? » is the summary of what is wrong (`problems`), and the weather
    # has its tool (`weather`, in v4_cases).
    if rnd.random() < 0.3:
        base = rnd.choice(VAGUE + (VAGUE_HELD_OUT if test else []))
        out.append({"cat": "problèmes", "base": base, "user": base, "answer": {"tool": "problems", "arguments": {}}})
    if rnd.random() < 0.2:
        base = rnd.choice(FEELINGS + (FEELINGS_HELD_OUT if test else []))
        out.append({"cat": "vague", "base": base, "user": base, "answer": {"text": rnd.choice(ASK_WHAT)}})
    return out
