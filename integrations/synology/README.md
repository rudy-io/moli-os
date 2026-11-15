# NAS Synology (DSM)

Pilote natif `crates/moli-synology`, **lecture et ordres** (`wake`, `shutdown`, `reboot`).

> **Éprouvé** sur un DS1019+ (DSM 7.4) : connexion, `SYNO.DSM.Info`,
> utilisation, stockage, onduleur et Hyper Backup ont répondu comme prévu ; les fixtures sont ces
> réponses, anonymisées. L'ordre **`reboot`** a aussi été envoyé pour de vrai : retenu par la garde,
> approuvé, DSM répond `success`, le NAS redémarre et Moli le relit seul (c'est ainsi qu'on a trouvé le refus
> de connexion pendant le démarrage de DSM). **Pas éprouvé** : `shutdown` (même appel que `reboot`, jamais
> envoyé) et `wake` (Wake-on-LAN désactivé sur ce NAS), un disque,
> un volume ou une sauvegarde **en panne** (fixtures `*-faulty` reconstituées : ce NAS est sain),
> un onduleur **activé** (celui de ce NAS ne l'est pas : les mots de statut NUT `OB`, `LB`, `RB`
> restent à confirmer).

- **Transport** : l'API web de DSM, **HTTPS seulement** (5001 par défaut), certificat **épinglé au
  premier contact** (empreinte rangée dans le coffre sous `cert`, avant que le mot de passe parte).
  Chaque appel est un POST de formulaire : le mot de passe et le jeton de session ne passent
  jamais dans une adresse. Une seule session ; perdue, elle est rouverte et l'appel renvoyé une fois.
- **Lecture** toutes les 60 s (option `poll_secs`, de 5 à 3600) : `SYNO.DSM.Info`,
  `SYNO.Core.System.Utilization`, `SYNO.Storage.CGI.Storage`, `SYNO.Core.ExternalDevice.UPS`,
  `SYNO.Backup.Task`. Un compte qui n'a pas le droit à une partie, ou une lecture annexe qui
  échoue (délai compris), perd cette partie, pas le reste : seul `SYNO.DSM.Info` dit si le NAS répond.
- **Veille des disques** : `SYNO.Storage.CGI.Storage` est l'appel du Gestionnaire de stockage, qui peut
  garder éveillés (ou réveiller) des disques que DSM voudrait endormir. Moli ne le lit donc que
  **toutes les 10 minutes** (et tout de suite après une coupure), pas à chaque lecture. Sur le
  DS1019+ essayé, la veille est réglée à 20 min et le journal d'hibernation de DSM est désactivé : l'effet
  réel du poll n'a pas été mesuré. Disques, volumes et leur alerte se rafraîchissent donc toutes les 10 min.
- **Appareils** :
  - `nas` : en marche (`up`), température, processeur, mémoire, durée d'allumage, `problem` /
    `detail` (température trop haute), et l'ordre `control` (`wake`, `shutdown`, `reboot`) ;
  - `volume_<n>` : occupation, espace libre (Go), état, `problem` / `detail` ;
  - `disk_<id>` : température, état, `problem` / `detail` (état, SMART, secteurs défectueux, usure) ;
  - `ups` (seulement s'il est activé dans DSM) : batterie, état, `problem` (sur batterie ou batterie à remplacer) ;
  - `backup_<n>` : un par tâche Hyper Backup — état, dernier résultat, dernière exécution,
    `problem` (le dernier résultat n'est pas une réussite).
- **Alertes** : `problem` (binaire, sens `alert`) et `detail` (texte) : la page « Parc » du
  dashboard liste ce qui ne va pas, et un automatisme peut s'y accrocher.
- **Un NAS éteint est normal** : l'appareil `nas` reste publié, `up` passe à faux, les valeurs à
  « inconnu », les disques, volumes et tâches hors ligne. Le pilote ne tombe pas en panne : l'ordre
  `wake` reste servi.
- **Ordres** (sens `control` : un agent a besoin de l'accord d'un humain) :
  - `wake` : paquet Wake-on-LAN (UDP, ports 9 et 7). Demande la **MAC** du NAS (option `mac`, ou
    apprise dans la table de voisins quand `host` est une adresse IPv4 du réseau local) et
    Wake-on-LAN activé dans DSM (Panneau de configuration → Matériel et alimentation → Général).
    Sans MAC, l'ordre n'est pas proposé ;
  - `shutdown`, `reboot` : `SYNO.Core.System` (`force=false`). **Demandent un compte administrateur.**
    **Exécutés au plus une fois** : renvoyés seulement si DSM répond « session perdue » (preuve qu'il
    ne les a pas exécutés), jamais après un échec dont on ignore l'issue.
- **Connexion refusée** : le pilote **n'insiste pas** (DSM bloque l'adresse après quelques échecs) :
  avertissement visible, NAS affiché « éteint », ordres refusés, et **un seul nouvel essai par
  pause, de plus en plus longue** (15 min, 30, 60, puis toutes les 2 h). DSM refuse aussi les connexions pendant son démarrage : un NAS qui redémarre (essai réel
  du `reboot`) revient donc tout seul. Mot de passe en cause : `moli-os secrets set`, puis relancer Moli.
  Le certificat qui change (`PIN_MISMATCH`) : attente d'un humain, avec `moli-os secrets forget <instance> cert`.
- **Certificat renouvelé** : si DSM renouvelle un certificat Let's Encrypt (environ tous les 90
  jours), l'empreinte épinglée change et le pilote s'arrête en attente (`PIN_MISMATCH`) jusqu'à
  `moli-os secrets forget <instance> cert`. Un certificat auto-signé de DSM, lui, ne change pas.
- **Pas encore** : double authentification (le compte de Moli ne doit pas en avoir), plusieurs
  NAS dans une instance (une instance = un NAS), mises à jour DSM, journaux, partages, Docker.
