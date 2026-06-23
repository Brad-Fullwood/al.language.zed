//! Validates all tree-sitter query files in languages/al/ against the AL grammar.
//!
//! tree-sitter's Query::new() returns an error if a query references a node type
//! that doesn't exist in the grammar, or uses an impossible parent-child pattern.
//! This test catches broken .scm files BEFORE they reach Zed at runtime.

use std::path::PathBuf;
use tree_sitter::Query;

fn al_language() -> tree_sitter::Language {
    al_lsp::syntax::parser::language()
}

fn languages_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("languages")
        .join("al")
}

fn validate_query_file(filename: &str) {
    let path = languages_dir().join(filename);
    if !path.exists() {
        panic!("{filename} does not exist at {}", path.display());
    }
    let source =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("Failed to read {filename}: {e}"));

    if source.trim().is_empty() {
        return; // Empty query files are valid
    }

    match Query::new(&al_language(), &source) {
        Ok(_) => {}
        Err(e) => {
            panic!(
                "Query validation FAILED for {filename}:\n  {e}\n\n\
                 This means {filename} references node types or patterns that don't exist \
                 in the tree-sitter-al grammar. Fix the grammar or fix the query.\n\n\
                 Query source (first 20 lines):\n{}\n",
                source.lines().take(20).collect::<Vec<_>>().join("\n")
            );
        }
    }
}

#[test]
fn validate_highlights_query() {
    validate_query_file("highlights.scm");
}

#[test]
fn validate_brackets_query() {
    validate_query_file("brackets.scm");
}

#[test]
fn validate_outline_query() {
    validate_query_file("outline.scm");
}

#[test]
fn validate_folds_query() {
    validate_query_file("folds.scm");
}

#[test]
fn validate_indents_query() {
    validate_query_file("indents.scm");
}

#[test]
fn validate_locals_query() {
    validate_query_file("locals.scm");
}

#[test]
fn validate_textobjects_query() {
    validate_query_file("textobjects.scm");
}

#[test]
fn validate_injections_query() {
    validate_query_file("injections.scm");
}

#[test]
fn validate_overrides_query() {
    validate_query_file("overrides.scm");
}

#[test]
fn validate_runnables_query() {
    validate_query_file("runnables.scm");
}

/// Catch-all: validate ALL .scm files in languages/al/, including any new ones
/// that might be added in the future without a dedicated test.
#[test]
fn validate_all_query_files() {
    let dir = languages_dir();
    let mut scm_files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", dir.display()))
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".scm") {
                Some(name)
            } else {
                None
            }
        })
        .collect();
    scm_files.sort();

    assert!(
        !scm_files.is_empty(),
        "No .scm files found in {}",
        dir.display()
    );

    let mut failures = Vec::new();
    for filename in &scm_files {
        let path = dir.join(filename);
        let source = std::fs::read_to_string(&path).unwrap();
        if source.trim().is_empty() {
            continue;
        }
        if let Err(e) = Query::new(&al_language(), &source) {
            failures.push(format!("  {filename}: {e}"));
        }
    }

    if !failures.is_empty() {
        panic!(
            "Query validation FAILED for {} of {} .scm files:\n{}\n",
            failures.len(),
            scm_files.len(),
            failures.join("\n")
        );
    }
}
