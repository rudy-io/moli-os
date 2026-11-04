# Qobuz

Moli joue la musique de l'abonnement Qobuz sur les enceintes de la maison :
une recherche (« du jazz », « l'album Thriller », « Bohemian Rhapsody ») est
traduite en une file de pistes que les pilotes `cast` et `sonos` jouent via
leur point `play`. Demandez à voix haute au satellite, ou écrivez `play`
d'une enceinte depuis le tableau de bord.

**À savoir avant de l'installer.** Qobuz ne propose aucune interface
officielle pour ça : Moli utilise celle du player web Qobuz, comme le font
qobuz-dl et streamrip. Elle peut changer ou cesser sans préavis.

## Configuration

```toml
[[driver]]
id = "qobuz"
kind = "qobuz"
[driver.options]
format_id = 6   # 5 = MP3 320 kb/s, 6 = FLAC 16/44.1, 7/27 = hi-res 24-bit
queue = 12      # pistes mises en file par recherche
```

Les identifiants (`app_id`, `app_secret`, `email`, `password`) vivent dans
le coffre chiffré : voir `onboarding.md`.

## Ce que Moli voit

| Point | Sens |
|---|---|
| `status` | `missing`, `connecting`, `connected`, `error` |
