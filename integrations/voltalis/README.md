# Voltalis

- API web non officielle (`api.myvoltalis.com`), dépendante d'Internet.
- Lecture et **commandes** : marche/arrêt, mode, consigne, retour au programme (`automatic`). Le
  diagnostic du service (`fault`) est en lecture.
- Un ordre écrit le réglage manuel du radiateur (`PUT manualsetting/{id}`), valable jusqu'à nouvel
  ordre. Choisir un mode ou une consigne allume le radiateur ; éteindre garde le mode pour la suite.
  Un mode que le radiateur n'annonce pas (`availableModes`) n'est jamais envoyé.
- Le service ne donne pas de clé durable : le compte est gardé dans le coffre chiffré, le jeton de
  session en mémoire seulement. Une session d'un autre client (l'application, Home Assistant)
  reste valable en parallèle (vérifié).
- Un compte refusé arrête le pilote jusqu'à l'intervention d'un humain, pour ne pas bloquer le
  compte. Une session expirée est rouverte une fois au relevé suivant.
- Tout statut de diagnostic autre que `OK` compte comme un problème.
- `fixtures/` : vraies réponses réduites aux champs lus, noms et identifiants remplacés.
- Vérifié sur quatre radiateurs réels (`VX_WIRE`, novembre 2026) en lecture ; commandes essayées sur
  l'un : marche, arrêt, mode, consigne, programme et retour au manuel, relus aussitôt auprès du service.
  Sans mode `ECOV` sous la main : cette écriture n'est testée que sur le code.
- Reste : consommation, programmes, chauffe-eau.
