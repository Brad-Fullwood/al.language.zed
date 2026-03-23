//! Duplicate code detection.
//!
//! T1708: Find structurally similar code blocks across AL source files using AST comparison.
//! Uses normalized procedure body hashing to detect duplicates.

use serde::Serialize;
use std::collections::HashMap;

use al_syntax::AlParser;

use crate::workspace::Workspace;

/// A pair of duplicate/similar code blocks.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateBlock {
    /// First occurrence.
    pub first: BlockLocation,
    /// Second occurrence.
    pub second: BlockLocation,
    /// Similarity score 0.0..=1.0 (1.0 = exact).
    pub similarity: f32,
    /// Number of normalized tokens in common.
    pub token_count: usize,
}

/// Location of a code block.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockLocation {
    pub file: String,
    pub object: String,
    pub procedure: String,
    pub line: u32,
}

/// Find duplicate/similar procedures across workspace files.
///
/// Minimum similarity threshold and minimum token count can be configured.
pub fn find_duplicates(workspace: &Workspace, min_tokens: usize, min_similarity: f32) -> Vec<DuplicateBlock> {
    // Extract procedure bodies from all workspace files
    let mut procedures: Vec<ProcedureBody> = Vec::new();

    for entry in workspace.file_index.files.iter() {
        let file_path = entry.key().to_string_lossy().to_string();
        let text = entry.value();
        let parsed = AlParser::parse_quick(text);

        let Some(obj_info) = al_syntax::find_object_declaration(&parsed.tree, text) else {
            continue;
        };

        collect_procedure_bodies(
            &file_path,
            text,
            &obj_info.name,
            &parsed.tree,
            &mut procedures,
        );
    }

    // Compare all pairs
    let mut duplicates = Vec::new();
    for i in 0..procedures.len() {
        for j in (i + 1)..procedures.len() {
            let a = &procedures[i];
            let b = &procedures[j];

            // Skip trivially short procedures
            let shorter = a.tokens.len().min(b.tokens.len());
            if shorter < min_tokens { continue; }

            // Skip identical procedure names in the same object (same proc, different file sections)
            if a.location.object == b.location.object && a.location.procedure == b.location.procedure {
                continue;
            }

            let similarity = compute_similarity(&a.tokens, &b.tokens);
            let token_count = ((a.tokens.len() + b.tokens.len()) as f32 / 2.0 * similarity) as usize;

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

    // Sort by similarity descending
    duplicates.sort_by(|a, b| b.similarity.partial_cmp(&a.similarity).unwrap_or(std::cmp::Ordering::Equal));
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
                let name = node
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source).ok())
                    .unwrap_or("(unknown)")
                    .trim_matches('"')
                    .to_string();

                let line = node.start_position().row as u32 + 1;

                // Extract normalized tokens from the procedure body
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
                // Skip children of procedure/trigger declarations
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
    if !node.is_named() {
        // Punctuation/keywords — keep as-is
        if let Ok(text) = node.utf8_text(source) {
            let lower = text.to_lowercase();
            tokens.push(lower);
        }
        return;
    }

    match node.kind() {
        "identifier" | "name" => {
            // Normalize identifiers to their category
            tokens.push("$ID".to_string());
        }
        "string" | "verbatim_string" => {
            tokens.push("$STR".to_string());
        }
        "integer" | "decimal" => {
            tokens.push("$NUM".to_string());
        }
        "comment" => {
            // Skip comments in similarity analysis
        }
        _ => {
            // Recurse into children
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                collect_tokens(child, source, tokens);
            }
        }
    }
}

/// Compute similarity using Jaccard-like coefficient on token bigrams.
fn compute_similarity(a: &[String], b: &[String]) -> f32 {
    if a.is_empty() && b.is_empty() { return 1.0; }
    if a.is_empty() || b.is_empty() { return 0.0; }

    // Build bigram bags
    let a_bigrams = bigrams(a);
    let b_bigrams = bigrams(b);

    let total: usize = a_bigrams.values().sum::<usize>() + b_bigrams.values().sum::<usize>();
    if total == 0 { return 0.0; }

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
    use crate::workspace::Workspace;
    use std::path::PathBuf;

    fn workspace_with(files: Vec<(&str, &str)>) -> Workspace {
        let ws = Workspace::new();
        for (name, content) in files {
            ws.file_index.add_file(PathBuf::from(name), content.to_string());
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

        // With high threshold, no duplicates for clearly different code
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
