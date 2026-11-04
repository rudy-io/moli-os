//! `moli-os backup <dir>` and `moli-os restore <dir>`: everything Moli knows
//! about the house, in one folder.
//!
//! - SQLite databases are copied with `VACUUM INTO`: a consistent snapshot,
//!   safe while Moli runs (WAL), compact, a standalone file.
//! - Small files are copied as they are (`secrets.enc` stays encrypted: the
//!   master key lives in a secrets manager, never in a backup).
//! - The house plan's images (`plan/`) go along with `plan.json`.
//! - `manifest.json` says when, which version, and each file's size.
//!
//! Restoring is for a stopped Moli and never overwrites without `--force`.
//! Every file is checked against the manifest before anything is written:
//! a damaged backup restores nothing rather than half of the house.

use std::path::Path;

use anyhow::{Context as _, bail};

/// SQLite databases: copied through SQLite itself.
const DATABASES: [&str; 2] = ["history.db", "energy.db"];
/// Everything else worth keeping (missing ones are skipped).
const FILES: [&str; 19] = [
    "moli.toml",
    crate::managed_airzone::FILE,
    "plan.json",
    "phones.json",
    "former-emails.json",
    "secrets.enc",
    "labels.toml",
    "home.json",
    "tuya.json",
    "automations.json",
    "automation-runs.jsonl",
    "journal.jsonl",
    "journal.jsonl.1",
    "state.json",
    "state.devices.json",
    "state.observations.json",
    "trusted-profiles.toml",
    "perf.jsonl",
    "bench.json",
];

pub fn backup(data_dir: &Path, dir: &Path) -> anyhow::Result<()> {
    if dir.exists() && std::fs::read_dir(dir)?.next().is_some() {
        bail!(
            "{} is not empty: a backup goes into a new folder",
            dir.display()
        );
    }
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let mut files = serde_json::Map::new();
    for name in DATABASES {
        let source = data_dir.join(name);
        if !source.exists() {
            continue;
        }
        let target = dir.join(name);
        let db = rusqlite::Connection::open_with_flags(
            &source,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("cannot open {name}"))?;
        db.execute("VACUUM INTO ?1", [target.to_string_lossy().as_ref()])
            .with_context(|| format!("cannot snapshot {name}"))?;
        files.insert(name.into(), std::fs::metadata(&target)?.len().into());
    }
    for name in FILES {
        let source = data_dir.join(name);
        if !source.exists() {
            continue;
        }
        let bytes = std::fs::copy(&source, dir.join(name))
            .with_context(|| format!("cannot copy {name}"))?;
        files.insert(name.into(), bytes.into());
    }
    if let Ok(entries) = std::fs::read_dir(data_dir.join("plan")) {
        std::fs::create_dir_all(dir.join("plan"))?;
        for entry in entries.filter_map(Result::ok) {
            let Some(name) = entry
                .file_name()
                .to_str()
                .filter(|n| moli_api::plan_image_name_ok(n))
                .map(str::to_owned)
            else {
                continue;
            };
            let bytes = std::fs::copy(entry.path(), dir.join("plan").join(&name))
                .with_context(|| format!("cannot copy plan/{name}"))?;
            files.insert(format!("plan/{name}"), bytes.into());
        }
    }
    let manifest = serde_json::json!({
        "moli_os": env!("CARGO_PKG_VERSION"),
        "taken_ms": moli_core::now_ms(),
        "files": files,
    });
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)? + "\n",
    )?;
    println!(
        "backup: {} files in {} (secrets stay encrypted; the master key is not in it)",
        files.len(),
        dir.display()
    );
    Ok(())
}

pub fn restore(data_dir: &Path, dir: &Path, force: bool) -> anyhow::Result<()> {
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(dir.join("manifest.json"))
            .with_context(|| format!("{} has no manifest.json: not a backup", dir.display()))?,
    )?;
    let listed = manifest["files"]
        .as_object()
        .context("manifest without files")?;
    let names: Vec<String> = listed.keys().cloned().collect();
    // Only names this program writes: a manifest never chooses a path.
    if let Some(odd) = names.iter().find(|n| !known_name(n)) {
        bail!("unexpected file in the manifest: {odd:?}");
    }
    // The whole backup is checked before the first byte is written.
    for name in &names {
        let expected = listed[name.as_str()].as_u64();
        let found = std::fs::metadata(dir.join(name)).map(|m| m.len()).ok();
        if expected.is_none() || found != expected {
            bail!(
                "{name}: {} bytes in the backup, {} in its manifest: damaged backup, nothing restored",
                found.map_or_else(|| "no".to_owned(), |n| n.to_string()),
                expected.map_or_else(|| "?".to_owned(), |n| n.to_string()),
            );
        }
    }
    // An older backup has no authoritative cache. Keeping a newer sidecar
    // would resurrect devices/readings instead of restoring its state.json.
    let obsolete: Vec<_> = ["state.devices.json", "state.observations.json"]
        .into_iter()
        .filter(|name| !listed.contains_key(*name) && data_dir.join(name).exists())
        .collect();
    let existing = names.iter().filter(|n| data_dir.join(n).exists()).count() + obsolete.len();
    if existing != 0 && !force {
        bail!(
            "{} already holds {} of these files: stop moli-os, then restore with --force",
            data_dir.display(),
            existing
        );
    }
    std::fs::create_dir_all(data_dir)?;
    if names.iter().any(|n| n.starts_with("plan/")) {
        std::fs::create_dir_all(data_dir.join("plan"))?;
    }
    for name in &names {
        let target = data_dir.join(name);
        // A database restored next to a stale WAL would replay it.
        if DATABASES.contains(&name.as_str()) {
            for suffix in ["-wal", "-shm"] {
                let _ = std::fs::remove_file(data_dir.join(format!("{name}{suffix}")));
            }
        }
        std::fs::copy(dir.join(name), &target).with_context(|| format!("cannot restore {name}"))?;
    }
    // Only after every manifest entry and the overwrite permission have been
    // checked, and all listed files copied. No unrelated data is removed.
    for name in obsolete {
        std::fs::remove_file(data_dir.join(name))
            .with_context(|| format!("cannot remove obsolete {name}"))?;
    }
    println!(
        "restored {} files into {} (taken {} ms since the epoch)",
        names.len(),
        data_dir.display(),
        manifest["taken_ms"]
    );
    Ok(())
}

fn known_name(name: &str) -> bool {
    DATABASES.contains(&name)
        || FILES.contains(&name)
        || name
            .strip_prefix("plan/")
            .is_some_and(moli_api::plan_image_name_ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_backup_restores_into_an_empty_home() {
        let base = std::env::temp_dir().join(format!("moli-backup-{}", std::process::id()));
        let (data, saved, fresh) = (base.join("data"), base.join("saved"), base.join("fresh"));
        std::fs::create_dir_all(&data).unwrap();
        let db = rusqlite::Connection::open(data.join("history.db")).unwrap();
        db.execute_batch(
            "PRAGMA journal_mode = WAL; CREATE TABLE t (v INTEGER); INSERT INTO t VALUES (42);",
        )
        .unwrap();
        std::fs::write(data.join("labels.toml"), "x = 1\n").unwrap();
        let definitions = br#"{"version":1,"devices":{},"unclaimed":{}}"#;
        std::fs::write(data.join("state.devices.json"), definitions).unwrap();
        let observations =
            br#"{"version":2,"generation":[1,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"states":{}}"#;
        std::fs::write(data.join("state.observations.json"), observations).unwrap();
        std::fs::create_dir_all(data.join("plan")).unwrap();
        std::fs::write(data.join("plan").join("0123456789abcdef.png"), b"png").unwrap();
        std::fs::write(data.join("plan").join("notes.txt"), b"not a plan").unwrap();

        backup(&data, &saved).unwrap();
        assert!(
            backup(&data, &saved).is_err(),
            "never into a non-empty folder"
        );
        restore(&fresh, &saved, false).unwrap();
        let back = rusqlite::Connection::open(fresh.join("history.db")).unwrap();
        let v: i64 = back.query_row("SELECT v FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 42);
        assert_eq!(
            std::fs::read(fresh.join("state.devices.json")).unwrap(),
            definitions
        );
        assert_eq!(
            std::fs::read(fresh.join("state.observations.json")).unwrap(),
            observations
        );
        assert_eq!(
            std::fs::read_to_string(fresh.join("labels.toml")).unwrap(),
            "x = 1\n"
        );
        assert_eq!(
            std::fs::read(fresh.join("plan").join("0123456789abcdef.png")).unwrap(),
            b"png",
            "the plan's images come back"
        );
        assert!(!fresh.join("plan").join("notes.txt").exists());
        assert!(
            restore(&fresh, &saved, false).is_err(),
            "no overwrite without --force"
        );
        restore(&fresh, &saved, true).unwrap();
        // A damaged backup restores nothing.
        std::fs::write(saved.join("labels.toml"), "x = 12345\n").unwrap();
        let other = base.join("other");
        assert!(restore(&other, &saved, false).is_err());
        assert!(!other.join("history.db").exists(), "nothing half-restored");
        drop((db, back));
        let _ = std::fs::remove_dir_all(base);
    }

    #[derive(Debug)]
    struct OfflineFixture;
    impl moli_runtime::Driver for OfflineFixture {
        fn kind(&self) -> &'static str {
            "synthetic"
        }
        fn run<'a>(
            &'a self,
            ctx: &'a mut moli_runtime::DriverCtx,
        ) -> moli_runtime::BoxFuture<'a, anyhow::Result<()>> {
            Box::pin(async move {
                ctx.cancelled().await;
                Ok(())
            })
        }
    }

    async fn assert_newer_cache(data_dir: &Path, expected: Option<bool>) {
        let hub = moli_runtime::Hub::new(moli_runtime::HubOptions {
            state_path: Some(data_dir.join("state.json")),
            ..moli_runtime::HubOptions::default()
        })
        .unwrap();
        let cancel = tokio_util::sync::CancellationToken::new();
        let task = moli_runtime::spawn_driver(
            &hub,
            "synthetic".into(),
            std::sync::Arc::new(OfflineFixture),
            cancel.clone(),
        );
        assert_eq!(
            hub.device(&"synthetic:newer".into()).is_some(),
            expected.is_some(),
            "newer inventory must not resurrect"
        );
        assert_eq!(
            hub.state(&moli_core::PointId::from("synthetic:newer/on"))
                .map(|sample| sample.value),
            expected.map(moli_core::Value::Bool),
            "newer observations must not resurrect"
        );
        cancel.cancel();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn restoring_a_legacy_backup_removes_only_absent_state_sidecars_after_validation() {
        let base = std::env::temp_dir().join(format!("moli-legacy-backup-{}", std::process::id()));
        let (data, saved, target) = (base.join("data"), base.join("saved"), base.join("target"));
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        let old_state = br#"{"synthetic:older":{"on":{"value":false,"ts":1}}}"#;
        std::fs::write(data.join("state.json"), old_state).unwrap();
        std::fs::write(data.join("labels.toml"), "x = 1\n").unwrap();
        backup(&data, &saved).unwrap();
        // The sidecars are the only collisions: removing them also requires --force.
        let sample = serde_json::json!({"on":{"value":true,"ts":2}});
        let generation = [1_u8; 16];
        let definitions = serde_json::to_vec(&serde_json::json!({
            "version":2,"generation":generation,"unclaimed":{},
            "devices":{"synthetic:newer":{
                "kind":"synthetic","integration":"synthetic",
                "device":{"id":"synthetic:newer","instance":"synthetic","native_name":"Newer device",
                    "points":[{"key":"on","label":"On","kind":{"type":"binary"},
                        "access":{"read":true,"write":true},"semantic":"on_off"}]},
                "states":sample
            }}
        })).unwrap();
        let observations = serde_json::to_vec(&serde_json::json!({
            "version":2,"generation":generation,"states":{"synthetic:newer":sample}
        }))
        .unwrap();
        let sidecars = [
            ("state.devices.json", definitions),
            ("state.observations.json", observations),
        ];
        for (name, content) in &sidecars {
            std::fs::write(target.join(name), content).unwrap();
        }
        assert_newer_cache(&target, Some(true)).await;
        std::fs::write(target.join("unrelated.txt"), b"keep me").unwrap();
        assert!(restore(&target, &saved, false).is_err());
        assert!(!target.join("state.json").exists());
        // Neither an unknown path nor a damaged late entry may erase the cache.
        let manifest_path = saved.join("manifest.json");
        let manifest_bytes = std::fs::read(&manifest_path).unwrap();
        let mut invalid: serde_json::Value = serde_json::from_slice(&manifest_bytes).unwrap();
        invalid["files"]["unexpected.json"] = 0.into();
        std::fs::write(&manifest_path, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(restore(&target, &saved, true).is_err());
        std::fs::write(&manifest_path, manifest_bytes).unwrap();
        std::fs::write(saved.join("state.json"), b"damaged").unwrap();
        assert!(restore(&target, &saved, true).is_err());
        for (name, content) in &sidecars {
            assert_eq!(std::fs::read(target.join(name)).unwrap(), *content);
        }
        assert!(!target.join("labels.toml").exists());
        std::fs::write(saved.join("state.json"), old_state).unwrap();
        restore(&target, &saved, true).unwrap();
        assert_eq!(std::fs::read(target.join("state.json")).unwrap(), old_state);
        assert!(!target.join("state.devices.json").exists());
        assert!(!target.join("state.observations.json").exists());
        assert_newer_cache(&target, None).await;
        assert_eq!(
            std::fs::read(target.join("unrelated.txt")).unwrap(),
            b"keep me"
        );
        std::fs::remove_dir_all(base).unwrap();
    }
}
