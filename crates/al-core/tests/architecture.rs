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

/// Reproduces: eb12bf9fb43d529d — FileIndex.file_symbols and get_cached_symbols use
/// tower_lsp::lsp_types::DocumentSymbol as the stored/returned type inside al-core,
/// violating the dependency-direction rule (al-core must not embed LSP wire types).
#[test]
fn test_file_index_does_not_embed_tower_lsp_document_symbol() {
    let source = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/file_index.rs"))
        .expect("failed to read file_index.rs");

    // Check the file_symbols field declaration line specifically.
    // We scan line by line so we only flag the field declaration, not comments.
    let field_violation = source.lines().any(|line| {
        // The line must declare file_symbols as a DashMap containing the LSP wire type.
        line.contains("file_symbols") && line.contains("tower_lsp::lsp_types::DocumentSymbol")
    });
    assert!(
        !field_violation,
        "FileIndex.file_symbols in file_index.rs stores \
         `tower_lsp::lsp_types::DocumentSymbol` — an LSP wire type — directly \
         inside al-core. Replace with a crate-local `AlDocumentSymbol` type and \
         convert to DocumentSymbol at the al-lsp boundary."
    );

    // Also check get_cached_symbols return type.
    let return_violation = source.lines().any(|line| {
        line.contains("get_cached_symbols") && line.contains("tower_lsp::lsp_types::DocumentSymbol")
    });
    assert!(
        !return_violation,
        "get_cached_symbols in file_index.rs returns \
         `tower_lsp::lsp_types::DocumentSymbol` — an LSP wire type. \
         Return a crate-local type instead and convert at the al-lsp boundary."
    );
}

/// Reproduces: ef9c083a9859bf71 — Three private helpers in resolution.rs return
/// `Vec<tower_lsp::lsp_types::CompletionItem>` directly from al-core, violating the
/// CLAUDE.md rule that al-core query/resolution helpers must not return LSP wire types.
/// The helpers `completion_items_for_receiver`, `enum_completion_items`, and
/// `workspace_field_items` must return a crate-local `CompletionCandidate` type instead.
///
/// Strategy: for each helper, find its `fn <name>` position in the source, then scan
/// the next 10 lines for the return type.  This handles multi-line signatures.
#[test]
fn test_resolution_helpers_do_not_return_tower_lsp_completion_item() {
    let source = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/resolution.rs"))
        .expect("failed to read resolution.rs");

    let helpers = [
        "completion_items_for_receiver",
        "enum_completion_items",
        "workspace_field_items",
    ];

    for helper in helpers {
        let needle = format!("fn {helper}");
        // Find the byte offset of the fn declaration.
        let fn_pos = source.find(&needle).unwrap_or_else(|| {
            panic!("resolution.rs: could not find `fn {helper}` — test assumption broken")
        });
        // Grab the 300 bytes after the `fn` keyword to cover the full signature
        // including multi-line parameter lists and the return type arrow.
        let window_end = (fn_pos + 300).min(source.len());
        let window = &source[fn_pos..window_end];

        // The opening brace `{` marks the start of the body; return type must appear before it.
        let sig = if let Some(brace) = window.find('{') {
            &window[..brace]
        } else {
            window
        };

        let violation = sig.contains("tower_lsp::lsp_types::CompletionItem");
        assert!(
            !violation,
            "resolution.rs: `fn {helper}` has `tower_lsp::lsp_types::CompletionItem` in its \
             return type — an LSP wire type must not appear in al-core helper signatures. \
             Define a crate-local `CompletionCandidate` struct and convert at the al-lsp boundary."
        );
    }
}

/// Negative guard: resolution.rs must be readable (detects path misconfiguration).
#[test]
fn test_resolution_source_is_readable() {
    let result = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/resolution.rs"));
    assert!(
        result.is_ok(),
        "Could not read crates/al-core/src/resolution.rs — path assumption is wrong"
    );
}

/// Reproduces: ca6efab0bb29e004 — `document_symbols()` in queries/symbols.rs
/// previously returned `tower_lsp::lsp_types::DocumentSymbolResponse`. Query
/// functions must return transport-agnostic types.
#[test]
fn test_document_symbols_does_not_return_tower_lsp_response() {
    let source = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/queries/symbols.rs"
    ))
    .expect("failed to read queries/symbols.rs");
    let fn_pos = source
        .find("fn document_symbols")
        .expect("could not find document_symbols");
    let window = &source[fn_pos..(fn_pos + 400).min(source.len())];
    let sig = window.split('{').next().unwrap_or(window);
    assert!(
        !sig.contains("tower_lsp::lsp_types::DocumentSymbolResponse"),
        "document_symbols still returns tower_lsp::lsp_types::DocumentSymbolResponse. \
         Return Vec<AlDocumentSymbol> instead and convert at the al-lsp boundary."
    );
}

/// Reproduces: 8e48b967d935bfd5 — `get_or_build_insight_graph` in
/// `workspace.rs` previously held a `std::sync::RwLock` write guard
/// across `graph.build_from_index(...)`. The expensive build must run
/// without any lock held to avoid parking the tokio executor.
#[test]
fn test_get_or_build_insight_graph_does_not_hold_write_lock_across_build() {
    let source = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/workspace.rs"))
        .expect("failed to read workspace.rs");

    let fn_pos = source
        .find("fn get_or_build_insight_graph")
        .expect("could not find get_or_build_insight_graph");
    let window_end = (fn_pos + 1500).min(source.len());
    let window = &source[fn_pos..window_end];

    // Find the first `.write()` call after the fn declaration and the
    // `build_from_index` call. The fix moves `build_from_index` BEFORE the
    // write guard is taken, so build_from_index must appear before .write().
    let build_pos = window.find("build_from_index");
    let write_pos = window.find(".write()");
    if let (Some(b), Some(w)) = (build_pos, write_pos) {
        assert!(
            b < w,
            "get_or_build_insight_graph still calls `build_from_index` AFTER \
             acquiring the write lock — meaning the std::sync::RwLock write guard \
             is held across the expensive build. Move the build before the write \
             lock acquisition to avoid parking the tokio executor."
        );
    }
}

/// Reproduces: 2e506b918924b167 — `register_procedures_from_tree` in
/// `insight/calls.rs` was a self-recursive tree-sitter walker. Same risk
/// as collect_call_sites_from_block: must be iterative.
#[test]
fn test_register_procedures_from_tree_is_iterative() {
    let source = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/insight/calls.rs"))
        .expect("failed to read insight/calls.rs");

    let fn_pos = source
        .find("fn register_procedures_from_tree")
        .expect("could not find register_procedures_from_tree");
    let window_end = (fn_pos + 2000).min(source.len());
    let window = &source[fn_pos..window_end];
    let after_decl = &window[window.find('{').unwrap_or(0)..];

    assert!(
        !after_decl.contains("register_procedures_from_tree("),
        "register_procedures_from_tree in insight/calls.rs is recursive. \
         CLAUDE.md requires iterative tree-sitter traversal — rewrite using \
         an explicit `Vec<Node>` stack."
    );
}

/// Reproduces: 27f075b6af883ba3 — `collect_call_sites_from_block` in
/// `insight/calls.rs` was a self-recursive tree-sitter walker. CLAUDE.md
/// requires iterative traversal (explicit stack) for tree-sitter nodes to
/// avoid stack overflow on deeply nested AL.
#[test]
fn test_collect_call_sites_from_block_is_iterative() {
    let source = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/insight/calls.rs"))
        .expect("failed to read insight/calls.rs");

    // Locate the function body.
    let fn_pos = source
        .find("fn collect_call_sites_from_block")
        .expect("could not find collect_call_sites_from_block");
    // Grab a generous window for the function body.
    let window_end = (fn_pos + 1500).min(source.len());
    let window = &source[fn_pos..window_end];

    // The body must NOT call itself — that is the recursion this test guards against.
    // Skip the first occurrence (the fn declaration itself) and check for any
    // subsequent self-call.
    let after_decl = &window[window.find('{').unwrap_or(0)..];
    assert!(
        !after_decl.contains("collect_call_sites_from_block("),
        "collect_call_sites_from_block in insight/calls.rs is recursive. \
         CLAUDE.md requires iterative tree-sitter traversal — rewrite using \
         an explicit `Vec<Node>` stack."
    );
}
