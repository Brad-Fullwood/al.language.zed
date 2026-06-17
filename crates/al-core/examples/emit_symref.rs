//! Emit `SymbolReference.json` for an AL project, natively (no alc).
//!
//! Usage: `cargo run -p al-core --example emit_symref -- <project_dir>`
//! Prints the JSON to stdout — used to differential-test against alc's output.

use std::path::Path;

use al_core::emit::{
    build_symbol_reference, extract_objects, load_external_symbols, EmitObject, SymbolRefMeta,
};

fn main() {
    let dir = std::env::args().nth(1).expect("usage: emit_symref <project_dir>");
    let dir = Path::new(&dir);

    let app_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("app.json")).expect("app.json"))
            .expect("parse app.json");
    let s = |k: &str| app_json.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();

    let mut objects: Vec<EmitObject> = Vec::new();
    let src_dir = dir.join("src");
    let mut files: Vec<_> = walk_al(&src_dir);
    files.sort();
    for f in files {
        let content = std::fs::read_to_string(&f).expect("read .al");
        let rel = f.strip_prefix(dir).unwrap_or(&f).to_string_lossy().replace('\\', "/");
        objects.extend(extract_objects(&content, &rel));
    }

    let meta = SymbolRefMeta {
        runtime_version: s("runtime"),
        app_id: s("id"),
        name: s("name"),
        publisher: s("publisher"),
        version: s("version"),
    };
    let external = load_external_symbols(dir);
    let value = build_symbol_reference(&objects, &meta, external.as_ref());
    println!("{}", serde_json::to_string_pretty(&value).unwrap());
}

fn walk_al(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk_al(&p));
        } else if p.extension().and_then(|x| x.to_str()) == Some("al") {
            out.push(p);
        }
    }
    out
}
