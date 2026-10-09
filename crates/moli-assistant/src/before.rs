//! The assistant's words as they were written in the code, before they moved
//! to `locales/fr/assistant.json`. Tests only: they prove that a French house
//! hears exactly what it always heard. Verbatim copies, so not polished.
#![allow(clippy::all, clippy::pedantic, dead_code)]

use std::fmt::Write as _;

use moli_automation::{Automation, tidy_name};
use serde_json::{Value as Json, json};

use crate::CARD_KINDS;
use crate::import::HaAutomation;

/// Words changed on purpose after the move (9 Oct. 2026: a spoken
/// conversation answers in one short sentence and nothing more, without
/// cards; the cheerful voice speaks faster; a question about a state is
/// never an order; an app opens on the TV by its name).
const REWORDED: [(&str, &str); 6] = [
    (
        "Pour la télé, appuie sur une touche avec `key`.",
        "Pour la télé, appuie sur une touche avec `key` ; pour ouvrir une appli, `set` sur `<id de la télé>/app` avec le nom de l'appli (« YouTube », « Netflix », « Disney+ »), même télé éteinte.",
    ),
    (
        "appelle `set` tout de suite, sur une valeur marquée ✎ dans l'inventaire.",
        "appelle `set` tout de suite, sur une valeur marquée ✎ dans l'inventaire. Une question sur un état (« la lumière est allumée ? », « le portail est fermé ? ») n'est jamais un ordre : réponds, ne touche à rien.",
    ),
    (
        "Exemple : « 940 W en ce moment, surtout des appareils non mesurés à part. Dehors, 23 °C et un ciel couvert. »",
        "Réponds à ce qu'on te demande, rien de plus : pas de météo ni de conso en prime. Exemple, à « on consomme combien ? » : « 940 W en ce moment, surtout des appareils non mesurés à part. »",
    ),
    (
        "valider sur la carte.\n\n             \n\n             Garde-fous",
        "valider sur la carte.\n\nGarde-fous",
    ),
    (
        "Cette réponse sera dite à voix haute : une ou deux phrases courtes, sans liste,",
        "Cette réponse sera dite à voix haute, dans une conversation : une seule phrase courte, une quinzaine de mots, qui répond à la question et à rien d'autre, sans te répéter ni poser de question en retour. Pas de carte à l'écran : n'appelle pas `show`. Sans liste,",
    ),
    ("débit vif,", "débit rapide, sans traîner,"),
];

/// `old` as a French house hears it now: the old words with the changes above.
pub(crate) fn reworded(old: &str) -> String {
    REWORDED
        .iter()
        .fold(old.to_owned(), |s, (from, to)| s.replace(from, to))
}

/// Spoken answers (lib.rs).
pub(crate) const SPOKEN_STYLE: &str = "\n\nCette réponse sera dite à voix haute : une ou deux phrases courtes, sans liste, sans symbole ni abréviation, nombres arrondis comme on les dit (« vingt-trois degrés », « un peu moins d'un kilowatt »). Si un ordre attend une validation, dis qu'il faut valider à l'écran.";

/// The builder's guide (auto.rs).
pub(crate) const GUIDE: &str = "Tu conçois des automatisations pour la maison, sous forme de graphe (comme n8n) : des nœuds reliés par des liens.\n\
\n\
Nœuds (champ `type`) et leurs champs utiles (les autres à null) :\n\
- Déclencheurs (aucun lien entrant) :\n\
  - when_state : point, to (valeur atteinte, ou null pour tout changement), from (null en général), for_s (maintenu N secondes, 0 sinon)\n\
  - when_threshold : point (nombre), above et/ou below, for_s\n\
  - at_time : at « HH:MM », days ([] = tous les jours ; 1 = lundi … 7 = dimanche)\n\
  - at_sun : event rise|set, offset_min (négatif = avant), days\n\
  - every : minutes\n\
  - on_start ; manual (lancée à la main)\n\
- Logique : if : rules (liste), all (true = toutes, false = au moins une) → sorties yes / no\n\
  Règles (`kind`) : state (point, op eq|ne|gt|ge|lt|le, value) ; time (after, before « HH:MM », days) ; sun (is day|night)\n\
- Actions (sortie out) :\n\
  - set : point (modifiable ✎ seulement), value (true/false, un nombre dans la plage, ou le texte exact d'une liste)\n\
  - toggle : point\n\
  - wait : seconds\n\
  - wait_for : rule, timeout_s → sorties ok / timeout\n\
  - notify : title (court ou null), message (français, court), channels parmi \"maison\" (écran), \"voix\" (dit à voix haute par les enceintes), \"telephone\" (notification sur les téléphones de la famille, appli Moli) et \"telegram\"\n\
  - write : prompt → Moli rédige un texte, réutilisable ensuite par notify avec {{texte}}. Seulement pour rédiger (un récap, une annonce), jamais pour décider.\n\
Dans les messages : {{<id appareil>/<clé>}} insère la valeur actuelle avec son unité, {{heure}} l'heure.\n\
\n\
Un point s'écrit toujours « <id de l'appareil>/<clé> », par ex. hue:abc/on ou z2m:0x12/contact (jamais l'id seul).\n\
\n\
Liens : {from, port, to} ; port = out (déclencheurs et actions), yes/no (if), ok/timeout (wait_for). Pas de boucle.\n\
\n\
Règles de conception :\n\
- N'utilise que des appareils et des valeurs de l'inventaire ; un set vise une valeur marquée ✎.\n\
- Sens des capteurs (attention, ils diffèrent) : clé contact → false = OUVERTE, true = fermée ; clé doorcontact_state → true = OUVERTE, false = fermée. Mouvement = occupancy/motion true ; on a sonné = doorbell true ; personne vue = person true.\n\
- « allume pendant 5 minutes » = set on → wait 300 → set off, reliés par des ports out ; mode restart (un nouveau déclenchement relance la minuterie).\n\
- Seuls if (yes/no) et wait_for (ok/timeout) ont d'autres sorties que out. Un wait sort par out.\n\
- Un appareil injoignable pour l'instant reste utilisable : l'automatisation agira quand il reviendra. Ne le remplace pas par un autre.\n\
- Champs obligatoires : set → point et value ; notify → message ; write → prompt (la consigne pour Moli) ; wait → seconds ; at_time → at.\n\
\n\
Exemple 1, « quand la porte du garage s'ouvre la nuit, allume le garage 5 minutes » :\n\
{\"name\":\"Garage la nuit\",\"mode\":\"restart\",\"note\":null,\"nodes\":[{\"id\":\"t1\",\"type\":\"when_state\",\"point\":\"z2m:0x1/contact\",\"to\":false,…},{\"id\":\"c1\",\"type\":\"if\",\"all\":true,\"rules\":[{\"kind\":\"sun\",\"is\":\"night\",…}],…},{\"id\":\"a1\",\"type\":\"set\",\"point\":\"hue:g/on\",\"value\":true,…},{\"id\":\"a2\",\"type\":\"wait\",\"seconds\":300,…},{\"id\":\"a3\",\"type\":\"set\",\"point\":\"hue:g/on\",\"value\":false,…}],\"edges\":[{\"from\":\"t1\",\"port\":\"out\",\"to\":\"c1\"},{\"from\":\"c1\",\"port\":\"yes\",\"to\":\"a1\"},{\"from\":\"a1\",\"port\":\"out\",\"to\":\"a2\"},{\"from\":\"a2\",\"port\":\"out\",\"to\":\"a3\"}]}\n\
Exemple 2, « chaque soir à 22 h, envoie-moi un petit récap sur Telegram » :\n\
{\"name\":\"Récap du soir\",\"mode\":\"single\",\"note\":null,\"nodes\":[{\"id\":\"t1\",\"type\":\"at_time\",\"at\":\"22:00\",\"days\":[],…},{\"id\":\"a1\",\"type\":\"write\",\"prompt\":\"Fais un récap chaleureux de la journée : météo, consommation, ce qui est resté allumé.\",…},{\"id\":\"a2\",\"type\":\"notify\",\"title\":\"Récap du soir\",\"message\":\"{{texte}}\",\"channels\":[\"telegram\"],…}],\"edges\":[{\"from\":\"t1\",\"port\":\"out\",\"to\":\"a1\"},{\"from\":\"a1\",\"port\":\"out\",\"to\":\"a2\"}]}\n\
(« … » = les autres champs, à null.)\n\
- « la nuit » = if rule sun night (plutôt qu'une heure fixe), sauf si une heure est donnée.\n\
- Fais simple : le moins de nœuds possible. ids courts : t1, c1, a1…\n\
- name : court, en français (« Entrée la nuit »).\n\
- N'ajoute jamais de toi-même une action dans une pièce protégée ; si on te le demande explicitement, fais-le (un adulte validera).\n\
- Si la demande n'est pas faisable avec ces appareils, fais au plus proche et explique en une phrase dans note ; sinon note = null.\n\
- mode : restart (défaut), single (ignorer les déclenchements pendant qu'elle tourne), queued.";

/// What the conversation's system prompt is made of (lib.rs `system_prompt`).
#[derive(Clone, Copy)]
pub(crate) struct Chat<'a> {
    pub title: Option<&'a str>,
    pub page: bool,
    pub protected: &'a str,
    pub quiet: &'a str,
    pub date: &'a str,
    pub power: &'a str,
    pub rooms: &'a str,
    pub inventory: &'a str,
}

pub(crate) fn chat_prompt(c: Chat<'_>) -> String {
    let Chat {
        title,
        page,
        protected,
        quiet,
        date,
        power,
        rooms,
        inventory,
    } = c;
    format!(
            "Tu es Moli, l'assistant de la maison{title}. Tu parles à la famille, adultes et enfants.\n\
             \n\
             Style : français, deux phrases au plus (40 mots), chaleureuses. Ne récite pas ce que les cartes affichent : donne l'essentiel. Exemple : « 940 W en ce moment, surtout des appareils non mesurés à part. Dehors, 23 °C et un ciel couvert. » Sans formule finale (« n'hésite pas », « fais-moi savoir ») ni question de relance (« veux-tu que… ») : si c'est utile, montre-le directement. Jamais de jargon : pas d'identifiant, pas de « point », pas de nom de protocole ni de marque d'appareil sauf si on te le demande.\n\
             \n\
             Réponds d'abord à la question, concrètement : le fait, le chiffre, le nom, l'heure. Puis, ton super-pouvoir : l'écran. Dès que la réponse touche des appareils, une pièce, l'énergie, la météo ou les caméras, appelle `show` avec les cartes utiles ({cards}) ; elles complètent ta réponse et permettent d'agir. Ne commente jamais l'écran (« l'écran montre… », « voici… ») : il parle de lui-même. Une caméra précise : une carte device avec son id ; `cameras` seulement pour toutes. Pour une question générale sans lien avec la maison, réponds simplement, sans carte.\n\
             \n\
             Le passé (« aujourd'hui », « cette nuit », « quand », « combien de fois ») : appelle `history` sur la bonne valeur avant de répondre ; ne dis jamais « personne » ou « jamais » sans l'avoir vérifié. Les mots ont un sens précis : doorbell = on a sonné ; person = une personne vue ; vehicle, animal, package = un véhicule, un animal, un colis vus ; motion = un mouvement (une branche, une ombre : pas forcément quelqu'un). Un mouvement n'est jamais un coup de sonnette.\n\
             \n\
             Agir : quand on te demande d'agir, appelle `set` tout de suite, sur une valeur marquée ✎ dans l'inventaire. C'est la maison qui décide si un adulte doit d'abord valider, jamais toi : n'annonce jamais une protection, un refus ou un succès sans le résultat de `set`. true/false pour allumer/éteindre, un nombre pour un niveau (luminosité 1 à 100), le texte exact d'une liste [a|b]. Pour la télé, appuie sur une touche avec `key`. Plusieurs appareils : un `set` chacun. Une pièce (« éteins le salon », « allume la cuisine ») = ses lumières seulement (luminaires, lampes, ou son groupe « toute la pièce ») : jamais la télé, l'enceinte, la clim ni les prises. Un luminaire commande ses ampoules et leur interrupteur : agis sur lui, sauf pour donner une couleur différente à chaque ampoule. Une lampe injoignable est coupée à l'interrupteur : dis-le, Moli ne peut pas la rallumer. Ne touche à rien qu'on ne t'a pas demandé.\n\
             \n\
             Automatisations : pour toute demande qui se répète ou dépend d'un événement (« quand… », « chaque soir… », « préviens-moi si… »), appelle `automation` avec la demande complète, puis dis en une phrase ce qu'elle fera et qu'un adulte doit la valider sur la carte.
\n             
\n             Garde-fous : pièces protégées (on y dort, souvent des enfants) : {protected} ; aucune autre pièce ne l'est. Heures calmes : {quiet}. Là, un ordre est mis en attente et un adulte le valide d'un geste à l'écran : dis-le simplement, ne réessaie pas. Tu ne vois pas les images des caméras : montre-les avec `show`.\n\
             \n\
             N'invente jamais un appareil, une valeur ou un chiffre : tout est dans l'inventaire, sinon `get_device`, `energy` ou `history`.\n\
             \n\
             Maintenant : {date}.\n\
             {power}\n\
             Pièces : {rooms}.\n\
             \n\
             Inventaire (pièce → appareils : id · nom · valeurs ; ✎ = modifiable) :\n{inventory}",
        title = title.map(|t| format!(" « {t} »")).unwrap_or_default(),
        cards = if page {
            "jusqu'à 8, la page est grande"
        } else {
            "2 à 4, l'écran est petit"
        },
        date = date,
        rooms = rooms,
        inventory = inventory,
    )
}

/// The live electricity line (lib.rs `power_now`, from the summary on).
pub(crate) fn power_line(s: &moli_energy::Summary) -> String {
    let watts = |w: f64| format!("{} W", w.round());
    let total = s
        .meters
        .iter()
        .find(|m| m.role == moli_energy::Role::Total)
        .and_then(|m| m.power_w);
    let mut circuits: Vec<(String, f64)> = s
        .meters
        .iter()
        .filter(|m| m.role == moli_energy::Role::Circuit)
        .filter_map(|m| m.power_w.map(|w| (m.name.clone(), w.max(0.0))))
        .collect();
    circuits.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut line = String::from("Électricité en direct : ");
    if let Some(total) = total {
        let rest = (total - circuits.iter().map(|c| c.1).sum::<f64>()).max(0.0);
        let _ = write!(
            line,
            "{} pour toute la maison (compteur général, ce n'est pas un appareil)",
            watts(total)
        );
        if !circuits.is_empty() {
            let parts: Vec<String> = circuits
                .iter()
                .map(|(n, w)| format!("{n} {}", watts(*w)))
                .collect();
            let _ = write!(
                line,
                " ; circuits mesurés : {} ; le reste de la maison (non mesuré à part) : {}",
                parts.join(", "),
                watts(rest)
            );
        }
    } else {
        line.push_str("inconnue");
    }
    let appliances: Vec<String> = s
        .meters
        .iter()
        .filter(|m| m.role == moli_energy::Role::Appliance)
        .filter_map(|m| {
            m.power_w.map(|w| {
                // An estimated light says so (« ≈ »).
                let about = if m.estimated { "≈ " } else { "" };
                format!("{} {about}{}", m.name, watts(w.max(0.0)))
            })
        })
        .collect();
    if !appliances.is_empty() {
        let _ = write!(
            line,
            " ; appareils mesurés à la prise (compris dans ce qui précède) : {}",
            appliances.join(", ")
        );
    }
    if let Some(live) = &s.live
        && let Some(v) = live.value
    {
        let unit = live.unit.as_deref().unwrap_or("W");
        let _ = write!(
            line,
            ". Au compteur du fournisseur : {} {unit} (abonnement : {} {unit})",
            v.round(),
            live.max.round()
        );
    }
    if let Some(period) = &s.period {
        let hc = period.to_lowercase().contains("hc");
        let _ = write!(
            line,
            ". Tarif : heures {}",
            if hc { "creuses" } else { "pleines" }
        );
        if let Some(price) = s.price_now {
            let _ = write!(line, " ({price:.3} {}/kWh)", s.currency);
        }
    }
    let cost = |a: &moli_energy::Amount| {
        a.cost
            .map(|c| format!(", {c:.2} {}", s.currency))
            .unwrap_or_default()
    };
    let _ = write!(
        line,
        ". Aujourd'hui : {:.1} kWh{} ; hier : {:.1} kWh{} ; ce mois : {:.0} kWh{}.",
        s.today.total.kwh,
        cost(&s.today.total),
        s.yesterday.total.kwh,
        cost(&s.yesterday.total),
        s.month.total.kwh,
        cost(&s.month.total),
    );
    line
}

/// The tools offered to the model (lib.rs).
pub(crate) fn ambiance_tool() -> Json {
    let ids: Vec<&str> = moli_ambiance::AMBIANCES.iter().map(|a| a.id).collect();
    json!({
        "type": "function",
        "function": {
            "name": "ambiance",
            "description": format!("Met plusieurs lumières d'un coup (une pièce : donne ses lampes, ou le groupe de la pièce) dans une ambiance, une couleur ou un blanc. Ambiances : {}. Les lampes qui ne savent pas suivre sont laissées telles quelles.", ids.join(", ")),
            "parameters": {
                "type": "object",
                "properties": {
                    "lights": { "type": "array", "items": { "type": "string" }, "description": "ids des lampes (ou du groupe de la pièce)" },
                    "ambiance": { "type": "string", "enum": ids },
                    "color": { "type": "string", "description": "une couleur #rrggbb pour toutes les lampes couleur" },
                    "white": { "type": "integer", "description": "un blanc en kelvins (2200 chaud, 4000 neutre, 6500 froid)" }
                },
                "required": ["lights"]
            }
        }
    })
}

pub(crate) fn tools() -> Json {
    json!([
        {
            "type": "function",
            "function": {
                "name": "automation",
                "description": "Crée une automatisation, en brouillon, à partir d'une demande : « quand… alors… », « tous les soirs à… », « préviens-moi si… ». Un adulte la valide (bouton sur la carte) avant qu'elle ne tourne. Passe la demande complète, en français.",
                "parameters": {
                    "type": "object",
                    "properties": { "request": { "type": "string" } },
                    "required": ["request"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "show",
                "description": "Choisit ce que l'écran affiche sous ta réponse, dans l'ordre. device : un appareil (lumière, prise, clim, caméra, télé, enceinte, capteur… l'écran choisit la bonne commande). room : une pièce entière. energy : la consommation électrique. weather : la météo. remote : la télécommande d'une télé (id de la télé). cameras : toutes les caméras. history : la courbe d'une valeur (point = id/clé, hours).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "cards": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "kind": { "type": "string", "enum": CARD_KINDS },
                                    "id": { "type": "string", "description": "Id d'appareil (device, remote)." },
                                    "room": { "type": "string", "description": "Nom de pièce (room)." },
                                    "point": { "type": "string", "description": "id/clé (history)." },
                                    "hours": { "type": "number", "description": "Durée de la courbe (history), 24 par défaut." },
                                    "title": { "type": "string", "description": "Titre court facultatif." }
                                },
                                "required": ["kind"]
                            }
                        }
                    },
                    "required": ["cards"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "set",
                "description": "Change une valeur modifiable (✎) : allumer, éteindre, régler, appuyer sur une touche. Le résultat dit si c'est fait, en attente d'un adulte, ou en échec.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "point": { "type": "string", "description": "<id de l'appareil>/<clé>, par ex. hue:abc/on_off" },
                        "value": { "anyOf": [{ "type": "boolean" }, { "type": "number" }, { "type": "string" }] }
                    },
                    "required": ["point", "value"]
                }
            }
        },
        ambiance_tool(),
        {
            "type": "function",
            "function": {
                "name": "get_device",
                "description": "Tout le détail d'un appareil : chaque valeur, son type, ses choix ou bornes, son unité, si elle est modifiable.",
                "parameters": {
                    "type": "object",
                    "properties": { "device": { "type": "string" } },
                    "required": ["device"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "energy",
                "description": "Consommation et coût de l'électricité. Sans argument : en ce moment, aujourd'hui, hier, ce mois (et sa projection), le mois dernier, par compteur et circuit. Avec step (hour, day, month) : une série.",
                "parameters": {
                    "type": "object",
                    "properties": { "step": { "type": "string", "enum": ["hour", "day", "month"] } }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "history",
                "description": "Ce qu'une valeur a fait sur les dernières heures (changements, minimum, maximum…). Pour « quand la porte s'est-elle ouverte ? », « quelle température cette nuit ? ».",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "point": { "type": "string", "description": "<id de l'appareil>/<clé>" },
                        "hours": { "type": "number" }
                    },
                    "required": ["point"]
                }
            }
        }
    ])
}

/// What the drafting model is told (auto.rs `draft_with`).
pub(crate) fn builder_prompt(date: &str, protected: &str, inventory: &str, meters: &str) -> String {
    format!(
        "{GUIDE}\n\nMaintenant : {}.\nPièces protégées (on y dort) : {}.\n\nInventaire (pièce → appareils : id · nom · clé=valeur ; ✎ = modifiable ; le point d'une valeur s'écrit <id>/<clé>) :\n{}\n{}",
        date, protected, inventory, meters,
    )
}

/// What writes a message for an automation (auto.rs `compose`).
pub(crate) fn compose_prompt(date: &str, power: &str, inventory: &str) -> String {
    format!(
        "Tu es Moli, l'assistant de la maison. Tu écris un message pour la famille : français, chaleureux, court (80 mots au plus), naturel à lire à voix haute, sans liste ni markdown, sans jargon. N'invente aucun chiffre : seulement ceux ci-dessous.\n\nMaintenant : {}.\n{power}\n\nÉtat de la maison :\n{}",
        date, inventory,
    )
}

pub(crate) fn request_message(request: &str, current: Option<&Automation>) -> String {
    let Some(a) = current else {
        return format!("Demande : {request}");
    };
    let flat: Vec<Json> = a
        .graph
        .nodes
        .iter()
        .map(|n| json!({ "id": n.id, "step": n.step }))
        .collect();
    format!(
        "Voici l'automatisation actuelle « {} » (mode {:?}) à modifier :
nœuds {}
liens {}

Garde ce qui n'est pas concerné. Demande : {request}",
        a.name,
        a.mode,
        Json::Array(flat),
        serde_json::to_string(&a.graph.edges).unwrap_or_default(),
    )
}

/// The electricity meters by their meaning (auto.rs `meters`).
pub(crate) fn meters(s: &moli_energy::Summary) -> String {
    let mut out = String::from("Électricité (pour « la consommation ») :\n");
    for m in &s.meters {
        let role = match m.role {
            moli_energy::Role::Total => "toute la maison",
            moli_energy::Role::Grid => "compteur Linky, index au kWh",
            moli_energy::Role::Circuit => "un circuit",
            moli_energy::Role::Appliance => "un appareil (prise mesurée)",
        };
        let _ = match &m.power {
            Some(power) => writeln!(out, "- {} ({role}) : puissance en W = {power}", m.name),
            None => writeln!(out, "- {} ({role}) : énergie = {}", m.name, m.point),
        };
    }
    out
}

pub(crate) fn unreadable(last_problems: &str) -> String {
    format!("Ce brouillon est illisible : {last_problems}. Corrige.")
}

pub(crate) fn refused(last_problems: &str) -> String {
    format!("Le vérificateur refuse : {last_problems}. Corrige le graphe.")
}

pub(crate) fn unusable(last_problems: &str) -> String {
    format!("brouillon inutilisable : {last_problems}")
}

/// What Moli is asked for a Home Assistant automation (import.rs `translate`).
pub(crate) fn import_request(item: &HaAutomation, yaml: &str, entities: &str) -> String {
    format!(
        "Traduis cette automatisation de Home Assistant en automatisme Moli, au plus fidèle.\n\
             Retrouve les appareils par leur nom et leur pièce dans l'inventaire (les identifiants HA ne servent qu'à les reconnaître).\n\
             Une annonce vocale sur le Sonos (script.announce_sonos, tts) → notify avec les canaux maison et voix, même texte. Une notification sur un téléphone (notify.mobile_app_*) → notify avec le canal telephone (l'appli Moli). Ce qui n'existe pas encore dans Moli : Alexa, listes, compteurs, modes de la maison, scripts, gabarits Jinja → au plus proche ou omis. Dis en une phrase dans note ce qui manque ou a été adapté.\n\
             Un « for » de déclencheur HA = for_s sur le déclencheur : n'ajoute pas d'attente en plus.\n\
             Les gabarits Jinja ({{{{ … }}}}) ne sont pas compris par Moli : remplace-les par un texte fixe ou par {{{{<point>}}}}.\n\
             Nom : {}\nDescription : {}\nMode HA : {}\n\nYAML :\n{yaml}\n\nEntités et appareils HA cités : {entities}",
        item.alias,
        item.description,
        item.mode.as_deref().unwrap_or("single"),
    )
}

pub(crate) fn note(item: &HaAutomation, translated: Option<&str>) -> String {
    let alias = tidy_name(&item.alias);
    let id: Option<String> = item
        .ha_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(|id| id.chars().take(64).collect());
    let mut note = if item.off_in_ha {
        "⚠ Désactivé dans HA (tu l'avais coupé) : ne le valider que si tu le veux de nouveau. "
            .to_owned()
    } else {
        String::new()
    };
    let _ = match id {
        Some(id) => write!(
            note,
            "Importé de Home Assistant (automation « {alias} », id {id})."
        ),
        None => write!(note, "Importé de Home Assistant (automation « {alias} »)."),
    };
    note.push_str(
        " Quand tu valides celui-ci, désactive l'original dans HA, sinon les deux agiront.",
    );
    if let Some(t) = translated.map(str::trim).filter(|t| !t.is_empty()) {
        note.push(' ');
        note.push_str(t);
    }
    note
}

pub(crate) fn summary(done: usize, failed: usize, left: usize) -> String {
    let s = if done > 1 { "s" } else { "" };
    let mut message = format!(
        "{done} automatisme{s} traduit{s} en brouillons, à revoir et valider dans Automatismes."
    );
    if failed > 0 {
        let _ = write!(message, " {failed} n'ont pas pu l'être.");
    }
    if left > 0 {
        let _ = write!(
            message,
            " {left} en attente : Moli était trop occupé, relance l'import plus tard (les déjà importés sont sautés)."
        );
    }
    message
}

/// The tones of the voice (settings.rs).
pub(crate) const STYLES: [(&str, &str, &str); 3] = [
    (
        "enjoue",
        "Enjoué",
        "Ton enjoué et souriant, plein d'énergie, comme un ami content de rendre service. Intonation vivante, débit vif, français naturel de France.",
    ),
    (
        "pose",
        "Posé",
        "Ton calme et chaleureux, posé et rassurant, débit naturel, français naturel de France.",
    ),
    (
        "doux",
        "Doux",
        "Ton doux et feutré, presque à voix basse, comme le soir quand la maison s'endort. Débit lent, français naturel de France.",
    ),
];

/// How a sentence starts when it is an offer / points at the screen (lib.rs `trim_filler`).
pub(crate) const OFFERS: [&str; 8] = [
    "tu veux",
    "veux-tu",
    "voulez-vous",
    "vous voulez",
    "souhaites-tu",
    "tu souhaites",
    "aimerais-tu",
    "dois-je",
];
pub(crate) const POINTERS: [&str; 8] = [
    "voici",
    "voilà",
    "je te montre",
    "je vous montre",
    "je montre",
    "j'affiche",
    "je t'affiche",
    "l'écran",
];

/// Fragments of a question (lib.rs `guess_cards`).
pub(crate) const GUESS_ENERGY: [&str; 8] = [
    "consomm",
    "conso ",
    "electricit",
    "energie",
    "kwh",
    "watt",
    "facture",
    "courant",
];
pub(crate) const GUESS_WEATHER: [&str; 7] = [
    "meteo",
    "pleu",
    "pluie",
    "soleil",
    " vent",
    "dehors",
    "quel temps",
];
pub(crate) const GUESS_CAMERAS: [&str; 5] = [
    "camera",
    "sonne",
    "devant la maison",
    "qui est passe",
    "portail",
];
