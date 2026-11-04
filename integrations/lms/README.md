# piCorePlayer / Lyrion Music Server (LMS / Squeezebox)

- Pilote les lecteurs SqueezeLite / piCorePlayer via l'API JSON-RPC locale de Lyrion Music
  Server (`POST /jsonrpc.js` sur le port `9000`).
- Lecture / pause (`playing`), piste suivante / précédente (`next`, `previous`), volume
  (`0` à `100 %`), sourdine (`mute`), titre, artiste et pochette (`image`).
- Recherche musicale Qobuz (`play`) : utilise les identifiants de piste `qobuz://<track_id>.flac`
  du plugin Qobuz de LMS (ou l'URL HTTP directe en repli).
