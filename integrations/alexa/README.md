# Enceintes Amazon Echo (Alexa)

Moli pilote les enceintes Echo d'un compte Amazon comme ses autres enceintes : musique par
recherche (« du jazz », « la playlist Apéro », « France Inter »), lecture et pause, volume, une
phrase dite, une annonce, une commande Alexa. Les groupes multi-pièces de l'appli Alexa
apparaissent comme des enceintes : la musique y joue partout à la fois.

**À savoir avant de l'installer.** Amazon ne propose aucune interface officielle pour piloter
la musique des Echo. Moli utilise celle de l'appli Alexa, comme le font Home Assistant (Alexa
Media Player), ioBroker et Node-RED : elle peut changer ou cesser sans préavis.

## Connexion

Depuis le tableau de bord (Système, carte Alexa) : un bouton ouvre la vraie page de connexion
d'Amazon ; on s'y connecte (mot de passe, code de vérification, captcha : tout se passe chez
Amazon, Moli ne voit jamais le mot de passe) ; la page finit sur une page d'erreur « introuvable »
dont l'adresse commence par `https://www.amazon.com/ap/maplanding` : c'est normal, on la copie et
on la colle dans Moli. Moli s'inscrit alors comme un appareil du compte (« Moli » dans la liste
des appareils Amazon) et garde un jeton chiffré dans son coffre. Le code de la page expire en
quelques minutes : coller l'adresse sans attendre.

Conseils : se connecter depuis un ordinateur (sur un téléphone où l'appli Alexa est installée,
elle peut intercepter la fin de la connexion) ; une double authentification par appli est plus
sûre qu'un code par SMS.

## Configuration

```toml
[[driver]]
id = "alexa"
kind = "alexa"
[driver.options]
domain = "amazon.fr"   # le site Amazon du compte : amazon.fr, amazon.de, amazon.com…
music = ""             # le service par défaut : vide = celui du compte (AMAZON_MUSIC, SPOTIFY, DEEZER, TUNEIN…)
```

## Ce que Moli voit

| Point | Sens |
|---|---|
| `playing` ✎ | lecture en cours (vrai) ou en pause |
| `volume` ✎ | 0 à 100 (pas pour un groupe : ses membres ont le leur) |
| `title`, `artist`, `state` | ce qui joue |
| `play` ✎ | une recherche musicale, jouée avec le service du compte |
| `say` ✎ | une phrase dite (250 caractères au plus ; un groupe : par chaque membre) |
| `announce` ✎ | une annonce (son d'annonce, puis la phrase ; à activer sur l'Echo) |
| `command` ✎ | une commande Alexa écrite, comme si on lui parlait |

## Limites

- Pause et lecture ne marchent que si la musique vient d'Alexa (pas de Spotify Connect lancé
  depuis un téléphone).
- Un changement de mot de passe Amazon peut obliger à refaire la connexion.
- L'état de lecture est relu toutes les 30 s.
