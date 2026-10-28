# Adaptateur Intermarché (`intermarche`)

> **Statut : vérifié le 2026-10-28** sur un compte connecté, magasin drive choisi : recherche, fiche,
> prix, prix unitaire, ajout d'un article, panier, retrait de l'article. Pas encore vu : le minimum
> de commande (pas affiché au panier).

- Site : `https://www.intermarche.com/`
- Genre : drive (retrait au magasin choisi).

## Le magasin

- Magasin choisi : son nom est dans l'en-tête, en deuxième position, juste après le logo
  (`document.querySelector('header').innerText`). Le comparer à `settings.store` des réglages de
  Moli. Session ouverte : l'en-tête montre « Compte » ; « Me connecter » = pas de session :
  s'arrêter, demander à l'humain de se connecter.
- **Sans magasin choisi**, « Courses en ligne » ou une recherche ouvrent une fenêtre « Saisissez
  votre adresse » avec une carte : **c'est le choix du magasin, réservé à l'humain**. L'agent ferme
  cette fenêtre (Échap), le dit et ne relève rien chez Intermarché.

## Chercher

- URL de recherche : `https://www.intermarche.com/recherche/<termes>` (`%20` pour l'espace).
  Chercher un EAN ne donne rien : chercher par la marque et le nom.
- `get_page_text` rend la liste : « (n produits) », puis par produit la marque, le libellé, le
  conditionnement et le prix unitaire (« Le sac de 3kg • 3,19 €/Kg »), le prix, et les mentions
  « Prix en baisse » / « Au lieu de … ».
- Les liens des fiches : `a[href*="/produit/"]`. La liste se charge au défilement : faire défiler
  avant de lister les liens.
- Une marque absente des résultats n'est pas vendue dans ce magasin : le dire, ne pas prendre un
  produit voisin.

## La fiche produit

- URL : `https://www.intermarche.com/produit/<slug>/<EAN>`. L'EAN en fin d'URL identifie le produit :
  c'est la `reference` (et l'`ean`) de la fiche dans Moli.
- `get_page_text` rend la fiche proprement : libellé, « le pack de 24x85g - 2,04kg | 4,00 €/Kg »
  (conditionnement et prix unitaire, virgule décimale), puis le prix (« 8,17 € »), puis « Ajouter au
  panier ».
- Rupture : « Indisponible » à la place du prix → relever `available: false`.
- Les produits « en vrac » (fruits, légumes) ont un prix estimé à la pièce (« à partir de 250g
  environ (soit 1 banane) ») : le signaler, le poids réel n'est connu qu'au retrait.
- Les promos se présentent en « -30% sur le 2ème », « 20% en avantage carte », « -35% remise
  immédiate » (prix surligné en jaune, « au lieu de … ») : les noter dans `promo`, ne pas changer la
  quantité pour les déclencher, les proposer en écart.
- Le compte a une page « Mes produits habituels »
  (`/gestion-de-compte/mes-produits-habituels`), lisible avec `get_page_text` : les produits déjà
  achetés, avec leur prix du jour. Utile pour retrouver la fiche d'un produit du foyer ; ce qu'elle
  montre va dans Moli (fiches à confirmer), jamais dans le dépôt ni dans des notes.

## Le panier

- Ajouter : le bouton « Ajouter au panier » de la fiche (cliquer à l'écran ; il est sans effet si le
  volet du panier est ouvert par-dessus). Il devient un sélecteur − n + : « + » ajoute un exemplaire.
- Le bouton « Panier » de l'en-tête ouvre un volet (qui propose aussi des produits : n'y rien
  ajouter) ; « Voir mon panier » ouvre la page `https://www.intermarche.com/commandes/panier`.
- Sur cette page : une corbeille par ligne, le sélecteur de quantité, « Vider le panier » en haut, et
  le résumé (« Total à payer »).
- **« Option de remplacement de produits (activée par défaut) »** : le magasin peut remplacer un
  produit manquant. Ne pas la changer ; le rappeler à l'humain avec le panier.

## La limite à ne pas franchir

Le bouton **« Choisir mon créneau »** du résumé mène au créneau puis au paiement : l'agent s'arrête
au panier rempli et ne clique jamais dessus. Ni créneau, ni code promo, ni validation de commande, ni
paiement.

## Pièges vus en vrai

- Bandeau de cookies à la première visite : « Continuer sans accepter » (en haut à droite de la
  fenêtre) le ferme sans consentement.
- `get_page_text` sur l'accueil ne rend que le contenu principal : l'en-tête (compte, magasin,
  panier) se lit avec `find`, `read_page` ou `document.querySelector('header')`.
- Rien ne s'affiche sans magasin choisi : le vérifier en premier.
