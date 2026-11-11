# ENGIE Belgique

Le compte client ENGIE Belgique, par l'interface de l'appli ENGIE Smart :

- **Contrat** : les prix de la période en cours, TVA comprise, par énergie, sens et plage
  (`electricity_offtake_peak`, `electricity_offtake_offpeak`, `electricity_injection_peak`,
  `gas_offtake`…, en €/kWh), et les pics du tarif capacitaire (`peak_month_kw`, son quart d'heure
  `peak_month_at`, `peak_today_kw`, `peak_previous_month_kw`). Relus toutes les heures.
- **Véhicule** (recharge intelligente ENGIE) : batterie, autonomie, puissance et état de charge,
  branché ; la session en cours : `departure` (heure de départ), `target_level` (objectif, %) et
  `direct` (charger tout de suite), modifiables ; ce que la recharge intelligente a fait gagner :
  la dernière session (`session_reward`, avec son énergie, son coût et son résultat), ce mois-ci
  (`reward_month`) et au total (`reward_total`, sur les 24 derniers mois), comptés session par session pour ce véhicule seul. Relus toutes les 5 minutes ; les gains, toutes les heures pour le mois
  en cours, une fois par jour pour les mois d'avant. Les sessions ne nomment leur véhicule que
  par son nom : deux véhicules suivis de même nom n'ont pas de gains.
- **Compte** : le solde de récompenses disponible (`reward_balance`, ce que l'appli appelle « disponible
  dans mon solde ») et le seuil à partir duquel ENGIE le verse (`reward_payout_threshold`). Relus
  toutes les heures.

**À savoir avant de l'installer.** ENGIE ne propose aucune interface officielle. Moli utilise
celle de l'appli ENGIE Smart (son identifiant de client, son adresse de retour), comme
l'intégration `engie_be` de Home Assistant : **elle n'est pas officielle et peut casser sans
prévenir**. Chaque requête dit qu'elle vient de Moli (`moli-os` à la fin du `User-Agent`). Se connecter avec le même compte dans l'appli ou sur engie.be **ferme la session de
Moli** : la carte du compte redemande alors une connexion. ENGIE conseille un utilisateur dédié
(gestion des utilisateurs du compte).

## Connexion

Depuis le tableau de bord (Système, carte ENGIE) : un bouton ouvre la vraie page de connexion
d'ENGIE (OAuth avec PKCE) ; on s'y connecte (e-mail, mot de passe, code reçu par SMS ou e-mail :
tout se passe chez ENGIE, Moli ne voit jamais le mot de passe). La page finit en voulant ouvrir
l'appli (`be.engie.smart://login-callback/…?code=…`) : le navigateur n'y arrive pas, c'est normal ;
elle ne s'affiche nulle part : on la prend dans les outils du navigateur (F12, onglet Réseau ouvert
avant la connexion, « Conserver les journaux » coché), en-tête `Location` de la dernière réponse
302 de `/authorize/resume`, et on la colle dans Moli. Moli l'échange contre un jeton de rafraîchissement, rangé chiffré dans son
coffre ; ENGIE le remplace à chaque usage, Moli range aussitôt le nouveau.

## Heure de départ

`departure` accepte `07:30` (la prochaine), `2026-11-12 07:30` (heure du fuseau `timezone` du
pilote, `Europe/Brussels` par défaut, comme l'affichage de `departure`) ou un instant
RFC 3339, dans le futur et à 7 jours au plus. Changer l'heure, l'objectif ou `direct` envoie les
trois réglages de la session en cours (ceux qu'ENGIE a, plus le nouveau), comme l'appli.

## Fixtures

Écrites d'après les champs que lit aioengiebelgium, valeurs fictives : à remplacer par des
réponses réelles anonymisées dès qu'un compte les aura données.
