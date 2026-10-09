//! Home Assistant's automations, translated by Moli into drafts: one at a
//! time, in the background. Nothing runs: each draft waits for a person.
//! What Moli cannot do yet (a Sonos announcement, a phone notification…)
//! is replaced as well as possible and said in the draft's note.
//!
//! An import is a person's gesture (the API checks the session) and pays
//! like the conversation: every translation takes a turn of the same
//! admission (`admit`), leaving some to the family. What is already there
//! (by stored name or by YAML) is not translated again, and the limit on
//! automations is checked before anything is spent.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::sync::PoisonError;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use moli_automation::{Actor, Author, Automation, Automations, MAX_AUTOMATIONS, tidy_name};
use moli_core::Notice;
use ring::digest::{SHA256, digest};
use serde::Deserialize;
use serde_json::Value as Json;

use crate::{Assistant, AssistantError};

/// One automation, as `tools/home-assistant/ha-automations-export.py` prints it.
#[derive(Debug, Deserialize)]
pub struct HaAutomation {
    pub alias: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub mode: Option<String>,
    pub yaml: String,
    #[serde(default)]
    pub entities: Vec<Json>,
    /// Home Assistant's own id for it (`id:` in automations.yaml): what to
    /// look for to switch the original off.
    #[serde(default)]
    pub ha_id: Option<String>,
    /// Switched off in Home Assistant (its toggle): imported all the same,
    /// so that nothing is lost, but the note says so first.
    #[serde(default)]
    pub off_in_ha: bool,
}

const MAX_ITEMS: usize = 150;
const MAX_YAML: usize = 9_000;
/// Turns of the conversation's window an import leaves to the family.
const SPARE_TURNS: usize = 10;
/// How often a waiting import asks again for a turn.
const RETRY: Duration = Duration::from_secs(5);

/// One automation to translate, and the SHA-256 of its YAML.
type Todo = (HaAutomation, String);

impl Assistant {
    /// Starts translating; returns how many will be tried (already imported
    /// ones are skipped). One import at a time.
    pub fn import_ha(
        &self,
        autos: &Automations,
        items: Vec<HaAutomation>,
    ) -> Result<usize, AssistantError> {
        let key = self.key()?;
        if items.len() > MAX_ITEMS {
            return Err(AssistantError::Invalid(format!(
                "at most {MAX_ITEMS} automations"
            )));
        }
        let running = Importing::start(self)?;
        let seen = self
            .0
            .imported
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let todo = plan(&autos.list(), items, &seen)?;
        let count = todo.len();
        if count == 0 {
            return Ok(0);
        }
        let (this, autos) = (self.clone(), autos.clone());
        tokio::spawn(async move {
            // However this task ends (panic included), the next import may run.
            let _running = running;
            let (mut done, mut failed, mut left) = (0, 0, 0);
            for (i, (item, sha)) in todo.into_iter().enumerate() {
                let Some(_turn) = this.background_turn().await else {
                    left = count - i;
                    break;
                };
                match this.translate(key.as_deref(), &autos, &item).await {
                    Ok(()) => {
                        done += 1;
                        this.0
                            .imported
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .insert(sha);
                    }
                    Err(e) => {
                        failed += 1;
                        tracing::warn!(automation = item.alias, error = %e, "not translated");
                    }
                }
            }
            tracing::info!(done, failed, left, "Home Assistant automations imported");
            this.0.hub.notice(Notice {
                title: Some(moli_i18n::tr!("assistant.import.title")),
                message: summary(done, failed, left),
                from: Some("Moli".into()),
                ts: moli_core::now_ms(),
            });
        });
        Ok(count)
    }

    /// A turn of the conversation's admission for one translation; waits for
    /// one (a window at most) rather than failing: `None` when none came.
    async fn background_turn(&self) -> Option<tokio::sync::SemaphorePermit<'_>> {
        let since = Instant::now();
        loop {
            if let Ok(turn) = self.admit_leaving(SPARE_TURNS) {
                return Some(turn);
            }
            if since.elapsed() > crate::TURN_WINDOW {
                return None;
            }
            tokio::time::sleep(RETRY).await;
        }
    }

    async fn translate(
        &self,
        key: Option<&str>,
        autos: &Automations,
        item: &HaAutomation,
    ) -> Result<(), AssistantError> {
        let yaml: String = item.yaml.chars().take(MAX_YAML).collect();
        let entities = serde_json::to_string(&item.entities).unwrap_or_default();
        let request = import_request(item, &yaml, &entities);
        let draft = self.draft_with(key, autos, &request, None).await?;
        let a = Automation {
            id: String::new(),
            name: tidy_name(&item.alias),
            enabled: false,
            mode: draft.mode,
            graph: draft.graph,
            author: Author::Import,
            approved: None,
            approved_version: None,
            note: Some(note(item, draft.note.as_deref())),
            created: 0,
            updated: 0,
        };
        autos
            .save(
                a,
                Actor {
                    human: false,
                    author: Author::Import,
                },
            )
            .await
            .map(|_| ())
            .map_err(|e| AssistantError::Invalid(e.to_string()))
    }
}

/// What Moli is asked for one Home Assistant automation. The YAML comes last
/// among the words filled in: nothing in it is ever taken for a placeholder.
fn import_request(item: &HaAutomation, yaml: &str, entities: &str) -> String {
    moli_i18n::tr!(
        "assistant.import.request",
        alias = item.alias,
        description = item.description,
        mode = item.mode.as_deref().unwrap_or("single"),
        entities = entities,
        yaml = yaml,
    )
}

/// What will be translated: once per stored name (the alias cut as saving
/// cuts it) and once per YAML, within the batch and against what is there
/// (`seen`: YAML already translated); refused whole if the automations'
/// limit would be passed.
fn plan(
    existing: &[Automation],
    items: Vec<HaAutomation>,
    seen: &HashSet<String>,
) -> Result<Vec<Todo>, AssistantError> {
    let mut names: HashSet<String> = existing
        .iter()
        .filter(|a| a.author == Author::Import)
        .map(|a| a.name.clone())
        .collect();
    let mut yamls = seen.clone();
    let mut todo = Vec::new();
    for item in items {
        let (name, sha) = (tidy_name(&item.alias), sha256(&item.yaml));
        if names.contains(&name) || yamls.contains(&sha) {
            continue;
        }
        names.insert(name);
        yamls.insert(sha.clone());
        todo.push((item, sha));
    }
    if existing.len() + todo.len() > MAX_AUTOMATIONS {
        return Err(AssistantError::Invalid(format!(
            "{} automations and {} to translate: at most {MAX_AUTOMATIONS}",
            existing.len(),
            todo.len()
        )));
    }
    Ok(todo)
}

fn sha256(text: &str) -> String {
    digest(&SHA256, text.as_bytes()).as_ref().iter().fold(
        String::with_capacity(64),
        |mut hex, b| {
            let _ = write!(hex, "{b:02x}");
            hex
        },
    )
}

/// The draft's note: where it comes from and what to do in Home Assistant
/// (both would act otherwise), then what Moli could not translate.
fn note(item: &HaAutomation, translated: Option<&str>) -> String {
    let alias = tidy_name(&item.alias);
    let id: Option<String> = item
        .ha_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(|id| id.chars().take(64).collect());
    let mut note = if item.off_in_ha {
        let mut off = moli_i18n::tr!("assistant.import.note_off");
        off.push(' ');
        off
    } else {
        String::new()
    };
    note.push_str(&match id {
        Some(id) => moli_i18n::tr!("assistant.import.note_from_id", alias = alias, id = id),
        None => moli_i18n::tr!("assistant.import.note_from", alias = alias),
    });
    note.push(' ');
    note.push_str(&moli_i18n::tr!("assistant.import.note_switch_off"));
    if let Some(t) = translated.map(str::trim).filter(|t| !t.is_empty()) {
        note.push(' ');
        note.push_str(t);
    }
    note
}

fn summary(done: usize, failed: usize, left: usize) -> String {
    // French counts none as one (« 0 automatisme traduit »): its own sentence.
    let mut message = if done == 0 {
        moli_i18n::tr!("assistant.import.done_none")
    } else {
        moli_i18n::tr!("assistant.import.done", count = done)
    };
    if failed > 0 {
        let _ = write!(
            message,
            " {}",
            moli_i18n::tr!("assistant.import.failed", failed = failed)
        );
    }
    if left > 0 {
        let _ = write!(
            message,
            " {}",
            moli_i18n::tr!("assistant.import.left", left = left)
        );
    }
    message
}

/// « An import is running »: cleared when dropped, however the import ends.
#[derive(Debug)]
struct Importing(Assistant);

impl Importing {
    fn start(assistant: &Assistant) -> Result<Self, AssistantError> {
        if assistant.0.importing.swap(true, Ordering::SeqCst) {
            return Err(AssistantError::Busy);
        }
        Ok(Self(assistant.clone()))
    }
}

impl Drop for Importing {
    fn drop(&mut self) {
        self.0.0.importing.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ha(alias: &str, yaml: &str) -> HaAutomation {
        HaAutomation {
            alias: alias.into(),
            description: String::new(),
            mode: None,
            yaml: yaml.into(),
            entities: Vec::new(),
            ha_id: None,
            off_in_ha: false,
        }
    }

    fn stored(name: &str, author: &str) -> Automation {
        serde_json::from_value(json!({ "name": name, "graph": {}, "author": author })).unwrap()
    }

    fn aliases(todo: &[Todo]) -> Vec<&str> {
        todo.iter().map(|(i, _)| i.alias.as_str()).collect()
    }

    #[test]
    fn the_limit_counts_what_is_there_and_what_is_to_translate() {
        let existing: Vec<Automation> = (0..MAX_AUTOMATIONS - 2)
            .map(|i| stored(&format!("A{i}"), "human"))
            .collect();
        let two = || vec![ha("Entrée", "alias: Entrée"), ha("Garage", "alias: Garage")];
        assert_eq!(plan(&existing, two(), &HashSet::new()).unwrap().len(), 2);
        let one_more = [existing, vec![stored("Salon", "human")]].concat();
        assert!(
            matches!(
                plan(&one_more, two(), &HashSet::new()),
                Err(AssistantError::Invalid(_))
            ),
            "refused before anything is spent"
        );
        // What is skipped does not count.
        let imported = [one_more, vec![stored("Garage", "import")]].concat();
        assert!(plan(&imported, two(), &HashSet::new()).is_err());
        let skipped = [
            imported[..MAX_AUTOMATIONS - 2].to_vec(),
            vec![stored("Garage", "import")],
        ]
        .concat();
        assert_eq!(
            aliases(&plan(&skipped, two(), &HashSet::new()).unwrap()),
            ["Entrée"]
        );
    }

    #[test]
    fn the_same_yaml_is_translated_once() {
        let items = vec![
            ha("Entrée la nuit", "trigger: porte"),
            // Renamed in HA or in Moli: same YAML, same automation.
            ha("Entrée (copie)", "trigger: porte"),
            ha("Garage", "trigger: garage"),
        ];
        let todo = plan(&[], items, &HashSet::new()).unwrap();
        assert_eq!(aliases(&todo), ["Entrée la nuit", "Garage"]);
        assert_eq!(todo[0].1, sha256("trigger: porte"));
        // Already translated since start, even if its draft was renamed.
        let seen = HashSet::from([sha256("trigger: garage")]);
        let todo = plan(
            &[stored("Lumières du garage", "import")],
            vec![ha("Garage", "trigger: garage")],
            &seen,
        )
        .unwrap();
        assert!(todo.is_empty());
    }

    #[test]
    fn long_aliases_are_compared_as_they_are_stored() {
        let long = "Quand ".repeat(30);
        let existing = [stored(&tidy_name(&long), "import")];
        assert!(
            plan(&existing, vec![ha(&long, "x")], &HashSet::new())
                .unwrap()
                .is_empty()
        );
        // Twice in the same batch: once.
        let twice = vec![ha("Garage", "a"), ha("Garage ", "b")];
        assert_eq!(
            aliases(&plan(&[], twice, &HashSet::new()).unwrap()),
            ["Garage"]
        );
        // A person's automation of the same name is not an import.
        let mine = [stored("Garage", "human")];
        assert_eq!(
            plan(&mine, vec![ha("Garage", "a")], &HashSet::new())
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn the_note_says_which_home_assistant_automation_to_switch_off() {
        let mut item = ha("Entrée la nuit", "x");
        item.ha_id = Some("1693412345678".into());
        let n = note(
            &item,
            Some("Les annonces Sonos deviennent « prévenir la maison »."),
        );
        assert!(n.starts_with(
            "Importé de Home Assistant (automation « Entrée la nuit », id 1693412345678). \
             Quand tu valides celui-ci, désactive l'original dans HA, sinon les deux agiront."
        ));
        assert!(n.ends_with("« prévenir la maison »."));
        item.off_in_ha = true;
        assert!(note(&item, None).starts_with("⚠ Désactivé dans HA"));
        item.off_in_ha = false;
        item.ha_id = None;
        assert_eq!(
            note(&item, None),
            "Importé de Home Assistant (automation « Entrée la nuit »). \
             Quand tu valides celui-ci, désactive l'original dans HA, sinon les deux agiront."
        );
    }

    #[tokio::test]
    async fn an_import_that_panics_does_not_block_the_next() {
        let hub = moli_runtime::Hub::new(moli_runtime::HubOptions::default()).unwrap();
        let config = serde_json::from_value(json!({})).unwrap();
        let moli = Assistant::new(hub, None, None, None, config).unwrap();
        let running = Importing::start(&moli).unwrap();
        assert!(matches!(Importing::start(&moli), Err(AssistantError::Busy)));
        let task = tokio::spawn(async move {
            let _running = running;
            panic!("a translation went wrong");
        });
        assert!(task.await.unwrap_err().is_panic());
        assert!(Importing::start(&moli).is_ok());
    }

    #[test]
    fn the_export_s_id_is_read() {
        let item: HaAutomation = serde_json::from_value(json!({
            "alias": "Entrée", "yaml": "alias: Entrée", "ha_id": "1693412345678"
        }))
        .unwrap();
        assert_eq!(item.ha_id.as_deref(), Some("1693412345678"));
        let older: HaAutomation =
            serde_json::from_value(json!({ "alias": "Entrée", "yaml": "x" })).unwrap();
        assert!(older.ha_id.is_none());
    }

    use crate::before;

    #[test]
    fn the_french_import_words_are_what_they_always_were() {
        let mut item = ha(
            "Entrée la nuit",
            "alias: Entrée\ntrigger: {{ states('porte') }}",
        );
        item.description = "Allume l'entrée".into();
        item.mode = Some("restart".into());
        let entities = r#"[{"entity_id":"light.entree","name":"Entrée"}]"#;
        let plain = ha("Garage", "trigger: garage");
        for it in [&item, &plain] {
            assert_eq!(
                import_request(it, &it.yaml, entities),
                before::import_request(it, &it.yaml, entities)
            );
        }
        for ha_id in [Some("1693412345678"), None] {
            item.ha_id = ha_id.map(Into::into);
            for off in [false, true] {
                item.off_in_ha = off;
                for translated in [
                    None,
                    Some("Les annonces Sonos deviennent « prévenir la maison »."),
                    Some("  "),
                ] {
                    assert_eq!(note(&item, translated), before::note(&item, translated));
                }
            }
        }
        for (done, failed, left) in [
            (0, 0, 0),
            (1, 0, 0),
            (2, 1, 0),
            (5, 0, 3),
            (1, 2, 3),
            (0, 3, 0),
        ] {
            assert_eq!(
                summary(done, failed, left),
                before::summary(done, failed, left)
            );
        }
    }

    /// The editor cuts the warning off the note at its last words.
    #[test]
    fn the_note_ends_where_the_editor_cuts_it() {
        assert!(
            crate::tests::french()["assistant.import.note_switch_off"]
                .ends_with("sinon les deux agiront.")
        );
        assert!(
            crate::tests::english()["assistant.import.note_switch_off"]
                .ends_with("or both will run.")
        );
    }
}
