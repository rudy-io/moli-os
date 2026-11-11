# Adaptateurs d'enseigne

Un adaptateur dit à l'agent comment se servir du site d'une enseigne : où lire, où cliquer, et
surtout où s'arrêter. Le skill `cellier` (`../SKILL.md`) ne pilote que les enseignes qui en ont un.

## Ajouter une enseigne

1. **Côté Moli** : une ligne dans `PROVIDERS` (`crates/moli-pantry/src/providers.rs`) avec son `id`,
   son nom, son genre (`Drive` ou `Online`) et son site. Le minimum de commande et les frais de
   préparation se règlent dans les réglages de Moli, pas ici.
2. **Côté skill** : un fichier `adapters/<id>.md`, construit **en observant le vrai site** dans le
   Chrome de l'utilisateur, connecté, magasin déjà choisi, en lecture seule et sans rien commander.
3. **Vérifier** avec l'adaptateur seul : relever le prix de trois produits courants, ajouter puis
   retirer un article du panier. Le panier doit revenir exactement à son état de départ (le noter
   avant de commencer).

## Ce que décrit un adaptateur

- comment **voir** le magasin choisi (et comment l'humain le **change** : l'agent ne le fait pas) ;
- comment **chercher** un produit, et la forme de l'URL d'une fiche (référence, EAN) ;
- où lire sur une fiche produit le **prix**, le **prix au kilo ou au litre**, la **promo** et la
  **rupture**, avec `get_page_text` de préférence aux captures ;
- comment **ajouter une quantité** au panier ;
- comment **vider** le panier ;
- l'**URL du panier** ;
- où le **paiement commence** : la limite à ne pas franchir ;
- les **pièges** vus en vrai (bandeaux, fenêtres, prix qui dépendent du magasin…).

## Ce qu'un adaptateur ne contient jamais

Ce dépôt est public : aucun nom de magasin, de ville ou de personne, aucune donnée de compte, aucun
historique de commande. Décrire **comment lire** le magasin choisi, pas **lequel** c'est.
`sh scripts/leak-check.sh` doit passer.
