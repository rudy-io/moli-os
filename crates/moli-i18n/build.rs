//! Embeds every catalogue of `locales/<language>/<area>.json` (repository
//! root): the binary carries the house's words, nothing to install.

use std::fmt::Write as _;
use std::path::Path;

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../locales");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut files = Vec::new();
    if let Ok(languages) = std::fs::read_dir(&root) {
        for language in languages.flatten() {
            let lang = language.file_name().to_string_lossy().into_owned();
            println!("cargo:rerun-if-changed={}", language.path().display());
            let Ok(areas) = std::fs::read_dir(language.path()) else {
                continue;
            };
            for area in areas.flatten() {
                let path = area.path();
                if path.extension().is_some_and(|e| e == "json") {
                    println!("cargo:rerun-if-changed={}", path.display());
                    files.push((lang.clone(), path));
                }
            }
        }
    }
    files.sort();
    let mut out = String::from(
        "/// (language, catalogue) pairs, from `locales/`.\npub static CATALOGUES: &[(&str, &str)] = &[\n",
    );
    for (lang, path) in &files {
        let absolute = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
        let _ = writeln!(
            out,
            "    ({lang:?}, include_str!({:?})),",
            absolute.display().to_string()
        );
    }
    out.push_str("];\n");
    let dest = Path::new(&std::env::var("OUT_DIR").unwrap_or_default()).join("catalogues.rs");
    std::fs::write(dest, out).unwrap_or_else(|e| panic!("cannot write the catalogue list: {e}"));
}
