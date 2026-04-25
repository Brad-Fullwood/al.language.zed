// Architecture boundary tests for al-core.
//
// al-core must NOT embed tower_lsp types in its core data structures —
// the dependency must flow downward only (al-lsp → al-core, not al-core → tower_lsp
// data structures).

use std::fs;

/// Reproduces: ccf4330ce2668b75 — CachedProcedureInfo embeds tower_lsp::lsp_types::Range
/// directly inside al-core's file_index.rs, violating the "no LSP types in al-core
/// core data structures" rule from CLAUDE.md.
#[test]
fn test_cached_procedure_info_does_not_embed_tower_lsp_range() {
    let source = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/file_index.rs"))
        .expect("failed to read file_index.rs");

    // Find the CachedProcedureInfo struct block.
    // We look for the pattern `tower_lsp::lsp_types::Range` appearing as a field
    // type inside that struct. A simple scan of the entire file is sufficient
    // because file_index.rs is the only place this struct is defined.
    let has_violation = source.contains("tower_lsp::lsp_types::Range");

    assert!(
        !has_violation,
        "CachedProcedureInfo in file_index.rs still has a field typed \
         `tower_lsp::lsp_types::Range`. al-core data structures must not \
         embed tower_lsp types. Replace with a crate-local range type \
         (e.g., `al_core::queries::Range` or a plain `[u32; 4]`)."
    );
}

/// Negative guard: ensure the test file itself can find the source it is scanning
/// (detects mis-configured CARGO_MANIFEST_DIR or missing file).
#[test]
fn test_file_index_source_is_readable() {
    let result = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/file_index.rs"));
    assert!(
        result.is_ok(),
        "Could not read crates/al-core/src/file_index.rs — path assumption is wrong"
    );
}
