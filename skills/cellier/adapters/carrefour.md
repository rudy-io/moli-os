# Adaptateur Carrefour (`carrefour`)

> **Statut : vérifié le 2026-10-28** sur un compte connecté, magasin drive choisi : recherche, fiche,
> prix, prix unitaire, ajout d'un article, panier, retrait de l'article. Pas encore vu : un produit
> en rupture au drive.

- Site : `https://www.carrefour.fr/`
- Genre : drive ou livraison selon le mode choisi ; le Cellier n'utilise que le **drive**.

## Le magasin

- Session ouverte : l'en-tête affiche « Bonjour <prénom> ». « Mon compte » / « Me connecter » à la
  place = pas de session : s'arrêter et demander à l'humain de se connecter.
- Magasin choisi : sous l'en-tête, une barre de modes « Drive · <magasin> ✓ », « Livraison »,
  « Livraison Express ». Vérifier que la coche est sur **Drive** et que le nom correspond à
  `settings.store`. Mauvais mode ou mauvais magasin : ne rien changer, s'arrêter et le dire. Avec un
  magasin choisi, l'accueil devient `/courses` (« Ravis de vous revoir »).
- Sans magasin : l'accueil affiche « Faire mes courses ». Le choix se fait dans une fenêtre :
  « Drive », puis une adresse ou une ville, puis « Choisir ce drive » sur une ligne de la liste. Il
  revient à l'humain : l'agent s'arrête et le dit.
- Attention aux noms : le magasin porte le nom de la commune où il est, pas forcément celui de la
  ville du foyer. Un « City » ou un « Express » en « Drive piéton » est un petit magasin, au choix
  réduit.

## Chercher

- URL de recherche : `https://www.carrefour.fr/s?q=<termes>` (`+` pour l'espace).
- Lister les résultats avec `find` (« liens des titres de produits ») : le lien du titre donne la
  fiche. `get_page_text` sur les résultats ne rend qu'un produit.
- Les résultats mélangent des produits voisins : vérifier le libellé et le conditionnement.

## La fiche produit

- URL : `https://www.carrefour.fr/p/<slug>-<EAN>`. L'EAN (13 chiffres en fin d'URL) identifie le
  produit : c'est la `reference` (et l'`ean`) de la fiche dans Moli.
- **Ne pas lire le prix avec `get_page_text`** : avec un magasin choisi, il rend un produit des
  suggestions, pas celui de la fiche. Lire le bloc d'achat :
  `document.querySelector('.delivery-choice').innerText` → `"<magasin> 8 ,79 € 2.93 € / KG
  Disponible en Drive Ajouter au panier"`. Le prix est en deux morceaux à recoller (`8` et `,79`) ;
  le prix unitaire a un point décimal et l'unité après la barre.
- La promo éventuelle apparaît dans ce bloc (« -15% MARDI PASS », « LE 2ÈME À -20% »…) : la noter dans
  `promo`.
- Le conditionnement est dans le titre de l'onglet (« … : le paquet de 3Kg à Prix Carrefour ») et
  sous le titre : il donne `unit_qty` et `unit`.
- Rupture : libellé pas encore vu. « Disponible en Drive » absent du bloc d'achat la signale
  probablement : relever `available: false` en le disant à l'humain, et noter ici le libellé vu.

## Le panier

- Ajouter : le bouton « Ajouter au panier » du bloc d'achat (cliquer à l'écran ; un clic par
  référence `find` ne part pas toujours). Une fenêtre « … a bien été ajouté à votre panier ! » propose
  des produits : la fermer, ne rien y ajouter.
- URL du panier drive : `https://www.carrefour.fr/cart/driveclcv`. Chaque ligne a « Supprimer » et un
  sélecteur − n + pour la quantité ; « Vider mon panier » en bas.
- Le résumé affiche le magasin (« Drive <magasin> · Retrait Parking »), le sous-total, la
  « Provision sacs consignés » (0,35 € par sac, remboursée au retour des sacs) et le total.
- Option de remplacement des produits manquants : pas observée au panier ; à noter ici si elle
  apparaît plus loin.
- **Minimum de commande : 30 €** (« Plus que x € pour compléter la commande » tant qu'il n'est pas
  atteint). À reporter dans `min_order_cents` des réglages Moli.

## La limite à ne pas franchir

Le panier est l'étape « 1. Votre Panier » sur « 2. Votre Drive » (créneau) et « 3. Votre paiement ».
Le bouton **« Valider mon panier »** mène au créneau : l'agent s'arrête au panier rempli et ne
clique jamais dessus, ni sur le champ « Code promo ».

## Pièges vus en vrai

- Bandeau de cookies à la première visite : « Refuser tous les cookies » le ferme sans
  consentement.
- Une fenêtre promo (« 20 € offerts… ») peut s'ouvrir au premier clic sur l'accueil : la fermer.
- Sans magasin, des prix s'affichent quand même : ce ne sont **pas** ceux du drive. Ne jamais les
  relever.
- Un assistant de courses (« Demander à hopla ») flotte en bas de page : ne pas s'en servir.
- Promotions de type « le 2e à -x % » ou « prenez-en 3 = payez-en 2 » : noter dans `promo`, proposer
  en écart, ne pas changer la quantité soi-même.
