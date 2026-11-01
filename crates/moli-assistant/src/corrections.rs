//! What a person taught Moli by correcting it: rules of the house (« éteins la lumière du salon » means
//! this order) kept in `assistant-rules.json`, and a journal of every correction
//! (`assistant-corrections.jsonl`) that feeds the next training. Both live in the house's data
//! folder and never leave it.

use std::io::{Error, ErrorKind, Result, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::house::norm;

/// Rules kept at most: the oldest give way.
pub(crate) const MAX_RULES: usize = 500;
/// The corrections journal stops growing past this (bytes).
const MAX_JOURNAL: u64 = 1024 * 1024;

/// What people said is for the house's own user: no one else reads these files.
fn private_file(options: &mut std::fs::OpenOptions) -> &mut std::fs::OpenOptions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
}

/// One tool call of a rule, with real identifiers (never the numbers a prompt happened to use).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct RuleCall {
    pub(crate) tool: String,
    pub(crate) arguments: Json,
}

/// What a rule makes Moli do.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum RuleAction {
    /// A tool call, with real identifiers (never the numbers a prompt happened to use).
    Call { tool: String, arguments: Json },
    /// Several devices chosen together, one call each (all of them, or none).
    Many { calls: Vec<RuleCall> },
    /// « Moli should not have acted »: it says it does nothing.
    Nothing,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Rule {
    pub(crate) id: u64,
    /// The sentence as the person said it.
    pub(crate) said: String,
    /// The sentence, normalized: what a new sentence is compared with.
    pub(crate) key: String,
    pub(crate) action: RuleAction,
    /// When it was taught (milliseconds since the epoch).
    pub(crate) created: u64,
    /// Who taught it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) by: Option<String>,
    pub(crate) enabled: bool,
}

/// A sentence as it is compared: no case, no accents, no punctuation, single spaces.
pub(crate) fn normalize(text: &str) -> String {
    norm(text)
        .replace(|c: char| !c.is_alphanumeric(), " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether a sentence only makes sense with what was said before or with the value a device has now
/// (« éteins-la », « monte un peu », « fais l'inverse »): such a sentence is corrected on the moment
/// and logged for training, but never kept as a rule (it would fire on the wrong device later).
pub(crate) fn contextual(text: &str) -> bool {
    // Pronouns, demonstratives, « again », « the other », relative changes (more, less, up, down) and
    // verbs whose result depends on the state now (switch over, change): in French and in English.
    const WORDS: [&str; 41] = [
        "ca", "lui", "plus", "moins", "encore", "pareil", "aussi", "inverse", "peu", "avant",
        "ici", "cette", "cet", "ces", "autre", "autres", "meme", "bascule", "basculer", "alterne",
        "change", "it", "that", "this", "them", "same", "again", "back", "up", "down", "more",
        "less", "bit", "also", "too", "other", "another", "toggle", "switch", "undo", "reverse",
    ];
    let spaced = norm(text).replace(['\'', '’'], " ");
    let words: Vec<&str> = spaced
        .split(|c: char| !c.is_alphanumeric() && c != '-')
        .filter(|w| !w.is_empty())
        .collect();
    // « -la », « -le », « -les », « -lui » glued to a verb (« éteins-la »).
    if words.iter().any(|w| {
        w.rsplit_once('-')
            .is_some_and(|(_, tail)| matches!(tail, "la" | "le" | "les" | "lui" | "y" | "en"))
    }) {
        return true;
    }
    // A bare article before a noun is no pronoun (« la lumière »): only one that ends the sentence is.
    let last = words.len().saturating_sub(1);
    words.iter().enumerate().any(|(i, w)| match *w {
        "la" | "le" | "les" => i == last,
        w => WORDS.contains(&w),
    })
}

/// A turn a person can still correct: what was asked, what came before, and what each order replaced.
#[derive(Clone, Debug)]
pub(crate) struct Turned {
    pub(crate) question: String,
    /// The exchanges the model was shown before the question, as (role, text).
    pub(crate) context: Vec<(String, String)>,
    /// Each order the turn carried out: what « Défaire » may put back.
    pub(crate) undo: Vec<Undo>,
    /// Who asked (`Turn::asker`): only they undo, correct or see the choices of this turn. A turn
    /// with no asker is nobody's to correct.
    pub(crate) asker: Option<String>,
    pub(crate) at: std::time::Instant,
}

/// An order of a turn: its point, the value it held before, and the value Moli wrote. « Défaire »
/// only puts `before` back while the point still holds `written`: what changed since is someone's.
#[derive(Clone, Debug)]
pub(crate) struct Undo {
    pub(crate) point: moli_core::PointId,
    pub(crate) before: Json,
    pub(crate) written: Json,
}

/// Turns kept per asker (a chatty agent only pushes out its own), for all askers together, and for
/// how long (« Défaire » is for what just happened).
const MAX_TURNS: usize = 50;
const MAX_ALL_TURNS: usize = 500;
pub(crate) const TURN_LIFETIME: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// A number nobody can guess or count to, kept whole by a browser (below 2^52), not 0 and not
/// `taken`: for turns, and for rules (a number never comes back, not even after a restart).
fn random_id(taken: impl Fn(u64) -> bool) -> u64 {
    let random = ring::rand::SystemRandom::new();
    loop {
        let mut bytes = [0_u8; 8];
        let id = if ring::rand::SecureRandom::fill(&random, &mut bytes).is_ok() {
            u64::from_le_bytes(bytes) & ((1 << 52) - 1)
        } else {
            moli_core::now_ms()
        };
        if id != 0 && !taken(id) {
            return id;
        }
    }
}

impl crate::Assistant {
    /// Keeps a turn for a correction; returns its number: random, kept whole by a browser (below 2^52),
    /// so that nobody finds a turn by counting from the clock.
    pub(crate) fn remember_turn(&self, turned: Turned) -> u64 {
        let mut turns = self
            .0
            .turns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        turns.retain(|(_, t)| t.at.elapsed() < TURN_LIFETIME);
        let id = random_id(|id| turns.iter().any(|(i, _)| *i == id));
        let asker = turned.asker.clone();
        turns.push_back((id, turned));
        while turns.iter().filter(|(_, t)| t.asker == asker).count() > MAX_TURNS {
            if let Some(oldest) = turns.iter().position(|(_, t)| t.asker == asker) {
                turns.remove(oldest);
            }
        }
        while turns.len() > MAX_ALL_TURNS {
            turns.pop_front();
        }
        id
    }

    /// The rooms and devices of the house, normalized: what makes two sentences about the same place.
    fn house_names(&self, layout: &crate::house::Layout) -> Vec<String> {
        let mut names: Vec<String> = layout
            .rooms
            .iter()
            .flat_map(|r| std::iter::once(r.name.clone()).chain(r.aliases.iter().cloned()))
            .collect();
        for d in &self.0.hub.snapshot().devices {
            names.push(crate::house::name_of(d).to_owned());
            names.push(layout.room_of(d));
        }
        let mut names: Vec<String> = names
            .iter()
            .map(|n| normalize(n))
            .filter(|n| n.chars().count() >= 3)
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// Whether a sentence names a device of the house, even loosely (« lampe » for « lamp »: the same first
    /// four letters). A room alone does not: « éteins la cuisine » is not about one ribbon of two.
    pub(crate) fn names_device(&self, layout: &crate::house::Layout, text: &str) -> bool {
        let stems = |text: &str| -> std::collections::HashSet<String> {
            normalize(text)
                .split(' ')
                .filter(|w| w.chars().count() >= 4)
                .map(|w| w.chars().take(4).collect())
                .collect()
        };
        let said = stems(text);
        // « Capteur températures Cuisine » carries a room's name: that word names the room, not the device.
        let devices = self.0.hub.snapshot().devices;
        let mut rooms: std::collections::HashSet<String> = layout
            .rooms
            .iter()
            .flat_map(|r| std::iter::once(&r.name).chain(r.aliases.iter()))
            .flat_map(|n| stems(n))
            .collect();
        rooms.extend(devices.iter().flat_map(|d| stems(&layout.room_of(d))));
        devices
            .iter()
            .flat_map(|d| stems(crate::house::name_of(d)))
            .any(|stem| !rooms.contains(&stem) && said.contains(&stem))
    }

    /// What the house was taught answers before the model does. `None`: no rule, or one whose device
    /// is gone (the model answers as if it did not exist).
    pub(crate) async fn apply_rule(
        &self,
        turn: &crate::Turn,
        question: &str,
        layout: &crate::house::Layout,
        aliases: &std::collections::BTreeMap<String, moli_core::DeviceId>,
    ) -> Option<crate::Reply> {
        let rule = self
            .0
            .rules
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .find(question, &self.house_names(layout))
            .cloned()?;
        // Whoever asks, a rule acts for them as the model would (what it leaves waiting is theirs).
        let mut run = crate::Run {
            asker: turn.asker.clone().unwrap_or_default(),
            ..crate::Run::default()
        };
        let said = match &rule.action {
            RuleAction::Nothing => moli_i18n::tr!("assistant.house.rule_nothing"),
            RuleAction::Call { tool, arguments } => {
                let call = crate::correct::numbered_call(tool, arguments, aliases)?;
                self.house_call(&call, layout, turn.spoken, aliases, question, &mut run)
                    .await
            }
            RuleAction::Many { calls } => {
                // All the devices are still there, or the model answers: nothing is done by halves.
                let numbered: Vec<Json> = calls
                    .iter()
                    .map(|c| crate::correct::numbered_call(&c.tool, &c.arguments, aliases))
                    .collect::<Option<_>>()?;
                let mut said = Vec::new();
                for call in &numbered {
                    let words = self
                        .house_call(call, layout, turn.spoken, aliases, question, &mut run)
                        .await;
                    if !words.is_empty() {
                        said.push(words);
                    }
                }
                said.join(" ")
            }
        };
        let mut reply = self.finish(&said, run, layout, "");
        reply.rule = Some(rule.id);
        Some(self.seal(reply, turn, question))
    }

    /// Marks a house-model reply as correctable and keeps what is needed to correct it.
    pub(crate) fn seal(
        &self,
        mut reply: crate::Reply,
        turn: &crate::Turn,
        question: &str,
    ) -> crate::Reply {
        let before = turn.messages.len().saturating_sub(1);
        let context = turn.messages[..before]
            .iter()
            .rev()
            .take(4)
            .rev()
            .map(|m| {
                (
                    format!("{:?}", m.role).to_lowercase(),
                    m.content.chars().take(300).collect(),
                )
            })
            .collect();
        let undo: Vec<Undo> = reply
            .actions
            .iter()
            .filter(|a| a.status == "done")
            .filter_map(|a| {
                a.before.clone().map(|before| Undo {
                    point: a.point.clone(),
                    before,
                    written: a.value.clone(),
                })
            })
            .collect();
        reply.house = true;
        reply.undoable = !undo.is_empty();
        reply.turn_id = self.remember_turn(Turned {
            question: question.to_owned(),
            context,
            undo,
            asker: turn.asker.clone(),
            at: std::time::Instant::now(),
        });
        reply
    }

    /// A turn still open to correction.
    /// The turn `id` of `asker`, still fresh: someone else's turn is not there for them (no hint
    /// that it exists), and nobody — no asker — has a turn.
    pub(crate) fn turn_of(&self, id: u64, asker: Option<&str>) -> Option<Turned> {
        let asker = asker?;
        self.0
            .turns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|(i, t)| {
                *i == id && t.asker.as_deref() == Some(asker) && t.at.elapsed() < TURN_LIFETIME
            })
            .map(|(_, t)| t.clone())
    }
}

/// The rules of the house, in memory and on disk.
#[derive(Debug, Default)]
pub(crate) struct Rules {
    path: Option<PathBuf>,
    list: Vec<Rule>,
}

impl Rules {
    /// Reads the rules; no file, or a file that is not valid, gives none (and a file that is not
    /// valid is never overwritten: `save` refuses until it is fixed or removed).
    pub(crate) fn load(path: &Path) -> Self {
        let list: Vec<Rule> = match std::fs::read(path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(list) => list,
                Err(e) => {
                    tracing::warn!(error = %e, "assistant-rules.json is not valid: no rules, file left alone");
                    return Self {
                        path: None,
                        list: Vec::new(),
                    };
                }
            },
            Err(_) => Vec::new(),
        };
        Self {
            path: Some(path.to_owned()),
            list,
        }
    }

    /// Written whole then swapped in, so that nothing reads half a file.
    pub(crate) fn save(&self) -> Result<()> {
        let path = self
            .path
            .as_ref()
            .ok_or_else(|| Error::new(ErrorKind::InvalidData, "no rules file to write"))?;
        let temp = path.with_extension("json.tmp");
        let mut file = private_file(
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true),
        )
        .open(&temp)?;
        file.write_all(&serde_json::to_vec_pretty(&self.list)?)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)
    }

    /// Keeps a rule; the same sentence replaces the older rule. Returns its id.
    pub(crate) fn upsert(&mut self, mut rule: Rule) -> u64 {
        self.list.retain(|r| r.key != rule.key);
        // Random, never counted: a tab left open on an old rule never reaches a new one.
        rule.id = random_id(|id| self.list.iter().any(|r| r.id == id));
        let id = rule.id;
        self.list.push(rule);
        if self.list.len() > MAX_RULES {
            self.list.remove(0);
        }
        id
    }

    pub(crate) fn remove(&mut self, id: u64) -> bool {
        let before = self.list.len();
        self.list.retain(|r| r.id != id);
        self.list.len() != before
    }

    pub(crate) fn set_enabled(&mut self, id: u64, on: bool) -> bool {
        self.list.iter_mut().find(|r| r.id == id).is_some_and(|r| {
            r.enabled = on;
            true
        })
    }

    pub(crate) fn list(&self) -> &[Rule] {
        &self.list
    }

    /// What `restore` puts back when a change could not be written down.
    pub(crate) fn snapshot(&self) -> Vec<Rule> {
        self.list.clone()
    }

    pub(crate) fn restore(&mut self, list: Vec<Rule>) {
        self.list = list;
    }

    /// The rule for a sentence: the same sentence, or one a typo away (at most one edit in a word,
    /// same number of words) that names the same rooms and devices. `names`: the house's rooms
    /// and devices, normalized. Doubt means no rule: the model answers.
    pub(crate) fn find(&self, said: &str, names: &[String]) -> Option<&Rule> {
        let said = normalize(said);
        if said.is_empty() {
            return None;
        }
        let words: Vec<&str> = said.split(' ').collect();
        // A word that differs is a typo only if it is not itself the name of a room or a device
        // (« chambre » for « cuisine » would be another place, not a slip).
        let known: std::collections::HashSet<&str> =
            names.iter().flat_map(|n| n.split(' ')).collect();
        let live = || self.list.iter().filter(|r| r.enabled);
        // The same sentence first: a close one older in the list must not win over it.
        if let Some(rule) = live().find(|r| r.key == said) {
            return Some(rule);
        }
        live().find(|r| {
            let theirs: Vec<&str> = r.key.split(' ').collect();
            theirs.len() == words.len()
                && theirs.iter().zip(&words).all(|(a, b)| {
                    // A number, or a short word, is exact (2700 is not 2200; open is not oven).
                    a == b
                        || (a.chars().count() > 4
                            && !a.chars().any(|c| c.is_ascii_digit())
                            && !b.chars().any(|c| c.is_ascii_digit())
                            && within_one_edit(a, b)
                            && !known.contains(b))
                })
        })
    }
}

/// Whether two words are at most one insertion, deletion or substitution apart.
fn within_one_edit(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.len().abs_diff(b.len()) > 1 {
        return false;
    }
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let (ra, rb) = (&a[common..], &b[common..]);
    match ra.len().cmp(&rb.len()) {
        std::cmp::Ordering::Equal => ra[1..] == rb[1..],
        std::cmp::Ordering::Greater => ra[1..] == rb[..],
        std::cmp::Ordering::Less => ra[..] == rb[1..],
    }
}

/// Adds one line to the corrections journal (the data of the next training). Past a megabyte the
/// journal turns over: the full one becomes `assistant-corrections.1.jsonl` (the one before it
/// goes) and a new one starts, so the house keeps the last two megabytes at most and never stops
/// noting what people corrected. One write at a time (two corrections at once, or a turn-over).
pub(crate) fn log_correction(dir: &Path, line: &Json) -> Result<()> {
    static WRITING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = WRITING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let target = dir.join("assistant-corrections.jsonl");
    if std::fs::metadata(&target).is_ok_and(|m| m.len() > MAX_JOURNAL) {
        std::fs::rename(&target, dir.join("assistant-corrections.1.jsonl"))?;
    }
    let mut file =
        private_file(std::fs::OpenOptions::new().create(true).append(true)).open(target)?;
    file.write_all(format!("{line}\n").as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    pub(super) fn dir() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "moli-rules-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub(super) fn rule(said: &str) -> Rule {
        Rule {
            id: 0,
            said: said.to_owned(),
            key: normalize(said),
            action: RuleAction::Call {
                tool: "set".into(),
                arguments: json!({ "point": "z2m:lampe/state", "value": false }),
            },
            created: 1,
            by: None,
            enabled: true,
        }
    }

    #[test]
    fn a_sentence_is_compared_without_case_accents_or_punctuation() {
        assert_eq!(normalize("Éteins  la Lumière !"), "eteins la lumiere");
        assert_eq!(
            normalize("Éteins la lumière du salon."),
            normalize("eteins la lumiere du salon")
        );
    }

    #[test]
    fn a_sentence_that_leans_on_the_context_is_not_kept() {
        for said in [
            "Éteins-la",
            "éteins-le maintenant",
            "monte un peu",
            "baisse un peu la lumière",
            "fais l'inverse",
            "pareil dans la cuisine",
            "et la cuisine aussi",
            "plus fort",
            "moins chaud",
            "remets comme avant",
            "allume ça",
        ] {
            assert!(contextual(said), "{said}");
        }
        for said in [
            "Éteins la lumière du salon",
            "Allume le plafonnier de l'entrée",
            "Ferme les volets de la cuisine",
            "Mets la clim à 21 degrés",
            "Baisse la lumière à 30 %",
        ] {
            assert!(!contextual(said), "{said}");
        }
    }

    #[test]
    fn the_same_sentence_replaces_the_older_rule_and_the_list_is_bounded() {
        let mut rules = Rules::load(&dir().join("assistant-rules.json"));
        let first = rules.upsert(rule("Éteins la lumière du salon"));
        let second = rules.upsert(rule("éteins la lumiere du salon!"));
        assert_ne!(first, second);
        assert_eq!(rules.list().len(), 1);
        assert_eq!(rules.list()[0].id, second);
        for n in 0..MAX_RULES + 5 {
            rules.upsert(rule(&format!("phrase numéro {n}")));
        }
        assert_eq!(rules.list().len(), MAX_RULES);
    }

    #[test]
    fn rules_survive_a_restart_and_can_be_switched_off_and_removed() {
        let path = dir().join("assistant-rules.json");
        let mut rules = Rules::load(&path);
        let id = rules.upsert(rule("Éteins la lumière du salon"));
        rules.save().unwrap();
        let mut again = Rules::load(&path);
        assert_eq!(again.list().len(), 1);
        assert!(again.set_enabled(id, false));
        assert!(!again.list()[0].enabled);
        assert!(again.remove(id));
        assert!(!again.remove(id));
        // The last rule removed, written down, Moli restarted: its number never comes back.
        again.save().unwrap();
        let mut restarted = Rules::load(&path);
        let ids: Vec<u64> = (0..20)
            .map(|n| restarted.upsert(rule(&format!("phrase {n}"))))
            .collect();
        assert!(!ids.contains(&id), "{id} reused");
        assert!(ids.iter().all(|i| *i != 0 && *i < 1 << 52));
    }

    #[test]
    fn a_file_that_is_not_valid_is_never_overwritten() {
        let path = dir().join("assistant-rules.json");
        std::fs::write(&path, "not json").unwrap();
        let mut rules = Rules::load(&path);
        assert!(rules.list().is_empty());
        rules.upsert(rule("Éteins la lumière du salon"));
        assert!(rules.save().is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not json");
    }

    #[test]
    fn the_journal_turns_over_at_a_megabyte_and_keeps_the_last_two() {
        let dir = dir();
        log_correction(&dir, &json!({ "question": "a" })).unwrap();
        let (target, older) = (
            dir.join("assistant-corrections.jsonl"),
            dir.join("assistant-corrections.1.jsonl"),
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap().lines().count(), 1);
        // Full: it becomes the older one, and a new journal starts with the next correction.
        std::fs::write(&target, "x".repeat(1024 * 1024 + 1)).unwrap();
        log_correction(&dir, &json!({ "question": "b" })).unwrap();
        assert_eq!(std::fs::metadata(&older).unwrap().len(), 1024 * 1024 + 1);
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "{\"question\":\"b\"}\n"
        );
        // Full again: the oldest goes, never more than two journals.
        std::fs::write(&target, "y".repeat(1024 * 1024 + 1)).unwrap();
        log_correction(&dir, &json!({ "question": "c" })).unwrap();
        assert!(std::fs::read_to_string(&older).unwrap().starts_with('y'));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    fn names() -> Vec<String> {
        [
            "salon",
            "cuisine",
            "chambre",
            "lumiere",
            "lampe",
            "plafonnier",
        ]
        .map(str::to_owned)
        .to_vec()
    }

    #[test]
    fn a_rule_is_found_for_the_same_sentence_even_with_a_typo() {
        let mut rules = Rules::load(&dir().join("assistant-rules.json"));
        let id = rules.upsert(rule("Éteins la lumière du salon"));
        let found = |said: &str| rules.find(said, &names()).map(|r| r.id);
        assert_eq!(found("eteins la lumiere du salon"), Some(id));
        assert_eq!(
            found("Éteins la lumière du slon"),
            Some(id),
            "one letter off"
        );
        assert_eq!(found("Éteins la lumière du salon !"), Some(id));
    }

    #[test]
    fn a_rule_never_fires_for_another_room_or_a_longer_sentence() {
        let mut rules = Rules::load(&dir().join("assistant-rules.json"));
        rules.upsert(rule("Éteins la lumière du salon"));
        let found = |said: &str| rules.find(said, &names()).is_some();
        assert!(!found("Éteins la lumière de la cuisine"), "another room");
        assert!(!found("Éteins toute la lumière du salon"), "one more word");
        assert!(!found("Allume la lumière du salon"), "another verb");
        assert!(!found("Éteins la lumière"), "a word less");
        assert!(!found(""));
    }

    #[test]
    fn a_rule_switched_off_is_ignored() {
        let mut rules = Rules::load(&dir().join("assistant-rules.json"));
        let id = rules.upsert(rule("Éteins la lumière du salon"));
        rules.set_enabled(id, false);
        assert!(rules.find("Éteins la lumière du salon", &names()).is_none());
    }

    #[test]
    fn finding_a_rule_among_five_hundred_is_instant() {
        let mut rules = Rules::load(&dir().join("assistant-rules.json"));
        for n in 0..MAX_RULES {
            rules.upsert(rule(&format!("éteins la lampe numéro {n} du salon")));
        }
        let names = names();
        let start = std::time::Instant::now();
        for _ in 0..20 {
            assert!(rules.find("éteins la lampe du bureau", &names).is_none());
        }
        assert!(
            start.elapsed().as_millis() < 100,
            "{:?} for 20 searches",
            start.elapsed()
        );
    }

    #[test]
    fn a_number_that_differs_never_passes_for_a_typo() {
        let mut rules = Rules::load(&dir().join("assistant-rules.json"));
        rules.upsert(rule("Règle la lampe à 2200 kelvins"));
        rules.upsert(rule("Open the shutter"));
        let found = |said: &str| rules.find(said, &names()).is_some();
        assert!(found("règle la lampe à 2200 kelvins"));
        assert!(
            !found("Règle la lampe à 2700 kelvins"),
            "one digit off is another value"
        );
        assert!(!found("Règle la lampe à 1200 kelvins"));
        assert!(
            !found("Oven the shutter"),
            "a short word is exact: open and oven are not one slip apart"
        );
        assert!(
            found("Open the shuttr"),
            "a longer word may slip by a letter"
        );
    }

    #[test]
    fn the_exact_rule_wins_over_an_older_rule_that_is_only_close() {
        let mut rules = Rules::load(&dir().join("assistant-rules.json"));
        let close = rules.upsert(rule("Éteins la lampe du salon"));
        let exact = rules.upsert(rule("Éteins la lampe du salin"));
        assert_ne!(close, exact);
        assert_eq!(
            rules
                .find("Éteins la lampe du salin", &names())
                .map(|r| r.id),
            Some(exact)
        );
    }

    #[test]
    fn english_and_relative_sentences_are_not_kept_either() {
        for said in [
            "turn it off",
            "turn that off",
            "turn it down a bit",
            "the same",
            "a bit more",
            "less",
            "again",
            "put it back",
            "bascule la lumière du salon",
            "change la lumière",
            "éteins cette lampe",
            "ici aussi",
            "l'autre lampe",
            "allume la même",
        ] {
            assert!(contextual(said), "{said}");
        }
        for said in [
            "turn off the kitchen light",
            "Open the living room shutters",
            "set the heating to 21",
            "ferme les volets du salon",
        ] {
            assert!(!contextual(said), "{said}");
        }
    }

    #[test]
    fn a_rule_that_cannot_be_written_is_not_kept_in_memory_either() {
        let dir = dir();
        std::fs::create_dir(dir.join("assistant-rules.json")).unwrap(); // a folder where the file should be
        let mut rules = Rules::load(&dir.join("assistant-rules.json"));
        let backup = rules.snapshot();
        rules.upsert(rule("Éteins la lampe du salon"));
        assert!(rules.save().is_err());
        rules.restore(backup);
        assert!(rules.list().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn what_people_said_is_readable_by_the_house_user_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = dir();
        log_correction(&dir, &json!({ "question": "a" })).unwrap();
        let mut rules = Rules::load(&dir.join("assistant-rules.json"));
        rules.upsert(rule("Éteins la lampe du salon"));
        rules.save().unwrap();
        for file in ["assistant-corrections.jsonl", "assistant-rules.json"] {
            let mode = std::fs::metadata(dir.join(file))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "{file}");
        }
    }
}

#[cfg(test)]
mod applied_tests {
    use serde_json::json;

    use super::tests::{dir, rule};
    use super::*;
    use crate::house_model::tests::{ask, call, house_model_in, house_model_over, words};

    fn teach(moli: &crate::Assistant, said: &str, action: RuleAction) -> u64 {
        moli.0.rules.write().unwrap().upsert(Rule {
            id: 0,
            said: said.to_owned(),
            key: normalize(said),
            action,
            created: 1,
            by: None,
            enabled: true,
        })
    }

    fn lamp_on() -> RuleAction {
        RuleAction::Call {
            tool: "set".into(),
            arguments: json!({ "point": "fake:lamp/on", "value": true }),
        }
    }

    #[tokio::test]
    async fn a_rule_answers_before_the_model_and_goes_through_the_same_order() {
        let (moli, asked) = house_model_over(crate::tests::lamp_hub().await, vec![]).await;
        let id = teach(&moli, "Allume la lampe", lamp_on());
        let reply = moli.turn(ask("allume la lampe !")).await.unwrap();
        assert!(asked.lock().unwrap().is_empty(), "the model was not asked");
        assert_eq!(reply.rule, Some(id));
        assert_eq!(reply.actions.len(), 1);
        assert_eq!(reply.actions[0].point.as_str(), "fake:lamp/on");
        assert_eq!(reply.actions[0].status, "done");
        assert!(reply.house && reply.turn_id != 0);
    }

    #[tokio::test]
    async fn a_rule_on_a_device_the_guard_holds_is_held_not_done() {
        let guard = moli_runtime::guard::GuardPolicy {
            protect_unassigned: true,
            ..Default::default()
        };
        let hub = crate::tests::lamp_hub_guarded(false, guard).await;
        let (moli, asked) = house_model_over(hub, vec![]).await;
        teach(&moli, "Allume la lampe", lamp_on());
        let reply = moli.turn(ask("Allume la lampe")).await.unwrap();
        assert!(asked.lock().unwrap().is_empty());
        assert_eq!(reply.actions[0].status, "held");
    }

    #[tokio::test]
    async fn a_rule_at_quiet_hours_is_held_like_any_order_of_moli() {
        // Quiet hours around now (an hour on each side), for the whole house.
        let hour = jiff::Zoned::now()
            .with_time_zone(jiff::tz::TimeZone::UTC)
            .hour();
        let around = |h: i8| format!("{:02}:00", (h + 24) % 24);
        let guard = moli_runtime::guard::GuardPolicy {
            quiet_hours: Some(
                moli_runtime::guard::QuietHours::parse(
                    &around(hour - 1),
                    &around(hour + 1),
                    "UTC",
                    vec![],
                )
                .unwrap(),
            ),
            ..Default::default()
        };
        let hub = crate::tests::lamp_hub_guarded(false, guard).await;
        let (moli, asked) = house_model_over(hub, vec![]).await;
        teach(&moli, "Allume la lampe", lamp_on());
        let reply = moli.turn(ask("Allume la lampe")).await.unwrap();
        assert!(asked.lock().unwrap().is_empty());
        assert_eq!(reply.actions[0].status, "held");
    }

    /// What Moli leaves waiting belongs to whoever asked, whether a rule, the model or a correction
    /// acts: two people keep a request each, the same person replaces their own.
    #[tokio::test]
    async fn what_moli_leaves_waiting_belongs_to_whoever_asked() {
        let guard = moli_runtime::guard::GuardPolicy {
            protect_unassigned: true,
            ..Default::default()
        };
        let askers = ["person:alex", "person:sam", "person:sam"];
        let by = |who: &str| {
            let mut turn = ask("Allume la lampe");
            turn.asker = Some(who.to_owned());
            turn
        };
        let hub = crate::tests::lamp_hub_guarded(false, guard.clone()).await;
        let (moli, _) = house_model_over(hub, vec![]).await;
        teach(&moli, "Allume la lampe", lamp_on());
        for who in askers {
            assert_eq!(moli.turn(by(who)).await.unwrap().actions[0].status, "held");
        }
        assert_eq!(moli.0.hub.approvals().len(), 2, "a rule");

        let set = call("set", r#"{"point":"d1/on","value":true}"#);
        let hub = crate::tests::lamp_hub_guarded(false, guard.clone()).await;
        let (moli, _) = house_model_over(hub, vec![&set, &set, &set]).await;
        for who in askers {
            assert_eq!(moli.turn(by(who)).await.unwrap().actions[0].status, "held");
        }
        assert_eq!(moli.0.hub.approvals().len(), 2, "the model");

        let unsure = words("Je ne sais pas.");
        let hub = crate::tests::lamp_hub_guarded(false, guard).await;
        let (moli, _) = house_model_over(hub, vec![&unsure, &unsure, &unsure]).await;
        let on = json!({ "tool": "set", "arguments": { "point": "fake:lamp/on", "value": true } });
        for who in askers {
            // Each corrects their own turn (a turn is its asker's).
            let turn = moli.turn(by(who)).await.unwrap().turn_id;
            let origin = moli_core::Origin::Ui;
            let done = moli
                .correct(turn, on.clone(), false, origin, None, Some(who))
                .await
                .unwrap();
            assert_eq!(done.reply.actions[0].status, "held");
        }
        assert_eq!(moli.0.hub.approvals().len(), 2, "a correction");
    }

    #[tokio::test]
    async fn a_rule_on_a_device_that_is_gone_leaves_the_model_to_answer() {
        let answer = words("Je ne sais pas.");
        let (moli, asked) = house_model_over(crate::tests::lamp_hub().await, vec![&answer]).await;
        teach(
            &moli,
            "Allume la lampe",
            RuleAction::Call {
                tool: "set".into(),
                arguments: json!({ "point": "fake:ghost/on", "value": true }),
            },
        );
        let reply = moli.turn(ask("Allume la lampe")).await.unwrap();
        assert_eq!(asked.lock().unwrap().len(), 1, "the model answered");
        assert_eq!(reply.rule, None);
        assert!(reply.actions.is_empty());
    }

    #[tokio::test]
    async fn a_rule_that_says_do_nothing_does_nothing() {
        let (moli, asked) = house_model_over(crate::tests::lamp_hub().await, vec![]).await;
        teach(&moli, "Éteins la télé", RuleAction::Nothing);
        let reply = moli.turn(ask("Éteins la télé")).await.unwrap();
        assert!(asked.lock().unwrap().is_empty());
        assert!(reply.actions.is_empty());
        assert_eq!(reply.reply, moli_i18n::tr!("assistant.house.rule_nothing"));
    }

    #[tokio::test]
    async fn a_turn_remembers_what_each_order_replaced() {
        let answer = call("set", r#"{"point":"d1/on","value":true}"#);
        let (moli, _) =
            house_model_over(crate::tests::lamp_hub().await, vec![&answer, &answer]).await;
        let mut asked = ask("Allume la lampe");
        asked.asker = Some("person:alex".into());
        let reply = moli.turn(asked).await.unwrap();
        assert_eq!(reply.actions[0].before, Some(json!(false)));
        assert!(reply.house && reply.turn_id != 0 && reply.rule.is_none());
        let turned = moli
            .turn_of(reply.turn_id, Some("person:alex"))
            .expect("the turn is kept");
        assert_eq!(turned.question, "Allume la lampe");
        assert_eq!(turned.undo.len(), 1);
        assert_eq!(turned.undo[0].point.as_str(), "fake:lamp/on");
        assert_eq!(turned.undo[0].before, json!(false));
        assert_eq!(turned.undo[0].written, json!(true));
        assert!(
            moli.turn_of(reply.turn_id + 12345, Some("person:alex"))
                .is_none()
        );
        // Strict: no asker has no turn, and a turn asked by nobody is nobody's.
        assert!(moli.turn_of(reply.turn_id, None).is_none());
        let anonymous = moli.turn(ask("Allume la lampe")).await.unwrap().turn_id;
        assert!(moli.turn_of(anonymous, None).is_none());
        assert!(moli.turn_of(anonymous, Some("")).is_none());
    }

    #[tokio::test]
    async fn the_rules_are_loaded_from_the_data_folder_at_start() {
        let dir = dir();
        let mut rules = Rules::load(&dir.join("assistant-rules.json"));
        rules.upsert(rule("Allume la lampe"));
        rules.save().unwrap();
        let (moli, _) = house_model_in(
            crate::tests::lamp_hub().await,
            Some(dir.join("home.json")),
            vec![],
        )
        .await;
        assert_eq!(moli.0.rules.read().unwrap().list().len(), 1);
    }

    #[tokio::test]
    async fn a_turn_is_its_askers_and_a_chatty_one_pushes_out_only_its_own() {
        let (moli, _) = house_model_over(crate::tests::lamp_hub().await, vec![]).await;
        let turn = |who: &str| {
            moli.remember_turn(Turned {
                question: "Allume la lampe".into(),
                context: Vec::new(),
                undo: Vec::new(),
                asker: Some(who.into()),
                at: std::time::Instant::now(),
            })
        };
        let mine = turn("person:alex");
        assert!(moli.turn_of(mine, Some("person:alex")).is_some());
        assert!(
            moli.turn_of(mine, Some("person:sam")).is_none(),
            "someone else's turn is not there"
        );
        // An agent asks a hundred times: alex's turn stays, the agent keeps its last 50.
        let flood: Vec<u64> = (0..100).map(|_| turn("client:agent")).collect();
        assert!(moli.turn_of(mine, Some("person:alex")).is_some());
        assert!(moli.turn_of(flood[0], Some("client:agent")).is_none());
        assert!(moli.turn_of(flood[99], Some("client:agent")).is_some());
        let theirs = moli
            .0
            .turns
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, t)| t.asker.as_deref() == Some("client:agent"))
            .count();
        assert_eq!(theirs, MAX_TURNS);
    }

    #[tokio::test]
    async fn a_turn_number_cannot_be_guessed_from_the_clock() {
        let (moli, _) = house_model_over(crate::tests::lamp_hub().await, vec![]).await;
        let now = moli_core::now_ms();
        let ids: Vec<u64> = (0..20)
            .map(|_| {
                moli.remember_turn(Turned {
                    question: String::new(),
                    context: Vec::new(),
                    undo: Vec::new(),
                    asker: None,
                    at: std::time::Instant::now(),
                })
            })
            .collect();
        assert_eq!(
            ids.iter().collect::<std::collections::HashSet<_>>().len(),
            20
        );
        assert!(
            ids.iter().all(|id| *id != 0 && *id < (1 << 52)),
            "a number a browser keeps whole"
        );
        assert!(
            ids.iter().any(|id| id.abs_diff(now) > 1_000_000_000),
            "not the clock"
        );
    }

    #[tokio::test]
    async fn a_reply_says_whether_there_is_anything_to_put_back() {
        let answer = call("set", r#"{"point":"d1/on","value":true}"#);
        let (moli, _) = house_model_over(
            crate::tests::lamp_hub().await,
            vec![&answer, &words("Voilà.")],
        )
        .await;
        assert!(moli.turn(ask("Allume la lampe")).await.unwrap().undoable);
        assert!(!moli.turn(ask("Bonjour")).await.unwrap().undoable);
    }
}
