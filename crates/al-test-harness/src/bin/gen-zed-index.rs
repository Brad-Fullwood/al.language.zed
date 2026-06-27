//! Generate Zed's `extensions/index.json` so a PREBUILT copy of the AL
//! extension registers as a normal (non-dev) installed extension — no Zed
//! dev-compile, no wasi-sdk download. Reads the repo's `extension.toml`,
//! `languages/<lang>/config.toml`, and theme files, and prints the index JSON
//! to stdout. Host-side replacement for the former container `gen_index.py`,
//! so the editor-e2e container needs no Python.
//!
//! Usage: gen-zed-index <repo-root> > index.json

use std::path::Path;
use std::process::exit;

use serde_json::{json, Map, Value};
use toml::Value as Toml;

fn s(t: &Toml, key: &str, default: &str) -> String {
    t.get(key)
        .and_then(Toml::as_str)
        .unwrap_or(default)
        .to_string()
}

fn str_array(t: Option<&Toml>) -> Vec<String> {
    t.and_then(Toml::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn main() {
    let repo = match std::env::args().nth(1) {
        Some(p) => p,
        None => {
            eprintln!("usage: gen-zed-index <repo-root> > index.json");
            exit(2);
        }
    };
    let repo = Path::new(&repo);

    let ext: Toml = std::fs::read_to_string(repo.join("extension.toml"))
        .expect("read extension.toml")
        .parse()
        .expect("parse extension.toml");

    let ext_id = s(&ext, "id", "");
    let empty = Toml::Table(Default::default());
    let grammars = ext.get("grammars").unwrap_or(&empty);
    let gname = grammars
        .as_table()
        .and_then(|t| t.keys().next().cloned())
        .unwrap_or_else(|| "al".to_string());
    let grammar = grammars.get(&gname).unwrap_or(&empty);
    let lib = ext.get("lib").unwrap_or(&empty);

    let mut language_servers = Map::new();
    if let Some(ls_table) = ext.get("language_servers").and_then(Toml::as_table) {
        for (id, ls) in ls_table {
            language_servers.insert(
                id.clone(),
                json!({
                    "language": null,
                    "languages": str_array(ls.get("languages")),
                    "language_ids": {},
                    "code_action_kinds": null,
                }),
            );
        }
    }
    let map_keys_to = |key: &str, val: Value| -> Map<String, Value> {
        ext.get(key)
            .and_then(Toml::as_table)
            .map(|t| t.keys().map(|k| (k.clone(), val.clone())).collect())
            .unwrap_or_default()
    };

    let manifest = json!({
        "id": ext_id,
        "name": s(&ext, "name", &ext_id),
        "version": s(&ext, "version", "0.0.0"),
        "schema_version": ext.get("schema_version").and_then(Toml::as_integer).unwrap_or(1),
        "description": s(&ext, "description", ""),
        "repository": s(&ext, "repository", ""),
        "authors": str_array(ext.get("authors")),
        "lib": {"kind": s(lib, "kind", "Rust"), "version": s(lib, "version", "0.7.0")},
        "themes": str_array(ext.get("themes")),
        "icon_themes": [],
        "languages": str_array(ext.get("languages")),
        "grammars": {&gname: {
            "repository": s(grammar, "repository", ""),
            "rev": s(grammar, "rev", ""),
            "path": null,
        }},
        "language_servers": language_servers,
        "context_servers": map_keys_to("context_servers", json!({})),
        "slash_commands": {},
        "snippets": str_array(ext.get("snippets")),
        "capabilities": [],
        "debug_adapters": map_keys_to("debug_adapters", json!({"schema_path": null})),
        "debug_locators": map_keys_to("debug_locators", json!({})),
    });

    // Top-level language registry — read each languages/<path>/config.toml.
    let mut languages = Map::new();
    for lang_path in str_array(ext.get("languages")) {
        let cfg: Toml = std::fs::read_to_string(repo.join(&lang_path).join("config.toml"))
            .expect("read language config.toml")
            .parse()
            .expect("parse language config.toml");
        languages.insert(
            s(&cfg, "name", ""),
            json!({
                "extension": ext_id,
                "path": lang_path,
                "matcher": {
                    "path_suffixes": str_array(cfg.get("path_suffixes")),
                    "first_line_pattern": null,
                    "modeline_aliases": [],
                },
                "hidden": false,
                "grammar": cfg.get("grammar").and_then(Toml::as_str),
            }),
        );
    }

    // Top-level theme registry — read theme family names from each theme file.
    let mut themes = Map::new();
    for theme_path in str_array(ext.get("themes")) {
        if let Ok(text) = std::fs::read_to_string(repo.join(&theme_path)) {
            if let Ok(data) = serde_json::from_str::<Value>(&text) {
                if let Some(arr) = data.get("themes").and_then(Value::as_array) {
                    for t in arr {
                        if let Some(name) = t.get("name").and_then(Value::as_str) {
                            themes.insert(
                                name.to_string(),
                                json!({"extension": ext_id, "path": theme_path}),
                            );
                        }
                    }
                }
            }
        }
    }

    let index = json!({
        "extensions": {&ext_id: {"manifest": manifest, "dev": false}},
        "themes": themes,
        "icon_themes": {},
        "languages": languages,
    });
    println!("{}", serde_json::to_string_pretty(&index).unwrap());
}
