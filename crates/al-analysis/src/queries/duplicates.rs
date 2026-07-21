//! Duplicate code detection.
//!
//! Find structurally similar code blocks across AL source files using AST comparison.
//! Uses normalized procedure body hashing to detect duplicates.

use serde::Serialize;
use std::collections::HashMap;

use al_workspace::Workspace;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateBlock {
    pub first: BlockLocation,
    pub second: BlockLocation,
    /// Similarity score 0.0..=1.0 (1.0 = exact).
    pub similarity: f32,
    /// Number of normalized tokens in common.
    pub token_count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockLocation {
    pub file: String,
    pub object: String,
    pub procedure: String,
    pub line: u32,
}

pub fn find_duplicates(
    workspace: &Workspace,
    min_tokens: usize,
    min_similarity: f32,
) -> Vec<DuplicateBlock> {
    let mut procedures: Vec<ProcedureBody> = Vec::new();

    for entry in workspace.file_index.files.iter() {
        let path = entry.key().clone();
        let file_path = path.to_string_lossy().to_string();
        drop(entry);
        let Some((text, tree)) = workspace.file_index.get_cached_parse(&path) else {
            continue;
        };

        let Some(obj_info) = al_syntax::find_object_declaration(&tree, &text) else {
            continue;
        };

        collect_procedure_bodies(&file_path, &text, &obj_info.name, &tree, &mut procedures);
    }

    let mut duplicates = Vec::new();
    for i in 0..procedures.len() {
        for j in (i + 1)..procedures.len() {
            let a = &procedures[i];
            let b = &procedures[j];

            let shorter = a.tokens.len().min(b.tokens.len());
            if shorter < min_tokens {
                continue;
            }

            if a.location.object == b.location.object
                && a.location.procedure == b.location.procedure
            {
                continue;
            }

            let similarity = compute_similarity(&a.tokens, &b.tokens);
            let token_count =
                ((a.tokens.len() + b.tokens.len()) as f32 / 2.0 * similarity) as usize;

            if similarity >= min_similarity {
                duplicates.push(DuplicateBlock {
                    first: a.location.clone(),
                    second: b.location.clone(),
                    similarity,
                    token_count,
                });
            }
        }
    }

    duplicates.sort_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    duplicates
}

struct ProcedureBody {
    location: BlockLocation,
    tokens: Vec<String>,
}

fn collect_procedure_bodies(
    file_path: &str,
    text: &str,
    object_name: &str,
    tree: &tree_sitter::Tree,
    procedures: &mut Vec<ProcedureBody>,
) {
    let root = tree.root_node();
    let source = text.as_bytes();
    collect_procs_recursive(root, source, file_path, object_name, procedures);
}

fn collect_procs_recursive(
    root: tree_sitter::Node,
    source: &[u8],
    file_path: &str,
    object_name: &str,
    procedures: &mut Vec<ProcedureBody>,
) {
    let mut cursor = root.walk();
    let mut did_visit = false;
    loop {
        if !did_visit {
            let node = cursor.node();
            if matches!(node.kind(), "procedure_declaration" | "trigger_declaration") {
                let name = al_syntax::node_name_or(node, source, "(unknown)");

                let line = node.start_position().row as u32 + 1;

                let tokens = extract_normalized_tokens(node, source);

                procedures.push(ProcedureBody {
                    location: BlockLocation {
                        file: file_path.to_string(),
                        object: object_name.to_string(),
                        procedure: name,
                        line,
                    },
                    tokens,
                });
                did_visit = true;
                continue;
            }
        }
        if !did_visit && cursor.goto_first_child() {
            did_visit = false;
            continue;
        }
        if cursor.goto_next_sibling() {
            did_visit = false;
            continue;
        }
        if cursor.goto_parent() {
            did_visit = true;
            continue;
        }
        break;
    }
}

/// Extract normalized tokens from a node, replacing variable names with
/// type-based placeholders to detect structural similarity.
fn extract_normalized_tokens(node: tree_sitter::Node, source: &[u8]) -> Vec<String> {
    let mut tokens = Vec::new();
    collect_tokens(node, source, &mut tokens);
    tokens
}

fn collect_tokens(node: tree_sitter::Node, source: &[u8], tokens: &mut Vec<String>) {
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if !current.is_named() {
            if let Ok(text) = current.utf8_text(source) {
                let lower = text.to_lowercase();
                tokens.push(lower);
            }
            continue;
        }

        match current.kind() {
            "identifier" | "name" => {
                tokens.push("$ID".to_string());
            }
            "string" | "verbatim_string" => {
                tokens.push("$STR".to_string());
            }
            "integer" | "decimal" => {
                tokens.push("$NUM".to_string());
            }
            "comment" => {}
            _ => {
                for i in (0..current.child_count()).rev() {
                    if let Some(child) = current.child(i) {
                        stack.push(child);
                    }
                }
            }
        }
    }
}

/// Compute similarity using Jaccard-like coefficient on token bigrams.
fn compute_similarity(a: &[String], b: &[String]) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }

    let a_bigrams = bigrams(a);
    let b_bigrams = bigrams(b);

    let total: usize = a_bigrams.values().sum::<usize>() + b_bigrams.values().sum::<usize>();
    if total == 0 {
        return 0.0;
    }

    let mut intersection = 0usize;
    for (bigram, count) in &a_bigrams {
        if let Some(&b_count) = b_bigrams.get(bigram) {
            intersection += count.min(&b_count);
        }
    }

    2.0 * intersection as f32 / total as f32
}

fn bigrams(tokens: &[String]) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for window in tokens.windows(2) {
        let key = format!("{}|{}", window[0], window[1]);
        *counts.entry(key).or_insert(0) += 1;
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_workspace::Workspace;
    use std::path::PathBuf;

    fn workspace_with(files: Vec<(&str, &str)>) -> Workspace {
        let ws = Workspace::new();
        for (name, content) in files {
            ws.file_index
                .add_file(PathBuf::from(name), content.to_string());
        }
        ws
    }

    #[test]
    fn finds_identical_procedures() {
        let ws = workspace_with(vec![
            (
                "/src/A.al",
                r#"codeunit 50100 "Codeunit A"
{
    procedure ProcessRecord()
    var
        x: Integer;
    begin
        x := 1;
        if x > 0 then
            Message('positive');
        x := x + 1;
    end;
}"#,
            ),
            (
                "/src/B.al",
                r#"codeunit 50101 "Codeunit B"
{
    procedure ProcessRecord()
    var
        y: Integer;
    begin
        y := 1;
        if y > 0 then
            Message('positive');
        y := y + 1;
    end;
}"#,
            ),
        ]);

        let dups = find_duplicates(&ws, 5, 0.8);
        assert!(!dups.is_empty(), "Should find duplicate procedures");
        assert!(dups[0].similarity >= 0.8, "Similarity should be high");
    }

    #[test]
    fn no_duplicates_in_unique_code() {
        let ws = workspace_with(vec![
            (
                "/src/A.al",
                r#"codeunit 50100 "A"
{
    procedure DoA()
    begin
        Message('A');
    end;
}"#,
            ),
            (
                "/src/B.al",
                r#"codeunit 50101 "B"
{
    procedure DoB()
    begin
        Error('different error message from a completely different flow');
        Error('second error');
        Error('third error');
    end;
}"#,
            ),
        ]);

        let dups = find_duplicates(&ws, 10, 0.9);
        assert!(dups.is_empty(), "Unique code should have no duplicates");
    }

    #[test]
    fn empty_workspace_no_duplicates() {
        let ws = Workspace::new();
        let dups = find_duplicates(&ws, 5, 0.8);
        assert!(dups.is_empty());
    }

    #[test]
    fn similarity_of_identical_tokens() {
        let tokens: Vec<String> = vec!["if".to_string(), "$ID".to_string(), "then".to_string()];
        let sim = compute_similarity(&tokens, &tokens);
        assert_eq!(sim, 1.0);
    }

    #[test]
    fn similarity_of_disjoint_tokens() {
        let a: Vec<String> = vec!["if".to_string(), "$ID".to_string()];
        let b: Vec<String> = vec!["while".to_string(), "$NUM".to_string()];
        let sim = compute_similarity(&a, &b);
        assert_eq!(sim, 0.0);
    }
}
