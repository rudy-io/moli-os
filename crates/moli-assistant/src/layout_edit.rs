//! Edits of `home.json` that the conversation proposes (hide a device, rename
//! a room) once a person has confirmed them. The file is the family's: what
//! the edit does not touch stays as it was (unknown keys included), and it is
//! written whole then swapped in, so the dashboard that watches it never reads
//! half a file. A file that is not valid JSON is never overwritten.

use std::io::{Error, ErrorKind, Result};
use std::path::Path;

use serde_json::{Value as Json, json};

use crate::house::norm;

fn read(path: &Path) -> Result<Json> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
            Error::new(
                ErrorKind::InvalidData,
                format!("home.json is not valid JSON: {e}"),
            )
        }),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(e),
    }
}

/// One edit at a time: two confirmations at once would otherwise read the same
/// file and one of the two changes would be lost.
static EDITING: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn write(path: &Path, document: &Json) -> Result<()> {
    use std::io::Write as _;
    let temp = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&temp)?;
    file.write_all(&serde_json::to_vec_pretty(document)?)?;
    file.sync_all()?; // on disk before it replaces the file: a power cut leaves the old one or the new one
    std::fs::rename(&temp, path)
}

/// The house's position, as a person chose it (`location` in `home.json`): it
/// wins over `moli.toml` at start.
#[must_use]
pub fn saved_place(path: &Path) -> Option<crate::places::Place> {
    let document = read(path).ok()?;
    let at = document.get("location")?;
    let (latitude, longitude) = (at["latitude"].as_f64()?, at["longitude"].as_f64()?);
    ((-90.0..=90.0).contains(&latitude) && (-180.0..=180.0).contains(&longitude)).then(|| {
        crate::places::Place {
            label: at["place"].as_str().unwrap_or_default().to_owned(),
            latitude,
            longitude,
        }
    })
}

/// Keeps the house's position, or forgets it (`None`: `moli.toml` speaks again).
pub(crate) fn set_location(path: &Path, place: Option<&crate::places::Place>) -> Result<()> {
    let _one_at_a_time = EDITING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut document = read(path)?;
    let root = document
        .as_object_mut()
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "home.json is not an object"))?;
    match place {
        Some(p) => {
            root.insert(
                "location".into(),
                json!({ "place": p.label, "latitude": p.latitude, "longitude": p.longitude }),
            );
        }
        None => {
            root.remove("location");
        }
    }
    write(path, &document)
}

/// Hides a device from the dashboard and from the model, or shows it again.
pub(crate) fn set_hidden(path: &Path, device: &str, hidden: bool) -> Result<()> {
    let _one_at_a_time = EDITING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut document = read(path)?;
    let root = document
        .as_object_mut()
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "home.json is not an object"))?;
    let list = root.entry("hidden").or_insert_with(|| json!([]));
    let list = list
        .as_array_mut()
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "`hidden` is not a list"))?;
    list.retain(|h| h.as_str() != Some(device));
    if hidden {
        list.push(json!(device));
    }
    write(path, &document)
}

/// Renames a room, the old name kept as an alias (so labels and devices that
/// still say the old name keep resolving to the room). A room the file does
/// not list yet (the devices name it) is added.
pub(crate) fn rename_room(path: &Path, from: &str, to: &str) -> Result<()> {
    let _one_at_a_time = EDITING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut document = read(path)?;
    let root = document
        .as_object_mut()
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "home.json is not an object"))?;
    let rooms = root.entry("rooms").or_insert_with(|| json!([]));
    let rooms = rooms
        .as_array_mut()
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "`rooms` is not a list"))?;
    let names = |room: &Json| -> Vec<String> {
        std::iter::once(&room["name"])
            .chain(room["aliases"].as_array().into_iter().flatten())
            .filter_map(|n| n.as_str().map(norm))
            .collect()
    };
    let (from_key, to_key) = (norm(from), norm(to));
    let index = rooms.iter().position(|r| names(r).contains(&from_key));
    let taken = rooms
        .iter()
        .enumerate()
        .any(|(i, r)| Some(i) != index && names(r).contains(&to_key));
    if taken {
        return Err(Error::new(
            ErrorKind::AlreadyExists,
            "a room already has that name",
        ));
    }
    match index {
        Some(i) => {
            let entry = &mut rooms[i];
            let old = entry["name"].as_str().unwrap_or(from).to_owned();
            entry["name"] = json!(to);
            let aliases = entry
                .as_object_mut()
                .ok_or_else(|| Error::new(ErrorKind::InvalidData, "a room is not an object"))?
                .entry("aliases")
                .or_insert_with(|| json!([]));
            if let Some(aliases) = aliases.as_array_mut() {
                aliases.retain(|a| a.as_str().map(norm).as_ref() != Some(&to_key));
                if norm(&old) != to_key
                    && !aliases
                        .iter()
                        .any(|a| a.as_str().map(norm) == Some(norm(&old)))
                {
                    aliases.push(json!(old));
                }
            }
        }
        None => rooms.push(json!({ "name": to, "aliases": [from] })),
    }
    write(path, &document)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::house::Layout;

    fn file(content: Option<&str>) -> std::path::PathBuf {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "moli-layout-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("home.json");
        if let Some(content) = content {
            std::fs::write(&path, content).unwrap();
        }
        path
    }

    fn json_of(path: &Path) -> Json {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn the_location_is_kept_in_the_file_and_read_back_and_the_rest_stays() {
        let path = file(Some(r#"{"title":"Chez nous","hidden":["a:1"]}"#));
        assert_eq!(saved_place(&path), None);
        let place = crate::places::Place {
            label: "Lyon (Auvergne-Rhône-Alpes, France)".into(),
            latitude: 45.76,
            longitude: 4.84,
        };
        set_location(&path, Some(&place)).unwrap();
        assert_eq!(saved_place(&path), Some(place.clone()));
        let doc = json_of(&path);
        assert_eq!(doc["title"], "Chez nous");
        assert_eq!(doc["hidden"], json!(["a:1"]));
        assert_eq!(
            doc["location"]["place"],
            "Lyon (Auvergne-Rhône-Alpes, France)"
        );
        set_location(&path, None).unwrap();
        assert_eq!(saved_place(&path), None);
        assert!(json_of(&path).get("location").is_none());
    }

    #[test]
    fn a_location_out_of_the_earth_is_not_read_back() {
        let path = file(Some(
            r#"{"location":{"place":"X","latitude":120.0,"longitude":4.0}}"#,
        ));
        assert_eq!(saved_place(&path), None);
        let path = file(Some("not json"));
        assert_eq!(saved_place(&path), None);
    }

    #[test]
    fn a_device_is_hidden_and_shown_again_and_the_rest_of_the_file_stays() {
        let path = file(Some(
            r#"{"title":"Chez nous","rooms":[{"name":"Salon"}],"hidden":["a:1"],"other":{"x":1}}"#,
        ));
        set_hidden(&path, "b:2", true).unwrap();
        assert_eq!(json_of(&path)["hidden"], json!(["a:1", "b:2"]));
        set_hidden(&path, "b:2", true).unwrap();
        assert_eq!(json_of(&path)["hidden"], json!(["a:1", "b:2"]), "no twice");
        set_hidden(&path, "a:1", false).unwrap();
        let doc = json_of(&path);
        assert_eq!(doc["hidden"], json!(["b:2"]));
        assert_eq!(doc["title"], "Chez nous");
        assert_eq!(
            doc["other"],
            json!({ "x": 1 }),
            "keys the edit does not know stay"
        );
        assert_eq!(doc["rooms"], json!([{ "name": "Salon" }]));
        assert!(
            !path.with_extension("json.tmp").exists(),
            "nothing left behind"
        );
    }

    #[test]
    fn two_edits_at_once_both_land_and_the_file_stays_valid() {
        let path = file(Some("{}"));
        let threads: Vec<_> = (0..12)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || set_hidden(&path, &format!("a:{i}"), true).unwrap())
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(json_of(&path)["hidden"].as_array().unwrap().len(), 12);
    }

    #[test]
    fn hiding_works_without_a_file_yet() {
        let path = file(None);
        set_hidden(&path, "a:1", true).unwrap();
        assert_eq!(json_of(&path), json!({ "hidden": ["a:1"] }));
    }

    #[test]
    fn a_room_is_renamed_and_keeps_its_old_name_as_an_alias() {
        let path = file(Some(
            r#"{"rooms":[{"name":"Salon","aliases":["Séjour d'été"]},{"name":"Cuisine"}]}"#,
        ));
        rename_room(&path, "salon", "Séjour").unwrap();
        let doc = json_of(&path);
        assert_eq!(doc["rooms"][0]["name"], "Séjour");
        assert_eq!(doc["rooms"][0]["aliases"], json!(["Séjour d'été", "Salon"]));
        assert_eq!(doc["rooms"][1], json!({ "name": "Cuisine" }));
    }

    #[test]
    fn a_room_the_file_does_not_list_is_added_with_the_old_name_as_alias() {
        let path = file(Some(r#"{"rooms":[{"name":"Cuisine"}]}"#));
        rename_room(&path, "Salon", "Séjour").unwrap();
        assert_eq!(
            json_of(&path)["rooms"][1],
            json!({ "name": "Séjour", "aliases": ["Salon"] })
        );
    }

    #[test]
    fn the_new_name_of_another_room_is_refused_and_the_file_is_untouched() {
        let content = r#"{"rooms":[{"name":"Salon"},{"name":"Cuisine"}]}"#;
        let path = file(Some(content));
        let e = rename_room(&path, "Salon", "cuisine").unwrap_err();
        assert_eq!(e.kind(), ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
    }

    #[test]
    fn a_file_that_is_not_json_is_never_overwritten() {
        let path = file(Some("{ pas du json"));
        assert!(set_hidden(&path, "a:1", true).is_err());
        assert!(rename_room(&path, "Salon", "Séjour").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ pas du json");
    }

    #[tokio::test]
    async fn the_layout_the_assistant_reads_follows_the_edits() {
        let path = file(Some(r#"{"rooms":[{"name":"Salon"}]}"#));
        set_hidden(&path, "z2m:cam", true).unwrap();
        rename_room(&path, "Salon", "Séjour").unwrap();
        let layout = Layout::load(Some(&path)).await;
        assert!(layout.hides("z2m:cam"));
        assert_eq!(
            layout.room("Salon"),
            "Séjour",
            "the old name still finds the room"
        );
        assert_eq!(layout.room("séjour"), "Séjour");
    }
}
