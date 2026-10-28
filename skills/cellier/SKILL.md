---
name: cellier
description: Prépare les courses du foyer avec le Cellier de Moli OS (outils MCP pantry_*) et les drives de l'utilisateur dans son propre Chrome (Claude in Chrome) - relève les vrais prix, propose un panier, remplit le ou les paniers des drives et s'arrête avant le paiement. Déclencheurs - « ajoute … aux courses », « prépare les courses », « compare Inter et Carrefour », « j'ai passé la commande », « on a récupéré le drive », « voilà le ticket de caisse », « qu'est-ce qu'on a acheté ».
---

<!--
La méthode de ce skill reprend en partie celle de jlestel/claude-courses
(https://github.com/jlestel/claude-courses), sous licence MIT :

MIT License

Copyright (c) 2026 Julien Lestel

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
-->

# Le Cellier : les courses du foyer

> Méthode reprise en partie de [jlestel/claude-courses](https://github.com/jlestel/claude-courses)
> (MIT, © 2026 Julien Lestel, avis de licence complet en tête de ce fichier) : passer par le Chrome
> réel de l'utilisateur plutôt qu'un navigateur headless, vérifier le magasin sans le changer,
> repérer une fiche par l'EAN en fin d'URL, relever prix, prix au kilo/litre et promo, ne pas
> changer une quantité pour déclencher une promo mais la signaler, ne jamais vider un panier sans
> le dire, s'arrêter au panier rempli. Ce qui change ici : la liste, les fiches, les prix et le
> panier vivent dans Moli (outils MCP `pantry_*`), et la comparaison entre drives est faite par Moli.

Le Cellier tient la liste de courses, les produits du foyer, leurs fiches par enseigne et les prix
relevés. Moli compare ; l'agent lit les vrais prix sur les sites, remplit les paniers et
**s'arrête avant le paiement**. Rien n'est jamais payé par un agent.

## Prérequis

- Le serveur MCP de Moli est branché sur la session (`claude mcp add --transport http moli
  http://<hôte>:8790/mcp`). Sans lui : le dire et s'arrêter. `pantry_overview` qui répond « Le
  Cellier n'est pas activé sur cette instance » : la brique n'est pas configurée (`[pantry]`).
- Claude in Chrome est connecté au **Chrome de l'utilisateur**, avec les comptes des enseignes
  ouverts et le magasin de chaque enseigne déjà choisi sur le site.
- La connexion et les captchas se font **toujours par l'humain**, jamais par l'agent.
- Une fiche par enseigne : lire l'adaptateur correspondant dans `adapters/` (`intermarche.md`,
  `carrefour.md`) avant de toucher au site. Une enseigne sans adaptateur ne se pilote pas.
- Un adaptateur au statut brouillon sert à relever les prix en lecture seule : on ne touche ni au
  panier ni au compte tant que sa limite de paiement n'est pas décrite et vérifiée.

Les appels qui écrivent prennent `actor` (le nom de l'agent) et `on_behalf_of` (le prénom de la
personne qui parle, s'il y en a une), gardés avec ce qui est écrit, sauf `pantry_save_product`, qui ne
garde aucun auteur et ne les prend pas, et `pantry_record_purchases`, qui ne les garde que sur un
ticket de caisse (argument `receipt`).

## Ajouter un article

« Ajoute du lait aux courses » → `pantry_add_item` avec `text` (le besoin, dans les mots du foyer),
`qty` si elle est dite, `product` si le besoin correspond à un produit connu de `pantry_overview`,
et `on_behalf_of` = la personne qui parle. Redemander un besoin déjà ouvert en augmente la
quantité (sauf si sa ligne est déjà dans un panier rempli ou commandé : la demande fait alors une
nouvelle ligne) : ne pas le vérifier soi-même, Moli s'en charge. Retirer : `pantry_remove_item`
avec l'`id` de l'article.

## Préparer les courses

1. **`pantry_overview`** : la liste ouverte (`items`), les produits, les fiches (`offers`, avec
   leur `status` `pending` ou `confirmed`), les enseignes (`providers` : `info` et `settings` avec
   `enabled`, `store`, `min_order_cents`, `fee_cents`), le seuil et le panier ouvert.
2. **Pour chaque enseigne activée** (`settings.enabled`, `info.kind` = `drive`), ouvrir le site dans
   un nouvel onglet et vérifier, comme le décrit son adaptateur, que le magasin choisi sur le site est
   bien celui des réglages (`settings.store`). Différent, absent, ou page de connexion : le dire et
   ne pas relever cette enseigne. **Ne jamais changer le magasin soi-même.**
3. **Pour chaque article** de la liste :
   - une fiche **confirmée** de l'enseigne existe → ouvrir sa page (référence ou EAN) ;
   - sinon chercher un produit qui respecte la **flexibilité** du produit : `strict` (exactement ce
     produit), `format` (même produit, autre conditionnement), `brand` (même type, autre marque),
     `cheapest` (le moins cher qui convient). Un article sans produit : chercher son `text`.
   - un produit au `fixed_provider` ne se cherche que chez cette enseigne.
4. **Relever** sur la fiche produit le prix, le prix unitaire (kilo, litre, pièce), la promo et la
   disponibilité (lecture de texte de préférence aux captures, voir l'adaptateur), puis
   `pantry_record_prices` avec une lecture par fiche : `offer`, `store` = `settings.store` de
   l'enseigne, `price_cents`, `unit_price_cents`, `promo`, `available` (faux si rupture). Une fiche
   **nouvelle** passe d'abord par `pantry_save_product` (`product` + `offers` : `provider`,
   `reference`, `label`, `ean`, `unit_qty`, `unit`) qui renvoie l'`id` de la fiche ; elle reste « à
   confirmer » jusqu'à ce qu'un humain la confirme. Relever juste avant de proposer : un prix de plus
   de 12 h ne compte plus.
5. **Repérer les écarts malins**, sans les appliquer : lot ou « 2e à -x % » en promo, rupture et
   remplaçant, marque distributeur équivalente, grand format quand le rythme de rachat le justifie.
   Chacun devient un écart `{product, kind, text, gain_cents}` (`kind` : `promo_pack`,
   `out_of_stock`, `own_brand`, `bigger_size`…). Un écart refusé deux fois n'est plus proposé par
   Moli : ne pas insister.
6. **`pantry_propose_basket`** avec les écarts. Moli calcule une option par enseigne (`singles`), un
   panier réparti (`split`) et la recommandation (`options.recommended`), minimum de commande et frais
   compris. Les articles sans prix frais nulle part sont à part (`nowhere`) ; ceux déjà dans un
   panier rempli ou commandé ne sont pas repris.
7. **Présenter dans la conversation** : pour chaque option le total, l'économie du panier recommandé
   (`saving_cents`, `saving_ratio`), les écarts expliqués et numérotés (leur index sert au choix), les
   articles introuvables, les fiches encore « à confirmer », et toute option invalide (minimum de
   commande non atteint). Puis attendre le choix de l'humain.

## Où en est le panier

Le tableau de bord fait les mêmes gestes que l'agent : l'humain a pu choisir l'option, marquer la
commande payée ou les courses récupérées lui-même. **Avant de choisir, remplir ou enregistrer**,
relire `pantry_overview` et le `status` du panier (`basket.status`), puis sauter les étapes déjà
faites : déjà `chosen` → ne pas appeler `pantry_choose_option` ; déjà `filled` → ne pas remplir à
nouveau ; déjà `ordered` → ne pas repasser le statut à `ordered` ; `done` ou plus de panier ouvert →
seulement enregistrer les lignes corrigées si l'humain les donne. `pantry_overview` ne montre que le
dernier panier ouvert : garder l'`id` du panier en cours, et chaque article dit dans `basket` le
panier qui l'achète.

## Après le choix de l'humain

1. **`pantry_choose_option`** avec `basket`, `option` (l'id de l'enseigne, ou `split`) et
   `accepted` (les index des écarts acceptés). Cela confirme les fiches utilisées.
2. **Vider puis remplir** le ou les paniers sur le site, selon l'adaptateur. Un panier du site qui
   n'est pas vide : lister ce qu'il contient et demander avant de le vider. N'appliquer que les écarts
   acceptés.
3. **Vérifier le total** affiché par le site (il fait foi) contre le total proposé, et signaler tout
   écart de plus de 5 %, ligne par ligne si possible.
4. **`pantry_set_basket_status`** avec `status: "filled"`.
5. **S'arrêter avant le paiement.** Donner à l'humain le lien du panier de chaque drive et lui
   laisser le choix du créneau de retrait, la validation et le paiement.

## Commande passée

« J'ai passé la commande » :

1. Lire le récapitulatif de commande sur le site (page de confirmation ou détail de la commande dans
   le compte).
2. **`pantry_record_purchases`** avec `basket` et une ligne par produit : `product`, `provider`,
   `store`, `qty`, `price_cents` (prix unitaire), `at` (ms depuis l'epoch), `source: "drive"`,
   `status: "provisional"`.
3. **`pantry_set_basket_status`** avec `status: "ordered"`.

## Après le retrait

« On a récupéré le drive » :

1. Lire la facture ou le mail de fin de commande (ruptures, remplacements, poids réels), que
   l'humain montre ou ouvre lui-même.
2. **`pantry_record_purchases`** avec `basket`, les lignes corrigées (`status: "confirmed"`) et
   `confirm: true` : elles remplacent les lignes `drive` déjà enregistrées pour ce panier
   (provisoires, ou une correction précédente). Renvoyer le même appel ne compte qu'une fois.

## Tickets de caisse

Le foyer peut scanner ses tickets lui-même : dans le tableau de bord, page **Achats**, il photographie
le ticket, Moli en tire un **brouillon** (enseigne, date, total, lignes) et c'est un humain qui le
vérifie et le **valide** : rien ne devient un achat avant. Ce chemin n'est pas celui de l'agent (la
lecture par Moli et la validation sont réservées au tableau de bord) : si le foyer a **une photo à
faire lire par Moli**, le renvoyer vers la page Achats. Si l'humain montre le ticket à l'agent, c'est
l'agent qui l'enregistre lui-même, comme ci-dessous.

Quand l'humain montre un ticket **à l'agent** (photo ou texte lu par l'agent lui-même) :

1. Lire le ticket soi-même. **Ne jamais envoyer la photo à Moli** : aucun outil ne le permet, et un
   ticket n'a rien à faire ailleurs que dans la conversation.
2. **`pantry_overview`** pour retrouver l'`id` des produits connus et celui de l'enseigne
   (`providers[].info`).
3. **`pantry_record_purchases`** avec `receipt` = `{provider, store, at, total_cents}` (l'enseigne par
   son id, `store` la commune du magasin comme le fait la lecture des photos par Moli, la date du
   ticket en ms depuis l'epoch, le total imprimé en centimes) et `lines` : une ligne par article, `source: "ticket"`, `status: "confirmed"`, `product`,
   `provider`, `store`, `qty`, `price_cents` (prix unitaire), `at`, et si besoin `label` (le libellé
   imprimé), `discount_cents`, `line_cents` (ce que la ligne a vraiment coûté, remise déduite) et
   `unit` (`kg`, `l`) pour un produit pesé. Moli reprend l'enseigne, le magasin et la date de
   `receipt` pour toutes les lignes, et retient chaque `label` pour reconnaître le produit au
   prochain ticket : le donner tel qu'imprimé. Les montants sont toujours en centimes. Un produit
   inconnu passe d'abord par `pantry_save_product`.
4. Un ticket déjà enregistré (même enseigne, même minute, même total) est refusé : ne pas le
   renvoyer en changeant un détail, le dire. Un nouvel essai après une erreur ne compte pas deux fois,
   mais seulement si `total_cents` est donné : **toujours le passer** quand le ticket affiche son
   total (sans lui, la garde contre les doublons ne joue pas).
5. Dire ce qui a été enregistré (nombre de lignes, total) et ce qui reste douteux.

Pour **répondre à une question sur ce qui a été acheté** (« combien on a dépensé en lait ce mois-ci ? »,
« à quel prix a-t-on pris le café la dernière fois ? ») : **`pantry_purchases`** avec `from` et `to`
en ms (30 derniers jours par défaut). Il renvoie `{purchases, receipts}` : les achats, avec les
tickets dont ils viennent. Les montants sont en centimes, `line_cents` est ce qu'une ligne a vraiment
coûté. Les brouillons de ticket (`receipts` au statut `draft`) ne sont **pas encore des achats** :
les signaler comme « à valider par le foyer », ne pas les compter.

## Règles

- **Jamais de produit changé en douce** : un écart n'est appliqué que s'il a été accepté.
- **Jamais de paiement**, ni de créneau choisi, ni de commande validée par l'agent.
- **Jamais de mot de passe saisi par l'agent**, ni d'adresse, de moyen de paiement ou de réglage de
  compte modifié.
- Si le site affiche un **captcha ou une page de connexion**, l'agent s'arrête et le dit.
- Ne pas changer le magasin choisi sur le site ; ne pas accepter de conditions. Bandeau de cookies :
  le refuser (« Continuer sans accepter », « Refuser tous les cookies »).
- **Naviguer au rythme d'un humain** : une page à la fois, pas de boucle rapide.
- Ne jamais confirmer une fiche (seul un humain le fait dans le tableau de bord) ni essayer de passer
  un panier à `done`, ni de valider un brouillon de ticket.
- N'utiliser que les outils MCP `pantry_*`, jamais l'API REST du Cellier.
