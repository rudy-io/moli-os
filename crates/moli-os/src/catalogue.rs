//! `moli-os catalogue check`: every integration package (`integrations/<id>/`,
//! ARCHITECTURE.md « Le paquet d'intégration ») says what it is, truthfully.
//! Its manifest is held against what this binary runs, its profile compiles
//! as `profile check` would, and its fixtures exist and are replayed on the
//! profile's points. Nothing is built, nothing touches the network.

use std::collections::{BTreeSet, HashMap};
use std::path::{Component, Path};

use anyhow::{Context as _, bail};
use moli_core::Value;
use moli_profile::{Answer, Compiled, Profile, Source};
use serde::Deserialize;

/// Transports a manifest may name. `native`: nothing on the network, Moli
/// keeps the values itself (helpers).
const TRANSPORTS: &[&str] = &["http", "https", "mqtt", "upnp", "tcp", "native"];
const PROFILE: &str = "profile";

/// `integration.toml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    integration: Integration,
    #[serde(default)]
    discovery: Discovery,
    #[serde(default)]
    needs: Needs,
    origin: Origin,
    #[serde(default, rename = "fixture")]
    fixtures: Vec<Fixture>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Integration {
    id: String,
    name: String,
    #[serde(default)]
    brands: Vec<String>,
    #[serde(default)]
    models: Vec<String>,
    transport: String,
    /// `profile`, or the kind of a native driver.
    driver: String,
    /// The crate of a native driver (`moli-<kind>`).
    #[serde(default, rename = "crate")]
    krate: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Discovery {
    #[serde(default)]
    mdns: Vec<String>,
    #[serde(default)]
    ssdp: Vec<String>,
    #[serde(default)]
    ports: Vec<u16>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Needs {
    /// Names in the instance's encrypted store a human provides.
    #[serde(default)]
    secrets: Vec<String>,
    /// A profile's `{placeholders}`: the instance's `vars`.
    #[serde(default)]
    vars: Vec<String>,
    /// `[driver.options]` keys without a default.
    #[serde(default)]
    options: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Origin {
    /// What was reused (HA integration, library, documentation…), and how.
    reused: String,
    /// `agent` or `human`.
    author: String,
}

/// A real answer kept in `fixtures/`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    /// Relative to the package.
    file: String,
    /// Profiles: the request it answers, replayed on that request's points.
    #[serde(default)]
    request: Option<String>,
}

/// One package, as the table shows it.
#[derive(Debug, Default)]
pub struct Row {
    pub id: String,
    pub driver: String,
    pub transport: String,
    /// What runs it: the crate, or the profile's size.
    pub runs: String,
    pub fixtures: String,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

/// Every package of `dir` (one sub-folder each), checked. `kinds`: the
/// driver kinds this binary builds.
pub fn check(dir: &Path, kinds: &[&str]) -> anyhow::Result<Vec<Row>> {
    let mut packages: Vec<(String, std::path::PathBuf)> = std::fs::read_dir(dir)
        .with_context(|| format!("cannot read the catalogue {}", dir.display()))?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .map(|entry| {
            (
                entry.file_name().to_string_lossy().into_owned(),
                entry.path(),
            )
        })
        .collect();
    packages.sort();
    let mut rows: Vec<Row> = packages
        .iter()
        .map(|(id, path)| package(path, id, kinds))
        .collect();
    // One package per native kind: two would describe one driver twice.
    let mut natives: HashMap<String, usize> = HashMap::new();
    for row in rows
        .iter()
        .filter(|r| r.driver != PROFILE && r.errors.is_empty())
    {
        *natives.entry(row.driver.clone()).or_default() += 1;
    }
    for row in &mut rows {
        if natives.get(&row.driver).is_some_and(|n| *n > 1) {
            row.errors.push(format!(
                "another package also describes driver {:?}",
                row.driver
            ));
        }
    }
    Ok(rows)
}

/// `moli-os catalogue check [dir]`: the table, then warnings and errors.
pub fn run(dir: &Path, kinds: &[&str]) -> anyhow::Result<()> {
    let rows = check(dir, kinds)?;
    if rows.is_empty() {
        bail!("{}: no package (one folder per integration)", dir.display());
    }
    print_table(&rows);
    let (mut warnings, mut errors) = (0, 0);
    for row in &rows {
        for w in &row.warnings {
            println!("warning  {}: {w}", row.id);
            warnings += 1;
        }
        for e in &row.errors {
            println!("error    {}: {e}", row.id);
            errors += 1;
        }
    }
    let profiles = rows.iter().filter(|r| r.driver == PROFILE).count();
    let summary = format!(
        "{} ({} native, {}), {}",
        count(rows.len(), "package"),
        rows.len() - profiles,
        count(profiles, "profile"),
        count(warnings, "warning"),
    );
    if errors > 0 {
        bail!(
            "catalogue {}: {}; {summary}",
            dir.display(),
            count(errors, "error")
        );
    }
    println!("✓ catalogue {}: {summary}", dir.display());
    Ok(())
}

fn print_table(rows: &[Row]) {
    let header = ["", "id", "driver", "transport", "runs", "fixtures"];
    let cells: Vec<[&str; 6]> = rows
        .iter()
        .map(|r| {
            let mark = if !r.errors.is_empty() {
                "✗"
            } else if r.warnings.is_empty() {
                "✓"
            } else {
                "!"
            };
            [mark, &r.id, &r.driver, &r.transport, &r.runs, &r.fixtures]
        })
        .collect();
    let mut widths = header.map(|h| h.chars().count());
    for line in &cells {
        for (width, cell) in widths.iter_mut().zip(line) {
            *width = (*width).max(cell.chars().count());
        }
    }
    for line in std::iter::once(&header).chain(&cells) {
        let text: Vec<String> = line
            .iter()
            .zip(widths)
            .map(|(cell, width)| format!("{cell:<width$}"))
            .collect();
        println!("{}", text.join("  ").trim_end());
    }
}

/// One package: manifest, then profile or crate, then fixtures.
fn package(dir: &Path, id: &str, kinds: &[&str]) -> Row {
    let mut row = Row {
        id: id.to_owned(),
        ..Row::default()
    };
    let manifest = match std::fs::read_to_string(dir.join("integration.toml")) {
        Ok(text) => match toml::from_str::<Manifest>(&text) {
            Ok(manifest) => manifest,
            Err(e) => {
                row.errors.push(format!("integration.toml: {e}"));
                return row;
            }
        },
        Err(e) => {
            row.errors.push(format!("integration.toml: {e}"));
            return row;
        }
    };
    row.driver.clone_from(&manifest.integration.driver);
    row.transport.clone_from(&manifest.integration.transport);
    row.errors.extend(manifest_problems(&manifest, id));
    for doc in ["README.md", "onboarding.md"] {
        if !std::fs::metadata(dir.join(doc)).is_ok_and(|m| m.is_file() && m.len() > 0) {
            row.errors.push(format!("{doc} missing or empty"));
        }
    }
    let profile = if manifest.integration.driver == PROFILE {
        profile(dir, &manifest, &mut row)
    } else {
        native(dir, &manifest, kinds, &mut row);
        None
    };
    fixtures(dir, &manifest, profile.as_ref(), &mut row);
    row
}

/// What a manifest says about itself, checked on its own.
fn manifest_problems(manifest: &Manifest, folder: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let i = &manifest.integration;
    if i.id != folder {
        problems.push(format!("id {:?} must be the folder's name", i.id));
    }
    if i.id.is_empty()
        || !i
            .id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        problems.push("id: lowercase letters, digits and - only".into());
    }
    if i.name.trim().is_empty() {
        problems.push("name must not be empty".into());
    }
    if !TRANSPORTS.contains(&i.transport.as_str()) {
        problems.push(format!(
            "transport {:?} unknown (known: {})",
            i.transport,
            TRANSPORTS.join(", ")
        ));
    }
    if !["agent", "human"].contains(&manifest.origin.author.as_str()) {
        problems.push("origin.author is agent or human".into());
    }
    if manifest.origin.reused.contains('\n') {
        problems.push("origin.reused: one line (details go in README.md)".into());
    }
    let d = &manifest.discovery;
    if let Some(bad) = d
        .mdns
        .iter()
        .find(|s| !(s.starts_with('_') && (s.ends_with("._tcp") || s.ends_with("._udp"))))
    {
        problems.push(format!("discovery.mdns {bad:?}: a service like _hue._tcp"));
    }
    if d.ports.contains(&0) {
        problems.push("discovery.ports: 0 is not a port".into());
    }
    let n = &manifest.needs;
    let names = i
        .brands
        .iter()
        .chain(&i.models)
        .chain(&d.ssdp)
        .chain(&n.secrets)
        .chain(&n.vars)
        .chain(&n.options);
    for name in names {
        if name.trim().is_empty() || name.contains('\n') {
            problems.push(format!("{name:?}: an empty or multi-line name"));
        }
    }
    problems
}

/// A native driver: one this binary builds, from the crate the manifest names.
fn native(dir: &Path, manifest: &Manifest, kinds: &[&str], row: &mut Row) {
    let kind = manifest.integration.driver.as_str();
    if !kinds.contains(&kind) {
        row.errors.push(format!(
            "driver {kind:?} is not built by this binary (known: {})",
            kinds.join(", ")
        ));
    }
    let expected = format!("moli-{kind}");
    match &manifest.integration.krate {
        Some(krate) if *krate == expected => row.runs.clone_from(krate),
        _ => row.errors.push(format!(
            "a native driver names its crate: crate = {expected:?}"
        )),
    }
    if dir.join("profile.toml").exists() {
        row.errors
            .push("profile.toml in a native package: driver must be \"profile\"".into());
    }
}

/// A profile package: `profile.toml` compiles as `profile check` would
/// (built in when the binary embeds this very text, a file otherwise), and
/// the manifest agrees with it.
fn profile(dir: &Path, manifest: &Manifest, row: &mut Row) -> Option<Compiled> {
    let i = &manifest.integration;
    if let Some(krate) = &i.krate {
        row.errors.push(format!(
            "crate {krate:?}: a profile package runs on moli-profile"
        ));
    }
    let text = match std::fs::read_to_string(dir.join("profile.toml")) {
        Ok(text) => text,
        Err(e) => {
            row.errors.push(format!("profile.toml: {e}"));
            return None;
        }
    };
    let builtin = moli_profile::builtin(&i.id) == Some(text.as_str());
    let source = if builtin {
        Source::Builtin
    } else {
        Source::File
    };
    let compiled = match Profile::parse(&text).and_then(|p| p.compile(source)) {
        Ok(compiled) => compiled,
        Err(e) => {
            row.errors.push(format!("profile.toml: {}", e.message));
            return None;
        }
    };
    let meta = &compiled.meta;
    let mut problems = Vec::new();
    if meta.id != i.id {
        problems.push(format!("profile.toml has id {:?}", meta.id));
    }
    let transport = if meta.https { "https" } else { "http" };
    if i.transport != transport {
        problems.push(format!("transport is {transport:?} (the profile's https)"));
    }
    let secrets: BTreeSet<&str> = meta.secret.iter().map(String::as_str).collect();
    if sorted(&manifest.needs.secrets) != secrets {
        problems.push(format!("needs.secrets is {secrets:?} ([profile] secret)"));
    }
    let vars: BTreeSet<&str> = compiled.vars.iter().map(String::as_str).collect();
    if sorted(&manifest.needs.vars) != vars {
        problems.push(format!(
            "needs.vars is {vars:?} (the profile's placeholders)"
        ));
    }
    let needs_host = meta.host.is_none();
    if manifest.needs.options.iter().any(|o| o == "host") != needs_host {
        problems.push(format!(
            "needs.options {} \"host\" (the profile has {} host)",
            if needs_host { "lists" } else { "does not list" },
            if needs_host { "no" } else { "its own" },
        ));
    }
    row.errors.extend(problems);
    row.runs = format!(
        "{}, {}, {}{}",
        count(compiled.requests.len(), "request"),
        count(compiled.points.len(), "point"),
        count(compiled.writes.len(), "write"),
        if builtin {
            ""
        } else {
            " (file: needs profile trust)"
        },
    );
    Some(compiled)
}

/// `1 point`, `3 points`.
fn count(n: usize, what: &str) -> String {
    if n == 1 {
        format!("1 {what}")
    } else {
        format!("{n} {what}s")
    }
}

fn sorted(names: &[String]) -> BTreeSet<&str> {
    names.iter().map(String::as_str).collect()
}

/// Each cited fixture exists inside the package; a profile's are replayed on
/// the points of the request they answer. Every file of `fixtures/` is cited.
fn fixtures(dir: &Path, manifest: &Manifest, profile: Option<&Compiled>, row: &mut Row) {
    let mut answers: HashMap<String, Answer> = HashMap::new();
    let mut cited = BTreeSet::new();
    for fixture in &manifest.fixtures {
        let relative = Path::new(&fixture.file);
        if !relative
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
        {
            row.errors.push(format!(
                "fixture {:?}: a path inside the package",
                fixture.file
            ));
            continue;
        }
        let Ok(bytes) = std::fs::read(dir.join(relative)) else {
            row.errors
                .push(format!("fixture {:?} does not exist", fixture.file));
            continue;
        };
        cited.insert(dir.join(relative));
        let Some(request) = &fixture.request else {
            continue;
        };
        let Some(compiled) = profile else {
            if manifest.integration.driver != PROFILE {
                row.errors.push(format!(
                    "fixture {:?}: request applies to profiles",
                    fixture.file
                ));
            }
            continue;
        };
        match compiled.requests.iter().find(|r| r.id == *request) {
            None => row.errors.push(format!(
                "fixture {:?}: unknown request {request:?}",
                fixture.file
            )),
            Some(r) => match Answer::parse(r.format, &bytes) {
                Some(answer) => {
                    answers.insert(request.clone(), answer);
                }
                None => row.errors.push(format!(
                    "fixture {:?} is not a {:?} answer",
                    fixture.file, r.format
                )),
            },
        }
    }
    if let Ok(files) = std::fs::read_dir(dir.join("fixtures")) {
        for file in files.filter_map(Result::ok).map(|f| f.path()) {
            if !cited.contains(&file) {
                row.warnings.push(format!(
                    "{} is not cited in integration.toml",
                    file.strip_prefix(dir).unwrap_or(&file).display()
                ));
            }
        }
    }
    row.fixtures = match manifest.fixtures.len() {
        0 => "none".to_owned(),
        n => count(n, "file"),
    };
    if let Some(compiled) = profile {
        replay(compiled, &answers, row);
    }
}

/// What the replayed answers prove: each point read with a value at least
/// once, the identity found. A gap is a warning: the code was never held
/// against a real answer there.
fn replay(compiled: &Compiled, answers: &HashMap<String, Answer>, row: &mut Row) {
    if answers.is_empty() {
        row.warnings
            .push("no fixture to replay: the points were never held against a real answer".into());
        return;
    }
    let unread: Vec<&str> = compiled
        .points
        .iter()
        .filter(|p| {
            !answers
                .get(&p.request)
                .and_then(|answer| p.read(answer))
                .is_some_and(|v| !matches!(v, Value::Null))
        })
        .map(|p| &*p.spec.key)
        .collect();
    let total = compiled.points.len();
    row.fixtures = format!(
        "{}, {}/{total} points read",
        row.fixtures,
        total - unread.len()
    );
    if !unread.is_empty() {
        let shown: Vec<&str> = unread.iter().take(6).copied().collect();
        let more = unread.len() - shown.len();
        row.warnings.push(format!(
            "never read with a value in a fixture: {}{}",
            shown.join(", "),
            if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            },
        ));
    }
    for (what, reference) in [
        ("identity", &compiled.meta.identity),
        ("name_from", &compiled.meta.name_from),
    ] {
        let Some((request, path)) = reference.as_deref().and_then(|r| r.split_once('.')) else {
            continue;
        };
        let path: Vec<String> = path.split('.').map(str::to_owned).collect();
        if let Some(answer) = answers.get(request)
            && answer.text(&path).is_none()
        {
            row.errors.push(format!(
                "{what} {} is not in the fixture of {request:?}",
                reference.as_deref().unwrap_or_default()
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KINDS: &[&str] = &["hue", "profile"];

    const HUE: &str = r#"[integration]
id = "hue"
name = "Hue"
transport = "https"
driver = "hue"
crate = "moli-hue"
[origin]
reused = ""
author = "agent"
"#;

    /// A package folder with its two documents and these files.
    fn package(root: &Path, id: &str, files: &[(&str, &str)]) {
        let dir = root.join(id);
        std::fs::create_dir_all(dir.join("fixtures")).unwrap();
        for (name, text) in [("README.md", "r"), ("onboarding.md", "o")]
            .iter()
            .chain(files)
        {
            std::fs::write(dir.join(name), text).unwrap();
        }
    }

    fn errors<'a>(rows: &'a [Row], id: &str) -> &'a [String] {
        &rows.iter().find(|r| r.id == id).unwrap().errors
    }

    #[test]
    fn mistakes_are_named() {
        let root = std::env::temp_dir().join(format!("moli-catalogue-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        package(&root, "hue", &[("integration.toml", HUE)]);
        package(
            &root,
            "twin",
            &[(
                "integration.toml",
                &HUE.replace("\"hue\"\nname", "\"twin\"\nname"),
            )],
        );
        let lamp = HUE
            .replace("id = \"hue\"", "id = \"lamp\"")
            .replace("driver = \"hue\"", "driver = \"x10\"");
        package(&root, "lamp", &[("integration.toml", &lamp)]);
        package(
            &root,
            "renamed",
            &[(
                "integration.toml",
                &HUE.replace("\"hue\"\nname", "\"other\"\nname"),
            )],
        );
        package(&root, "empty", &[]);
        // A profile file (not the built-in text): compiled as `profile check` would.
        let profile = moli_profile::builtin("daikin-brp069")
            .unwrap()
            .replace("id = \"daikin-brp069\"", "id = \"clim\"");
        let clim = r#"[integration]
id = "clim"
name = "Clim"
transport = "https"
driver = "profile"
[needs]
vars = ["lat"]
options = ["host"]
[origin]
reused = ""
author = "agent"
[[fixture]]
file = "fixtures/control.kv"
request = "control"
[[fixture]]
file = "../hue/README.md"
[[fixture]]
file = "fixtures/missing.kv"
[[fixture]]
file = "fixtures/control.kv"
request = "nope"
"#;
        package(
            &root,
            "clim",
            &[
                ("integration.toml", clim),
                ("profile.toml", &profile),
                (
                    "fixtures/control.kv",
                    "ret=OK,pow=1,mode=3,stemp=22.0,f_rate=A,err=0",
                ),
                ("fixtures/stray.kv", "x=1"),
            ],
        );

        let rows = check(&root, KINDS).unwrap();
        let has = |id: &str, what: &str| errors(&rows, id).iter().any(|e| e.contains(what));
        for id in ["hue", "twin"] {
            assert!(has(id, "another package also describes driver"), "{id}");
        }
        assert!(has("lamp", "not built by this binary"));
        assert!(has("lamp", "crate = \"moli-x10\""));
        assert!(has("renamed", "must be the folder's name"));
        assert!(has("empty", "integration.toml"));
        for what in [
            "transport is \"http\"",
            "needs.vars",
            "a path inside the package",
            "does not exist",
            "unknown request \"nope\"",
        ] {
            assert!(has("clim", what), "{what}: {:?}", errors(&rows, "clim"));
        }
        let clim = rows.iter().find(|r| r.id == "clim").unwrap();
        assert!(clim.runs.contains("needs profile trust"), "{}", clim.runs);
        assert!(
            clim.fixtures.contains("5/8 points read"),
            "{}",
            clim.fixtures
        );
        assert!(
            clim.warnings.iter().any(|w| w.contains("stray.kv")),
            "{:?}",
            clim.warnings
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
