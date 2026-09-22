//! Every tree-sitter node kind named in al-analysis and al-insight must exist
//! in the AL grammar.
//!
//! A misspelled kind compiles, runs, and silently never matches, so the branch
//! behind it is dead and nothing fails. `test_coverage`'s direct pass keyed on
//! four kinds (`method_call`, `function_call`, `invocation_expression`,
//! `call_expression`) that tree-sitter-al does not have, which left ~110 lines
//! unreachable and every coverage claim coming from the call graph pass alone.
//!
//! The grammar's own kind table (`Language::node_kind_for_id`) is the
//! reference, so this test tracks a grammar bump without regeneration.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use regex::Regex;

/// A `(file, kind)` pair this test tolerates, with the reason.
///
/// Empty: every kind literal in both crates names a node the grammar defines.
/// An entry here is a debt, not a licence — write the reason and the finding
/// that tracks it.
const ALLOWED_ABSENT: &[(&str, &str)] = &[];

fn grammar_kinds() -> BTreeSet<String> {
    let language = al_syntax::parser::language();
    (0..language.node_kind_count())
        .filter_map(|id| language.node_kind_for_id(id as u16))
        .map(str::to_string)
        .collect()
}

fn crate_src_dirs() -> Vec<PathBuf> {
    let crates = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    vec![
        crates.join("al-analysis").join("src"),
        crates.join("al-insight").join("src"),
    ]
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// Every string literal compared against a tree-sitter `kind()`, by file.
///
/// Three shapes carry a kind: a direct `node.kind() == "x"` comparison, a
/// `matches!(node.kind(), "x" | "y")` arm list, and a comparison against a
/// local bound from `.kind()`. The binding form is restricted to a `let` whose
/// right-hand side is a `.kind()` *call*, so object-kind fields (`info.kind`,
/// which holds `"table"`, `"page"`, …) are not mistaken for node kinds.
fn kind_literals(source: &str) -> BTreeSet<String> {
    let direct = Regex::new(r#"\.kind\(\)\s*(?:==|!=)\s*"([^"]*)""#).unwrap();
    let reversed =
        Regex::new(r#""([^"]*)"\s*(?:==|!=)\s*[A-Za-z_][A-Za-z0-9_.]*\.kind\(\)"#).unwrap();
    let matches_call = Regex::new(r#"matches!\s*\(\s*[^,()]*\.kind\(\)\s*,([^)]*)\)"#).unwrap();
    let binding = Regex::new(
        r#"let\s+([a-z_][a-z0-9_]*)\s*(?::\s*&str)?\s*=\s*[A-Za-z_][A-Za-z0-9_.]*\.kind\(\)\s*;"#,
    )
    .unwrap();
    let literal = Regex::new(r#""([^"]*)""#).unwrap();

    let mut found = BTreeSet::new();
    let push_arms = |arms: &str, found: &mut BTreeSet<String>| {
        for capture in literal.captures_iter(arms) {
            found.insert(capture[1].to_string());
        }
    };

    for capture in direct.captures_iter(source) {
        found.insert(capture[1].to_string());
    }
    for capture in reversed.captures_iter(source) {
        found.insert(capture[1].to_string());
    }
    for capture in matches_call.captures_iter(source) {
        push_arms(&capture[1], &mut found);
    }

    let bound: BTreeSet<String> = binding
        .captures_iter(source)
        .map(|capture| capture[1].to_string())
        .collect();
    for name in bound {
        let escaped = regex::escape(&name);
        let compare = Regex::new(&format!(r#"\b{escaped}\s*(?:==|!=)\s*"([^"]*)""#)).unwrap();
        for capture in compare.captures_iter(source) {
            found.insert(capture[1].to_string());
        }
        let arms = Regex::new(&format!(r#"matches!\s*\(\s*{escaped}\s*,([^)]*)\)"#)).unwrap();
        for capture in arms.captures_iter(source) {
            push_arms(&capture[1], &mut found);
        }
    }
    found
}

#[test]
fn every_node_kind_literal_exists_in_the_grammar() {
    let kinds = grammar_kinds();
    assert!(
        kinds.contains("procedure_declaration"),
        "grammar kind table did not load"
    );

    let mut files = Vec::new();
    for dir in crate_src_dirs() {
        rust_files(&dir, &mut files);
    }
    files.sort();
    assert!(
        files.len() > 20,
        "expected to scan both crates, found {} files",
        files.len()
    );

    let mut absent: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut scanned = 0usize;
    for path in &files {
        let source = std::fs::read_to_string(path).unwrap();
        let label = path
            .components()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("/");
        for literal in kind_literals(&source) {
            scanned += 1;
            if kinds.contains(&literal) {
                continue;
            }
            if ALLOWED_ABSENT
                .iter()
                .any(|(file, kind)| label.ends_with(file) && *kind == literal)
            {
                continue;
            }
            absent.entry(label.clone()).or_default().insert(literal);
        }
    }
    assert!(
        scanned > 30,
        "the scan found almost no kind literals; the patterns have rotted"
    );

    if !absent.is_empty() {
        let report = absent
            .iter()
            .map(|(file, kinds)| {
                format!(
                    "  {file}: {}",
                    kinds.iter().cloned().collect::<Vec<_>>().join(", ")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        panic!(
            "These node kinds are compared against `kind()` but do not exist in \
             tree-sitter-al, so the branch behind each is dead:\n{report}\n\n\
             Check tree-sitter-al/src/node-types.json for the real spelling."
        );
    }
}

#[test]
fn the_scan_finds_a_misspelled_kind() {
    let source = r#"
        if node.kind() == "procedure_declaration" { }
        if matches!(other.kind(), "attribute" | "not_a_real_kind") { }
    "#;
    let found = kind_literals(source);
    assert!(found.contains("procedure_declaration"));
    assert!(found.contains("attribute"));
    assert!(found.contains("not_a_real_kind"));
    assert!(!grammar_kinds().contains("not_a_real_kind"));
}

#[test]
fn an_object_kind_field_is_not_read_as_a_node_kind() {
    // `CachedObjectInfo::kind` holds "table"/"page"/…, not a grammar node kind.
    let source = r#"
        let kind = info.kind.clone();
        if kind == "tableextension" { }
    "#;
    assert!(kind_literals(source).is_empty());
}
