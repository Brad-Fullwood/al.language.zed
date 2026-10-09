//! Every tree-sitter node kind and field name that the local test interpreter
//! (`al-runtime/src/interpreter/`) and the test router
//! (`al-test/src/router/ast.rs`) name must exist in the AL grammar.
//!
//! A misspelled kind or field compiles and runs, and the branch behind it
//! never matches. The interpreter and router matched eighteen kinds and looked
//! up two fields that tree-sitter-al does not have. The same guard for
//! al-analysis and al-insight is `al-analysis/tests/node_kind_literals.rs`.
//! This one also reads the arms of `match node.kind() { ... }` and the names
//! passed to `child_by_field_name`.
//!
//! The grammar's own tables (`Language::node_kind_for_id`,
//! `Language::field_name_for_id`) are the reference, so this test tracks a
//! grammar bump without regeneration.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use regex::Regex;

fn grammar_kinds() -> BTreeSet<String> {
    let language = al_syntax::parser::language();
    (0..language.node_kind_count())
        .filter_map(|id| language.node_kind_for_id(id as u16))
        .map(str::to_string)
        .collect()
}

fn grammar_fields() -> BTreeSet<String> {
    let language = al_syntax::parser::language();
    (1..=language.field_count())
        .filter_map(|id| language.field_name_for_id(id as u16))
        .map(str::to_string)
        .collect()
}

fn scanned_files() -> Vec<PathBuf> {
    let crates = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let mut files = Vec::new();
    rust_files(
        &crates.join("al-runtime").join("src").join("interpreter"),
        &mut files,
    );
    files.push(
        crates
            .join("al-test")
            .join("src")
            .join("router")
            .join("ast.rs"),
    );
    files.sort();
    files
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

/// Every string literal compared against a tree-sitter `kind()`.
///
/// Six shapes carry a kind: `node.kind() == "x"`, `"x" == node.kind()`,
/// `matches!(node.kind(), "x" | "y")`, a comparison against a local bound
/// from a `.kind()` call, the arms of `match node.kind() { ... }`, and the
/// kind list passed to the interpreter's `child_by_field_or_kind`.
fn kind_literals(source: &str) -> BTreeSet<String> {
    let direct = Regex::new(r#"\.kind\(\)\s*(?:==|!=)\s*"([^"]*)""#).unwrap();
    let reversed =
        Regex::new(r#""([^"]*)"\s*(?:==|!=)\s*[A-Za-z_][A-Za-z0-9_.]*\.kind\(\)"#).unwrap();
    let matches_call = Regex::new(r#"matches!\s*\(\s*[^,()]*\.kind\(\)\s*,([^)]*)\)"#).unwrap();
    let binding = Regex::new(
        r#"let\s+([a-z_][a-z0-9_]*)\s*(?::\s*&str)?\s*=\s*[A-Za-z_][A-Za-z0-9_.]*\.kind\(\)\s*;"#,
    )
    .unwrap();
    let kind_list =
        Regex::new(r#"child_by_field_or_kind\(\s*[^,]*,\s*"[^"]*",\s*&\[([^\]]*)\]"#).unwrap();
    let literal = Regex::new(r#""([^"]*)""#).unwrap();

    let mut found = BTreeSet::new();
    let push_literals = |text: &str, found: &mut BTreeSet<String>| {
        for capture in literal.captures_iter(text) {
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
        push_literals(&capture[1], &mut found);
    }
    for pattern in match_arm_patterns(source) {
        push_literals(&pattern, &mut found);
    }
    for capture in kind_list.captures_iter(source) {
        push_literals(&capture[1], &mut found);
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
            push_literals(&capture[1], &mut found);
        }
    }
    found
}

/// The pattern text of each arm of a `match <expr>.kind() {` block that
/// starts with a string literal: the text before `=>`.
///
/// The arms are the lines at the indentation of the first line inside the
/// block, as rustfmt lays them out. A pattern that rustfmt splits over lines
/// continues with `| "x"` at the same indentation. Arms at deeper indentation
/// belong to a nested `match` on something else (`"true" =>` on a name's
/// text) and are skipped.
fn match_arm_patterns(source: &str) -> Vec<String> {
    let opener = Regex::new(r"match\s+[A-Za-z_][A-Za-z0-9_.]*\.kind\(\)\s*\{\s*$").unwrap();
    let lines: Vec<&str> = source.lines().collect();
    let indent = |line: &str| line.len() - line.trim_start().len();
    let mut patterns = Vec::new();
    for (at, line) in lines.iter().enumerate() {
        if !opener.is_match(line) {
            continue;
        }
        let mut arm_indent = None;
        let mut pattern = String::new();
        for line in &lines[at + 1..] {
            let text = line.trim();
            if text.is_empty() || text.starts_with("//") {
                continue;
            }
            let depth = indent(line);
            let arms = *arm_indent.get_or_insert(depth);
            if depth < arms {
                break;
            }
            if depth > arms {
                continue;
            }
            if pattern.is_empty() && !text.starts_with('"') {
                continue;
            }
            pattern.push(' ');
            pattern.push_str(text);
            if let Some((head, _)) = pattern.split_once("=>") {
                patterns.push(head.to_string());
                pattern.clear();
            }
        }
    }
    patterns
}

/// Every field name passed to `child_by_field_name` as a literal.
fn field_literals(source: &str) -> BTreeSet<String> {
    let field = Regex::new(r#"child_by_field_name\(\s*"([^"]*)"\s*\)"#).unwrap();
    field
        .captures_iter(source)
        .map(|capture| capture[1].to_string())
        .collect()
}

fn label(path: &Path) -> String {
    path.components()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("/")
}

#[test]
fn every_node_kind_and_field_name_exists_in_the_grammar() {
    let kinds = grammar_kinds();
    let fields = grammar_fields();
    assert!(
        kinds.contains("procedure_declaration"),
        "grammar kind table did not load"
    );
    assert!(fields.contains("name"), "grammar field table did not load");

    let files = scanned_files();
    assert!(
        files.len() > 20,
        "expected to scan the interpreter and the router, found {} files",
        files.len()
    );

    let mut absent: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let (mut scanned_kinds, mut scanned_fields) = (0usize, 0usize);
    for path in &files {
        let source = std::fs::read_to_string(path).unwrap();
        for literal in kind_literals(&source) {
            scanned_kinds += 1;
            if !kinds.contains(&literal) {
                absent
                    .entry(label(path))
                    .or_default()
                    .insert(format!("kind {literal}"));
            }
        }
        for literal in field_literals(&source) {
            scanned_fields += 1;
            if !fields.contains(&literal) {
                absent
                    .entry(label(path))
                    .or_default()
                    .insert(format!("field {literal}"));
            }
        }
    }
    assert!(
        scanned_kinds > 60 && scanned_fields > 10,
        "the scan found almost no kind or field literals ({scanned_kinds}, {scanned_fields}), \
         so the patterns no longer match the code"
    );

    if !absent.is_empty() {
        let report = absent
            .iter()
            .map(|(file, names)| {
                format!(
                    "  {file}: {}",
                    names.iter().cloned().collect::<Vec<_>>().join(", ")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        panic!(
            "These node kinds or field names are matched but do not exist in \
             tree-sitter-al, so the branch behind each is dead:\n{report}\n\n\
             Check tree-sitter-al/src/node-types.json for the real names."
        );
    }
}

#[test]
fn the_scan_reads_match_arms_and_field_names() {
    let source = r#"
    match node.kind() {
        "procedure_declaration" | "not_a_real_kind" => {
            match name.as_str() {
                "true" => {}
                _ => {}
            }
        }
        "attribute"
        | "another_fake_kind" => {}
        other => {}
    }
    let value = node.child_by_field_name("not_a_real_field");
    let type_node = child_by_field_or_kind(child, "type", &["type_reference", "listed_fake_kind"]);
"#;
    let found = kind_literals(source);
    for kind in [
        "procedure_declaration",
        "not_a_real_kind",
        "attribute",
        "another_fake_kind",
        "type_reference",
        "listed_fake_kind",
    ] {
        assert!(found.contains(kind), "missed {kind} in {found:?}");
    }
    assert!(
        !found.contains("true"),
        "read a nested match on a name as a kind"
    );
    assert!(field_literals(source).contains("not_a_real_field"));
    assert!(!grammar_kinds().contains("not_a_real_kind"));
    assert!(!grammar_fields().contains("not_a_real_field"));
}
