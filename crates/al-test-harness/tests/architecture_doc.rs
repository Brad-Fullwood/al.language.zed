//! Pins `Docs/architecture.md` to the manifests it claims to be derived from.
//!
//! The document opens by saying every arrow comes from a real
//! `crates/*/Cargo.toml` `[dependencies]` table. Before this test it carried two
//! arrows no manifest backed (`al-snapshot`, which has no `al-*` dependency at
//! all) and was missing seven real ones. Solid `-->` arrows between `al_*` nodes
//! are compared against the non-optional path dependencies; optional and
//! dev-only edges are drawn dashed or left out by the document's own rules and
//! are excluded here.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Solid `al_x --> al_y` arrows in the mermaid diagrams, as `(from, to)` crate
/// names. Dashed arrows (`-.->`) are the optional and external edges the
/// document calls out separately.
fn documented_edges(markdown: &str) -> BTreeSet<(String, String)> {
    let mut edges = BTreeSet::new();
    for line in markdown.lines() {
        let line = line.trim();
        let Some((left, right)) = line.split_once("-->") else {
            continue;
        };
        if left.ends_with('.') || right.starts_with('|') {
            continue;
        }
        let from = left.trim();
        let to = right.trim();
        if let (Some(from), Some(to)) = (from.strip_prefix("al_"), to.strip_prefix("al_")) {
            if from.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                && to.chars().all(|c| c.is_ascii_lowercase() || c == '_')
            {
                edges.insert((
                    format!("al-{}", from.replace('_', "-")),
                    format!("al-{}", to.replace('_', "-")),
                ));
            }
        }
    }
    edges
}

/// Non-optional `al-*` entries in one manifest's `[dependencies]` table.
fn production_deps(manifest: &str) -> BTreeSet<String> {
    let mut deps = BTreeSet::new();
    let mut in_dependencies = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_dependencies = line == "[dependencies]";
            continue;
        }
        if !in_dependencies {
            continue;
        }
        let Some((name, rest)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if name.starts_with("al-") && !rest.contains("optional = true") {
            deps.insert(name.to_string());
        }
    }
    deps
}

fn manifest_edges(crates_dir: &Path) -> BTreeSet<(String, String)> {
    let mut edges = BTreeSet::new();
    for entry in std::fs::read_dir(crates_dir).expect("crates/ is readable") {
        let dir = entry.expect("directory entry").path();
        let manifest_path = dir.join("Cargo.toml");
        if !manifest_path.is_file() {
            continue;
        }
        let crate_name = dir
            .file_name()
            .expect("crate directory has a name")
            .to_string_lossy()
            .to_string();
        let manifest = std::fs::read_to_string(&manifest_path).expect("manifest is readable");
        for dep in production_deps(&manifest) {
            edges.insert((crate_name.clone(), dep));
        }
    }
    edges
}

#[test]
fn architecture_diagram_matches_the_manifests() {
    let root = repo_root();
    let markdown =
        std::fs::read_to_string(root.join("Docs/architecture.md")).expect("architecture.md exists");
    let documented = documented_edges(&markdown);
    let actual = manifest_edges(&root.join("crates"));

    let phantom: Vec<_> = documented.difference(&actual).collect();
    let missing: Vec<_> = actual.difference(&documented).collect();

    assert!(
        phantom.is_empty() && missing.is_empty(),
        "Docs/architecture.md disagrees with crates/*/Cargo.toml.\n\
         Drawn but not a production dependency: {phantom:?}\n\
         Production dependency but not drawn: {missing:?}"
    );
}
