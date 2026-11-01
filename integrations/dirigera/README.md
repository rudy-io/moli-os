# IKEA Dirigera

- Origine : l'API locale du hub n'est pas officielle. Le protocole est lu dans deux clients
  communautaires (lpgera/dirigera en TypeScript, Leggin/dirigera en Python, licence MIT),
  sans code repris.
- Appairage OAuth avec PKCE (`S256`) : `GET /v1/oauth/authorize` donne un code valable 60 s ;
  `POST /v1/oauth/token` répond `403 « Button not pressed… »` tant que personne n'a appuyé,
  `403 « Pairing request timed out »` puis `401 « Invalid authorization code »` quand le
  code est périmé (le pilote en redemande un), et le jeton après l'appui. Une seule demande
  à la fois : une deuxième reçoit `409 « Already one ongoing pairing request »`.
- Certificat : un certificat d'usine (P-384, valable de 2021 à 2071, sous l'autorité
  « IKEA Home smart Root CA »), épinglé au premier contact, avant tout code ni jeton.
- `fixtures/home.json` : le `/v1/home` d'un vrai hub, anonymisé (identifiants, numéros de
  série, noms, codes Matter, position) : 4 lampes (TRADFRI Driver 30W, ampoule CWS, ruban
  ORMANAS), un détecteur MYGGSPRAY (deux appareils `_1` et `_2`), le hub lui-même.
  `fixtures/events.json` : des événements réels du même hub (marche/arrêt depuis l'appli,
  fondu de luminosité, couleur, blanc, `pong`). Rejoués par `crates/moli-dirigera/src/model.rs`.
- Vu sur ce hub : jeton JWT valable 10 ans, plusieurs jetons valables ensemble ; `isOn` et
  `lightLevel` dans un même `PATCH` : `202`, mais la luminosité est ignorée ; teinte et
  saturation arrondies par le hub ; un nom de pièce avec une espace insécable au bout.
- Les points viennent des attributs présents, pas seulement du `deviceType` : le vieux
  détecteur TRADFRI n'a pas `isDetected`, un TIMMERFLOTTE se présente en deux appareils.
- Reste : stores, purificateurs, enceintes, pièces et scènes IKEA ; un produit à plusieurs
  fonctions (VALLHORN) reste en plusieurs appareils (`relationId` non regroupé).
- **Vérifié sur du vrai matériel** (17 octobre 2026, hub DIRIGERA matériel P2.5, firmware
  2.975.3) : appairage par le pilote, chargement, événements, commandes `on`, `brightness`,
  `color` et `color_temp` sur un TRADFRI Driver 30W et un ruban ORMANAS (état remis
  ensuite) ; détecteur MYGGSPRAY lu (injoignable ce jour-là) ; hub débranché puis rebranché :
  appareils hors ligne, puis rechargés sans nouvel appairage (jeton et certificat inchangés) ;
  WebSocket jamais fermé par le hub en 20 min, même sans ping (le ping reste, son `pong`
  prouve que la connexion vit).
- **Pas encore vu en vrai** : prises, boutons de raccourci, capteurs d'air, d'ouverture et de
  fuite (écrits d'après les réponses réelles publiées par lpgera/dirigera).
