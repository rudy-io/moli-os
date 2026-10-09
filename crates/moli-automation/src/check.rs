//! Before anything runs: is the graph sound, and what does it do, in one
//! sentence (in the house's language) anyone at home can read.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use moli_core::{Device, DeviceId, Kind, Label, PointId, PointSpec, Value};
use moli_i18n::tr;
use moli_runtime::Hub;
use serde::Serialize;
use serde_json::Value as Json;

use crate::model::{Channel, DayNight, Graph, Node, Op, Rule, Step, SunEvent, minutes_of};
use crate::{decimal, fold, join_last};

pub const MAX_NODES: usize = 60;
pub const MAX_EDGES: usize = 120;
const MAX_WAIT_S: u64 = 24 * 3600;

#[derive(Clone, Debug, Serialize)]
pub struct Check {
    /// What it does, in the house's language.
    pub summary: String,
    pub problems: Vec<Problem>,
    /// Protected rooms its actions reach: said out loud before approval.
    pub protected: Vec<String>,
}

impl Check {
    pub fn ok(&self) -> bool {
        !self.problems.iter().any(|p| p.level == Level::Error)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Problem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    pub level: Level,
    pub message: String,
}

impl Problem {
    /// A step no trigger leads to: only a warning, but a forgotten link
    /// worth a second look. (The message is words, not a code: ask here
    /// rather than comparing it.)
    pub fn is_unreached(&self) -> bool {
        self.level == Level::Warning && self.message == tr!("moteur.verif.jamais_atteint")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Error,
    Warning,
}

/// What the checker needs to know beyond the hub.
// Independent abilities of the house (not a state in disguise): one flag each.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Abilities {
    pub telegram: bool,
    pub writer: bool,
    /// At least one speaker can announce.
    pub voice: bool,
    /// At least one phone can be notified (the Moli app).
    pub phone: bool,
}

pub fn check(graph: &Graph, hub: &Hub, can: Abilities) -> Check {
    let mut problems = Vec::new();
    let mut err = |node: Option<&str>, message: String| {
        problems.push(Problem {
            node: node.map(str::to_owned),
            level: Level::Error,
            message,
        });
    };
    if graph.nodes.len() > MAX_NODES {
        err(
            None,
            tr!(
                "moteur.verif.trop_noeuds",
                n = graph.nodes.len(),
                max = MAX_NODES
            ),
        );
    }
    let ids: HashSet<&str> = graph.nodes.iter().map(|n| n.id.as_str()).collect();
    if ids.len() != graph.nodes.len() {
        err(None, tr!("moteur.verif.id_double"));
    }
    if graph.triggers().next().is_none() {
        err(None, tr!("moteur.verif.sans_declencheur"));
    }
    for e in &graph.edges {
        match (graph.node(&e.from), graph.node(&e.to)) {
            (Some(from), Some(to)) => {
                if !from.step.ports().contains(&e.port.as_str()) {
                    err(
                        Some(&from.id),
                        tr!("moteur.verif.sortie_inconnue", port = e.port),
                    );
                }
                if to.step.is_trigger() {
                    err(Some(&to.id), tr!("moteur.verif.declencheur_sans_entree"));
                }
            }
            _ => err(None, tr!("moteur.verif.lien_orphelin")),
        }
    }
    let mut seen = HashSet::new();
    if graph
        .edges
        .iter()
        .any(|e| !seen.insert((&e.from, &e.port, &e.to)))
    {
        err(None, tr!("moteur.verif.liens_doubles"));
    }
    if graph.edges.len() > MAX_EDGES {
        err(
            None,
            tr!(
                "moteur.verif.trop_liens",
                n = graph.edges.len(),
                max = MAX_EDGES
            ),
        );
    }
    if has_cycle(graph) {
        err(None, tr!("moteur.verif.cycle"));
    }
    let mut protected = Vec::new();
    for node in &graph.nodes {
        check_node(node, hub, can, &mut problems, &mut protected);
    }
    let reached = reachable(graph);
    for node in graph.nodes.iter().filter(|n| !n.step.is_trigger()) {
        if !reached.contains(node.id.as_str()) {
            problems.push(Problem {
                node: Some(node.id.clone()),
                level: Level::Warning,
                message: tr!("moteur.verif.jamais_atteint"),
            });
        }
    }
    for t in graph.triggers() {
        if graph.edges.iter().all(|e| e.from != t.id) {
            problems.push(Problem {
                node: Some(t.id.clone()),
                level: Level::Warning,
                message: tr!("moteur.verif.declencheur_inutile"),
            });
        }
    }
    protected.sort();
    protected.dedup();
    Check {
        summary: describe(graph, hub),
        problems,
        protected,
    }
}

/// Where one node's problems go.
struct Ctx<'a> {
    hub: &'a Hub,
    node: String,
    problems: &'a mut Vec<Problem>,
    protected: &'a mut Vec<String>,
}

impl Ctx<'_> {
    fn push(&mut self, level: Level, message: String) {
        self.problems.push(Problem {
            node: Some(self.node.clone()),
            level,
            message,
        });
    }

    /// The point's description, or a problem said once.
    fn need(&mut self, point: &str, writable: bool) -> Option<PointSpec> {
        let Some((device, key)) = PointId::from(point).split().map(|(d, k)| (d, k.to_owned()))
        else {
            self.push(Level::Error, tr!("moteur.verif.choisir_valeur"));
            return None;
        };
        let Some((found, label)) = self.hub.describe(&device) else {
            self.push(
                Level::Error,
                tr!("moteur.verif.appareil_introuvable", appareil = device),
            );
            return None;
        };
        let Some(spec) = found.points.iter().find(|p| *p.key == *key).cloned() else {
            self.push(
                Level::Error,
                tr!(
                    "moteur.verif.valeur_absente",
                    appareil = name(&found, &label),
                    valeur = key
                ),
            );
            return None;
        };
        if writable {
            if !spec.access.write {
                self.push(
                    Level::Error,
                    tr!("moteur.verif.lecture_seule", nom = spec.label),
                );
            }
            if let Some(room) = self.hub.protected_room(&device) {
                self.protected.push(room);
            }
        }
        Some(spec)
    }

    fn rule(&mut self, rule: &Rule) {
        match rule {
            Rule::State { point, op, value } => {
                if let Some(spec) = self.need(point, false)
                    && matches!(op, Op::Gt | Op::Ge | Op::Lt | Op::Le)
                    && (!matches!(spec.kind, Kind::Numeric { .. }) || value.as_f64().is_none())
                {
                    self.push(Level::Error, tr!("moteur.verif.comparaison_nombres"));
                }
            }
            Rule::Time {
                after,
                before,
                days,
            } => {
                for t in [after, before].into_iter().flatten() {
                    if minutes_of(t).is_none() {
                        self.push(Level::Error, tr!("moteur.verif.heure_illisible", heure = t));
                    }
                }
                if after.is_none() && before.is_none() && days.is_empty() {
                    self.push(Level::Error, tr!("moteur.verif.plage_vide"));
                }
                self.days(days);
            }
            Rule::Sun { .. } => {}
        }
    }

    fn days(&mut self, days: &[u8]) {
        if days.iter().any(|d| !(1..=7).contains(d)) {
            self.push(Level::Error, tr!("moteur.verif.jours_invalides"));
        }
    }
}

#[allow(clippy::too_many_lines)] // one arm per kind of node
fn check_node(
    node: &Node,
    hub: &Hub,
    can: Abilities,
    problems: &mut Vec<Problem>,
    protected: &mut Vec<String>,
) {
    let mut c = Ctx {
        hub,
        node: node.id.clone(),
        problems,
        protected,
    };
    match &node.step {
        Step::WhenState { point, .. } => {
            c.need(point, false);
        }
        Step::WhenThreshold {
            point,
            above,
            below,
            ..
        } => {
            if let Some(spec) = c.need(point, false)
                && !matches!(spec.kind, Kind::Numeric { .. })
            {
                c.push(Level::Error, tr!("moteur.verif.seuil_nombre"));
            }
            if above.is_none() && below.is_none() {
                c.push(Level::Error, tr!("moteur.verif.seuil_manquant"));
            }
        }
        Step::AtTime { at, days } => {
            if minutes_of(at).is_none() {
                c.push(
                    Level::Error,
                    tr!("moteur.verif.heure_illisible", heure = at),
                );
            }
            c.days(days);
        }
        Step::AtSun {
            offset_min, days, ..
        } => {
            if offset_min.abs() > 6 * 60 {
                c.push(Level::Error, tr!("moteur.verif.decalage_soleil"));
            }
            c.days(days);
        }
        Step::Every { minutes } => {
            if *minutes == 0 || *minutes > 24 * 60 {
                c.push(Level::Error, tr!("moteur.verif.intervalle"));
            }
        }
        Step::OnStart | Step::Manual => {}
        Step::If { rules, .. } => {
            if rules.is_empty() {
                c.push(Level::Error, tr!("moteur.verif.condition_manquante"));
            }
            for rule in rules {
                c.rule(rule);
            }
        }
        Step::Set { point, value } => {
            if let Some(spec) = c.need(point, true)
                && let Err(e) = moli_core::validate(&spec.kind, &Value::from_json(value))
            {
                c.push(
                    Level::Error,
                    tr!("moteur.verif.valeur_refusee", nom = spec.label, erreur = e),
                );
            }
        }
        Step::Toggle { point } => {
            if let Some(spec) = c.need(point, true)
                && !toggles(&spec)
            {
                c.push(
                    Level::Error,
                    tr!("moteur.verif.pas_bascule", nom = spec.label),
                );
            }
        }
        Step::Wait { seconds } => {
            if *seconds == 0 || *seconds > MAX_WAIT_S {
                c.push(Level::Error, tr!("moteur.verif.attente"));
            }
        }
        Step::WaitFor { rule, timeout_s } => {
            c.rule(rule);
            if *timeout_s == 0 || *timeout_s > MAX_WAIT_S {
                c.push(Level::Error, tr!("moteur.verif.delai"));
            }
        }
        Step::Notify {
            message, channels, ..
        } => {
            if message.trim().is_empty() {
                c.push(Level::Error, tr!("moteur.verif.message_vide"));
            }
            if channels.is_empty() {
                c.push(Level::Error, tr!("moteur.verif.canal_manquant"));
            }
            if channels.contains(&Channel::Telegram) && !can.telegram {
                c.push(Level::Warning, tr!("moteur.verif.telegram"));
            }
            if channels.contains(&Channel::Telephone) && !can.phone {
                c.push(Level::Warning, tr!("moteur.verif.telephone"));
            }
            if channels.contains(&Channel::Voix) && !can.voice {
                c.push(Level::Warning, tr!("moteur.verif.voix"));
            }
        }
        Step::Write { prompt } => {
            if prompt.trim().is_empty() {
                c.push(Level::Error, tr!("moteur.verif.ecrit_vide"));
            }
            if !can.writer {
                c.push(Level::Warning, tr!("moteur.verif.moli_absent"));
            }
        }
    }
}

/// On/off points: booleans, or ON/OFF lists.
pub(crate) fn toggles(spec: &PointSpec) -> bool {
    match &spec.kind {
        Kind::Binary => true,
        Kind::Enum { values } => {
            values.iter().any(|v| v.eq_ignore_ascii_case("on"))
                && values.iter().any(|v| v.eq_ignore_ascii_case("off"))
        }
        _ => false,
    }
}

fn has_cycle(graph: &Graph) -> bool {
    // Kahn: what cannot be ordered sits on a cycle.
    let mut indegree: HashMap<&str, usize> =
        graph.nodes.iter().map(|n| (n.id.as_str(), 0)).collect();
    for e in &graph.edges {
        if let Some(d) = indegree.get_mut(e.to.as_str()) {
            *d += 1;
        }
    }
    let mut ready: Vec<&str> = indegree
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(n, _)| *n)
        .collect();
    let mut seen = 0;
    while let Some(n) = ready.pop() {
        seen += 1;
        for e in graph.edges.iter().filter(|e| e.from == n) {
            if let Some(d) = indegree.get_mut(e.to.as_str()) {
                *d -= 1;
                if *d == 0 {
                    ready.push(e.to.as_str());
                }
            }
        }
    }
    seen < indegree.len()
}

fn reachable(graph: &Graph) -> HashSet<&str> {
    let mut seen = HashSet::new();
    let mut todo: Vec<&str> = graph.triggers().map(|n| n.id.as_str()).collect();
    while let Some(n) = todo.pop() {
        if seen.insert(n) {
            todo.extend(
                graph
                    .edges
                    .iter()
                    .filter(|e| e.from == n)
                    .map(|e| e.to.as_str()),
            );
        }
    }
    seen
}

// ---- the sentence ------------------------------------------------------------

fn name(device: &Device, label: &Label) -> String {
    label
        .name
        .clone()
        .unwrap_or_else(|| device.native_name.to_string())
}

struct Said {
    device: String,
    spec: Option<PointSpec>,
}

fn said(hub: &Hub, point: &str) -> Said {
    let Some((device, key)) = PointId::from(point).split().map(|(d, k)| (d, k.to_owned())) else {
        return Said {
            device: "?".into(),
            spec: None,
        };
    };
    match hub.describe(&DeviceId::from(device.as_str())) {
        Some((found, label)) => Said {
            device: name(&found, &label),
            spec: found.points.iter().find(|p| *p.key == *key).cloned(),
        },
        None => Said {
            device: device.to_string(),
            spec: None,
        },
    }
}

fn is_power(spec: Option<&PointSpec>) -> bool {
    spec.is_some_and(|s| {
        toggles(s)
            && (s.semantic == moli_core::Semantic::OnOff
                || ["on", "state", "power", "switch", "switch_1", "switch_2"].contains(&&*s.key))
    })
}

fn truthy(v: &Json) -> Option<bool> {
    match v {
        Json::Bool(b) => Some(*b),
        Json::String(s) if s.eq_ignore_ascii_case("on") => Some(true),
        Json::String(s) if s.eq_ignore_ascii_case("off") => Some(false),
        _ => None,
    }
}

fn value_words(spec: Option<&PointSpec>, v: &Json) -> String {
    let unit = spec
        .and_then(|s| s.unit.as_ref())
        .map(|u| format!(" {}", u.symbol()))
        .unwrap_or_default();
    match v {
        Json::Bool(b) => tr!(if *b {
            "moteur.valeur.vrai"
        } else {
            "moteur.valeur.faux"
        }),
        Json::Number(n) => format!("{}{unit}", decimal(&n.to_string())),
        Json::String(s) => tr!("moteur.valeur.texte", texte = s),
        other => other.to_string(),
    }
}

fn duration(s: u64) -> String {
    match s {
        0 => String::new(),
        s if s % 3600 == 0 => tr!("moteur.duree.h", heures = s / 3600),
        s if s >= 3600 => tr!(
            "moteur.duree.h_min",
            heures = s / 3600,
            minutes = format!("{:02}", (s % 3600) / 60)
        ),
        s if s % 60 == 0 => tr!("moteur.duree.min", minutes = s / 60),
        s if s > 60 => tr!("moteur.duree.min_s", minutes = s / 60, secondes = s % 60),
        s => tr!("moteur.duree.s", secondes = s),
    }
}

fn days_words(days: &[u8]) -> String {
    const NAMES: [&str; 7] = [
        "moteur.jour.lundi",
        "moteur.jour.mardi",
        "moteur.jour.mercredi",
        "moteur.jour.jeudi",
        "moteur.jour.vendredi",
        "moteur.jour.samedi",
        "moteur.jour.dimanche",
    ];
    let mut d: Vec<u8> = days
        .iter()
        .copied()
        .filter(|d| (1..=7).contains(d))
        .collect();
    d.sort_unstable();
    d.dedup();
    match d.as_slice() {
        [] | [1, 2, 3, 4, 5, 6, 7] => tr!("moteur.jour.chaque"),
        [1, 2, 3, 4, 5] => tr!("moteur.jour.semaine"),
        [6, 7] => tr!("moteur.jour.weekend"),
        list => {
            let names: Vec<String> = list
                .iter()
                .map(|d| tr!(NAMES[usize::from(*d - 1)]))
                .collect();
            tr!(
                "moteur.jour.liste",
                jours = join_last(&names, "moteur.jour.fin")
            )
        }
    }
}

/// "07:30" as the house reads a time: `07 h 30` or `07:30`.
fn clock_words(at: &str) -> String {
    match at.split_once(':') {
        Some((hour, "00")) => tr!("moteur.heure.pile", heure = hour),
        Some((hour, minute)) => tr!("moteur.heure.mn", heure = hour, minute = minute),
        None => at.to_owned(),
    }
}

/// What a point changing to a value means, in words.
fn change_words(key: &str, spec: Option<&PointSpec>, to: Option<&Json>) -> String {
    let t = to.and_then(truthy);
    let known = match (key, t) {
        ("contact", Some(false)) | ("doorcontact_state", Some(true)) => "moteur.changement.ouvre",
        ("contact", Some(true)) | ("doorcontact_state", Some(false)) => "moteur.changement.ferme",
        ("doorbell", Some(true)) => "moteur.changement.sonne",
        ("person", Some(true)) => "moteur.changement.personne",
        ("vehicle", Some(true)) => "moteur.changement.vehicule",
        ("animal", Some(true)) => "moteur.changement.animal",
        ("package", Some(true)) => "moteur.changement.colis",
        ("motion" | "occupancy" | "presence", Some(true)) => "moteur.changement.mouvement",
        ("motion" | "occupancy" | "presence", Some(false)) => "moteur.changement.plus_rien",
        ("smoke", Some(true)) => "moteur.changement.fumee",
        ("water_leak", Some(true)) => "moteur.changement.fuite",
        (_, Some(true)) if is_power(spec) => "moteur.changement.allume",
        (_, Some(false)) if is_power(spec) => "moteur.changement.eteint",
        _ => {
            let label = spec.map_or(key, |s| &*s.label);
            return match to {
                Some(v) => tr!(
                    "moteur.changement.valeur",
                    libelle = label,
                    valeur = value_words(spec, v)
                ),
                None => tr!("moteur.changement.change", libelle = label),
            };
        }
    };
    tr!(known)
}

/// The sun trigger in words: "at sunrise", "10 min before sunset"…
fn sun_words(event: SunEvent, offset_min: i32, days: &[u8]) -> String {
    let what = tr!(if event == SunEvent::Rise {
        "moteur.decrit.soleil_lever"
    } else {
        "moteur.decrit.soleil_coucher"
    });
    let when = match offset_min {
        0 => tr!("moteur.decrit.soleil_pile", evenement = what),
        m if m < 0 => tr!(
            "moteur.decrit.soleil_avant",
            duree = duration(u64::from(m.unsigned_abs()) * 60),
            evenement = what
        ),
        m => tr!(
            "moteur.decrit.soleil_apres",
            duree = duration(u64::from(m.unsigned_abs()) * 60),
            evenement = what
        ),
    };
    if days.is_empty() {
        when
    } else {
        tr!(
            "moteur.decrit.soleil_jours",
            jours = days_words(days),
            quand = when
        )
    }
}

fn trigger_words(step: &Step, hub: &Hub) -> String {
    match step {
        Step::WhenState {
            point, to, for_s, ..
        } => {
            let s = said(hub, point);
            let key = point.rsplit('/').next().unwrap_or_default();
            let change = change_words(key, s.spec.as_ref(), to.as_ref());
            if *for_s > 0 {
                tr!(
                    "moteur.decrit.quand_etat_depuis",
                    appareil = s.device,
                    changement = change,
                    duree = duration(*for_s)
                )
            } else {
                tr!(
                    "moteur.decrit.quand_etat",
                    appareil = s.device,
                    changement = change
                )
            }
        }
        Step::WhenThreshold {
            point,
            above,
            below,
            for_s,
        } => {
            let s = said(hub, point);
            let label = s.spec.as_ref().map_or_else(
                || tr!("moteur.decrit.seuil_defaut"),
                |p| p.label.to_lowercase(),
            );
            let unit = s
                .spec
                .as_ref()
                .and_then(|p| p.unit.as_ref())
                .map(|u| format!(" {}", u.symbol()))
                .unwrap_or_default();
            let fmt = |v: f64| format!("{}{unit}", decimal(&v.to_string()));
            let what = match (above, below) {
                (Some(a), Some(b)) => {
                    tr!("moteur.decrit.seuil_entre", a = fmt(*a), b = fmt(*b))
                }
                (Some(a), None) => tr!("moteur.decrit.seuil_depasse", a = fmt(*a)),
                (None, Some(b)) => tr!("moteur.decrit.seuil_descend", b = fmt(*b)),
                (None, None) => tr!("moteur.decrit.seuil_change"),
            };
            if *for_s > 0 {
                tr!(
                    "moteur.decrit.quand_seuil_pendant",
                    libelle = label,
                    appareil = s.device,
                    evenement = what,
                    duree = duration(*for_s)
                )
            } else {
                tr!(
                    "moteur.decrit.quand_seuil",
                    libelle = label,
                    appareil = s.device,
                    evenement = what
                )
            }
        }
        Step::AtTime { at, days } => tr!(
            "moteur.decrit.heure",
            jours = days_words(days),
            heure = clock_words(at)
        ),
        Step::AtSun {
            event,
            offset_min,
            days,
        } => sun_words(*event, *offset_min, days),
        Step::Every { minutes } => tr!(
            "moteur.decrit.toutes_les",
            duree = duration(u64::from(*minutes) * 60)
        ),
        Step::OnStart => tr!("moteur.decrit.demarrage"),
        Step::Manual => tr!("moteur.decrit.manuel"),
        _ => String::new(),
    }
}

pub(crate) fn rule_words(rule: &Rule, hub: &Hub) -> String {
    match rule {
        Rule::State { point, op, value } => {
            let s = said(hub, point);
            let key = point.rsplit('/').next().unwrap_or_default();
            if (*op == Op::Eq || *op == Op::Ne)
                && let Some(b) = truthy(value)
            {
                let on = if *op == Op::Eq { b } else { !b };
                if is_power(s.spec.as_ref()) {
                    return tr!(
                        if on {
                            "moteur.regle.allume"
                        } else {
                            "moteur.regle.eteint"
                        },
                        appareil = s.device
                    );
                }
                if key == "contact" {
                    return tr!(
                        if on {
                            "moteur.regle.ferme"
                        } else {
                            "moteur.regle.ouvert"
                        },
                        appareil = s.device
                    );
                }
            }
            let label = s
                .spec
                .as_ref()
                .map_or(key.to_owned(), |p| p.label.to_lowercase());
            let phrase = match op {
                Op::Eq => "moteur.regle.egal",
                Op::Ne => "moteur.regle.different",
                Op::Gt => "moteur.regle.plus",
                Op::Ge => "moteur.regle.au_moins",
                Op::Lt => "moteur.regle.moins",
                Op::Le => "moteur.regle.au_plus",
            };
            tr!(
                phrase,
                libelle = label,
                appareil = s.device,
                valeur = value_words(s.spec.as_ref(), value)
            )
        }
        Rule::Time {
            after,
            before,
            days,
        } => {
            let hours = match (after, before) {
                (Some(a), Some(b)) => tr!("moteur.regle.entre", a = a, b = b),
                (Some(a), None) => tr!("moteur.regle.apres", a = a),
                (None, Some(b)) => tr!("moteur.regle.avant", b = b),
                (None, None) => String::new(),
            };
            match (hours.is_empty(), days.is_empty()) {
                (false, true) => tr!("moteur.regle.heures", heures = hours),
                (true, false) => tr!("moteur.regle.jours", jours = days_words(days)),
                _ => tr!(
                    "moteur.regle.jours_heures",
                    jours = days_words(days),
                    heures = hours
                ),
            }
        }
        Rule::Sun { is } => tr!(if *is == DayNight::Day {
            "moteur.regle.jour"
        } else {
            "moteur.regle.nuit"
        }),
    }
}

fn action_words(step: &Step, hub: &Hub) -> String {
    match step {
        Step::Set { point, value } => {
            let s = said(hub, point);
            let key = point.rsplit('/').next().unwrap_or_default();
            if is_power(s.spec.as_ref())
                && let Some(b) = truthy(value)
            {
                return tr!(
                    if b {
                        "moteur.action.allumer"
                    } else {
                        "moteur.action.eteindre"
                    },
                    appareil = s.device
                );
            }
            if key == "key" {
                return tr!(
                    "moteur.action.appuyer",
                    valeur = value_words(None, value),
                    appareil = s.device
                );
            }
            let label = s
                .spec
                .as_ref()
                .map_or(key.to_owned(), |p| p.label.to_lowercase());
            tr!(
                "moteur.action.regler",
                libelle = label,
                appareil = s.device,
                valeur = value_words(s.spec.as_ref(), value)
            )
        }
        Step::Toggle { point } => {
            tr!("moteur.action.basculer", appareil = said(hub, point).device)
        }
        Step::Wait { seconds } => tr!("moteur.action.attendre", duree = duration(*seconds)),
        Step::WaitFor { rule, timeout_s } => tr!(
            "moteur.action.attendre_que",
            regle = rule_words(rule, hub),
            duree = duration(*timeout_s)
        ),
        Step::Notify {
            message, channels, ..
        } => {
            let mut places = Vec::new();
            if channels.contains(&Channel::Maison) {
                places.push(tr!("moteur.lieu.maison"));
            }
            if channels.contains(&Channel::Voix) {
                places.push(tr!("moteur.lieu.voix"));
            }
            if channels.contains(&Channel::Telegram) {
                places.push("Telegram".to_owned());
            }
            if channels.contains(&Channel::Telephone) {
                places.push(tr!("moteur.lieu.telephone"));
            }
            let to = if places.is_empty() {
                tr!("moteur.lieu.maison")
            } else {
                join_last(&places, "moteur.liste.et")
            };
            let short: String = message.chars().take(60).collect();
            let more = if message.chars().count() > 60 {
                "…"
            } else {
                ""
            };
            tr!(
                "moteur.action.prevenir",
                lieux = to,
                message = format!("{short}{more}")
            )
        }
        Step::Write { .. } => tr!("moteur.action.ecrire"),
        _ => String::new(),
    }
}

fn chain(
    graph: &Graph,
    from: &str,
    port: &str,
    hub: &Hub,
    depth: usize,
    budget: &Cell<usize>,
) -> String {
    // A graph of duplicate links could make this explode: a budget of
    // steps said, whatever the shape.
    if depth > 12 || budget.get() == 0 {
        return "…".into();
    }
    budget.set(budget.get() - 1);
    let next = graph.next(from, port);
    let parts: Vec<String> = next
        .iter()
        .map(|n| match &n.step {
            Step::If { rules, all } => {
                let cond = fold(
                    rules.iter().map(|r| rule_words(r, hub)),
                    if *all {
                        "moteur.liste.et"
                    } else {
                        "moteur.liste.ou"
                    },
                );
                let yes = chain(graph, &n.id, "yes", hub, depth + 1, budget);
                let no = chain(graph, &n.id, "no", hub, depth + 1, budget);
                match (yes.is_empty(), no.is_empty()) {
                    (false, true) => tr!("moteur.decrit.si", cond = cond, oui = yes),
                    (true, false) => tr!("moteur.decrit.si_pas", cond = cond, non = no),
                    (false, false) => {
                        tr!("moteur.decrit.si_sinon", cond = cond, oui = yes, non = no)
                    }
                    (true, true) => tr!("moteur.decrit.verifier", cond = cond),
                }
            }
            Step::WaitFor { .. } => {
                let ok = chain(graph, &n.id, "ok", hub, depth + 1, budget);
                let late = chain(graph, &n.id, "timeout", hub, depth + 1, budget);
                let mut s = action_words(&n.step, hub);
                if !ok.is_empty() {
                    s = tr!("moteur.decrit.puis", debut = s, suite = ok);
                }
                if !late.is_empty() {
                    s = tr!("moteur.decrit.sinon", debut = s, suite = late);
                }
                s
            }
            step => {
                let here = action_words(step, hub);
                let then = chain(graph, &n.id, "out", hub, depth + 1, budget);
                if then.is_empty() {
                    here
                } else {
                    tr!("moteur.decrit.puis", debut = here, suite = then)
                }
            }
        })
        .collect();
    fold(parts, "moteur.decrit.parallele")
}

pub fn describe(graph: &Graph, hub: &Hub) -> String {
    let budget = Cell::new(200);
    let triggers: Vec<&Node> = graph.triggers().collect();
    if triggers.is_empty() {
        return tr!("moteur.decrit.rien_declenche");
    }
    // Triggers leading to the same steps read as one: « quand A ou quand B ».
    let mut groups: Vec<(Vec<String>, String)> = Vec::new();
    for t in &triggers {
        let body = chain(graph, &t.id, "out", hub, 0, &budget);
        let words = trigger_words(&t.step, hub);
        match groups.iter_mut().find(|(_, b)| *b == body) {
            Some((ws, _)) => {
                if !ws.contains(&words) {
                    ws.push(words);
                }
            }
            None => groups.push((vec![words], body)),
        }
    }
    let sentences: Vec<String> = groups
        .into_iter()
        .map(|(ws, body)| {
            let when = fold(ws, "moteur.liste.ou");
            let s = if body.is_empty() {
                tr!("moteur.decrit.rien_pour_linstant", quand = when)
            } else {
                tr!("moteur.decrit.alors", quand = when, corps = body)
            };
            let mut c = s.chars();
            c.next().map_or_else(String::new, |f| {
                f.to_uppercase().chain(c).collect::<String>() + "."
            })
        })
        .collect();
    sentences.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_naturally() {
        assert_eq!(duration(300), "5 min");
        assert_eq!(duration(90), "1 min 30 s");
        assert_eq!(duration(3600), "1 h");
        assert_eq!(duration(5400), "1 h 30");
        assert_eq!(days_words(&[1, 2, 3, 4, 5]), "en semaine");
        assert_eq!(days_words(&[3, 1]), "le lundi, mercredi");
    }
}
