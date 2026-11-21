# Freebox (Freebox OS)

- API de Freebox OS : `api_version` en HTTP (sans secret), puis HTTPS local vérifié contre les autorités de
  Free (`crates/moli-freebox/src/freebox-roots.pem`), toutes les 30 s. Lecture seule.
- Autorisation une fois sur l'écran du Freebox Server ; jeton au coffre, sessions par HMAC-SHA1.
- Origine : écrit pour Moli d'après la documentation publique de Free (dev.freebox.fr, API v4) et ce que
  renvoie une Freebox v7 « Delta » (API 16.1) : températures et ventilateurs en listes (`sensors`, `fans`).
  Pas de fixture : les tests lisent des réponses écrites dans le code (formes de la v4 et des API récentes).
- Vérifié sur une Freebox Server v7 (Delta) : chaîne de certificat « Freebox ECC Intermediate CA » →
  « Freebox ECC Root CA », la racine n'étant pas envoyée par la box.
