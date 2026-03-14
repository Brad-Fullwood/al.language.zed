# Adversarial Atlas

## Purpose

This document is the authoritative catalog of adversarial tests, stress scenarios, fidelity gaps, and failure modes that the `al-test-harness` must cover. It serves as the project's quality contract: every entry here must have a corresponding test, and every test must be capable of failing. If the harness stops producing failures, the harness is brittle and must be hardened before any feature work continues.

This atlas is a living document. New entries are added whenever a gap is discovered, a regression occurs, or a Work Package introduces new attack surface.

## Stress Test Catalog

### ST-01: Large Symbol Files

- **Scenario**: .app file with 50MB+ SymbolReference.json (e.g., full Base Application ~25k objects)
- **Attack vectors**: memory exhaustion during parse, slow symbol index build, OOM in WASM
- **Pass criteria**: index builds in <30s, memory stays under 500MB, no OOM
- **Test method**: use real Microsoft_Base Application.app from BC26

### ST-02: Deep Event Recursion

- **Scenario**: event publisher triggers subscriber which publishes another event, 10+ levels deep
- **Attack vectors**: stack overflow in graph traversal, infinite loops in cyclic event chains
- **Pass criteria**: cycle detection terminates, graph truncates at configurable depth (default 20)
- **Test method**: synthetic fixture with cyclic event chain

### ST-03: Concurrent LSP Requests

- **Scenario**: 50+ simultaneous textDocument/* requests during active editing
- **Attack vectors**: race conditions in DocumentStore, deadlocks in DashMap, stale parse trees
- **Pass criteria**: no panics, no deadlocks (5s timeout), correct responses for all requests
- **Test method**: tokio::spawn flood of parallel requests via test harness

### ST-04: Malformed .app Files

- **Scenario**: corrupted NAVX header, truncated ZIP, invalid JSON in SymbolReference
- **Attack vectors**: panic on unwrap, infinite loop in ZIP parsing, serde deserialization crash
- **Pass criteria**: AlError::PackageCorrupt returned, other packages still load, no panic
- **Test method**: synthetic corrupt .app files (each corruption type)

### ST-05: Unicode Edge Cases

- **Scenario**: AL identifiers with Unicode (e.g., Norwegian ø, German ü, emoji in comments), BOM in .al files
- **Attack vectors**: byte offset vs char offset mismatch, tree-sitter position errors, URI encoding issues
- **Pass criteria**: correct positions in hover/definition/references, no off-by-one errors
- **Test method**: fixture .al files with multi-byte characters at known positions

### ST-06: .NET Bridge Crash Recovery

- **Scenario**: in-process CLR bridge (via `netcorehost`) throws unhandled exception, corrupts state, or hangs mid-request. NOTE: the bridge is NOT a subprocess — it is in-process CLR hosting. Failure means the host Rust process itself may be affected.
- **Attack vectors**: CLR exception during reflection call, bridge state corruption, deadlock in function pointer call, malformed JSON response from AlBridge.dll
- **Pass criteria**: bridge enters Failed state, `BridgeCrashed` error returned to pending requests, al-lsp continues serving syntax-only features without the bridge, bridge can be re-initialized (max 3 attempts)
- **Test method**: inject malformed response from bridge, trigger exception via invalid analyzer path, verify graceful degradation to syntax-only mode

### ST-07: Multi-Root Workspace

- **Scenario**: Zed project with 3 AL projects (different app.json, overlapping object IDs)
- **Attack vectors**: symbol collision between projects, wrong project context for queries, stale cross-project references
- **Pass criteria**: each project resolves symbols independently, no cross-contamination
- **Test method**: fixture with 3 app.json roots

### ST-08: Incremental Parse Corruption

- **Scenario**: rapid file edits (debounce bypass), undo/redo sequences, editing mid-parse
- **Attack vectors**: tree-sitter tree becomes invalid, incremental edit ranges wrong, stale AST for queries
- **Pass criteria**: parse tree always valid after edit, results match fresh parse
- **Test method**: automated edit sequence, then compare incremental vs fresh parse

### ST-09: Empty/Minimal Project

- **Scenario**: project with only app.json (no .al files, no .alpackages)
- **Attack vectors**: index out of bounds, division by zero in metrics, NPE-equivalent panics
- **Pass criteria**: LSP initializes, returns empty results for all queries, no panics
- **Test method**: minimal fixture

### ST-10: Symbol Index Hot Reload

- **Scenario**: user downloads new symbols while LSP is running, .app files change on disk
- **Attack vectors**: partial index state, queries return mix of old/new data, crash during reload
- **Pass criteria**: atomic swap of index, queries block briefly during swap, no mixed results
- **Test method**: swap .alpackages mid-session via test harness

### ST-11: Maximum File Size

- **Scenario**: single .al file with 50,000+ lines (generated codeunit with massive case statements)
- **Attack vectors**: tree-sitter parse timeout, slow semantic tokens, memory pressure
- **Pass criteria**: parse completes in <5s, semantic tokens in <2s, hover in <100ms
- **Test method**: generated large .al file fixture

### ST-12: Tree-sitter Grammar Edge Cases

- **Scenario**: action triggers (trigger OnAction() inside page action blocks), preprocessor directives (#if, #region), multiline string literals @'...'
- **Known gap**: braced_block doesn't include trigger_declaration for action triggers
- **Attack vectors**: missing trigger vars in type resolution, incorrect folding, wrong semantic tokens
- **Pass criteria**: workaround (text-based backwards scanning) correctly extracts action trigger vars
- **Test method**: fixture with each edge case, compare against expected symbols

### ST-13: NuGet Feed Failures

- **Scenario**: network timeout, 401 unauthorized, 404 package not found, rate limiting, DNS failure
- **Attack vectors**: hang on download, unhelpful error messages, retry storms
- **Pass criteria**: each failure mode returns specific AlError variant, timeout of 30s, max 2 retries with backoff
- **Test method**: mock HTTP server with each failure mode

### ST-14: WASM Memory Limits

- **Scenario**: zed-al WASM binary processing large initialization_options or workspace_configuration
- **Attack vectors**: WASM linear memory exhaustion, slow serialization
- **Pass criteria**: extension initializes with <10MB WASM memory, large payloads handled via streaming
- **Test method**: Zed simulation with large settings payload

### ST-15: Workspace with 1000+ AL Files

- **Scenario**: large enterprise project with 1000+ .al files
- **Attack vectors**: slow initial scan, high memory for file index, slow workspace/symbol
- **Pass criteria**: initial index in <5s, workspace/symbol response in <30ms, incremental updates in <100ms
- **Test method**: generated workspace fixture

## Zed Fidelity Gap Register

### FG-01: LSP Initialization Sequence

- **Risk**: Zed sends requests before initialized response
- **Verification**: harness must send requests during initialization and verify they're queued/rejected correctly

### FG-02: URI Encoding

- **Risk**: Zed encodes URIs differently than tower-lsp expects (spaces, special chars)
- **Known issue**: percent-encoding of spaces required for URI matching
- **Verification**: test with file paths containing spaces, unicode, and special characters

### FG-03: Semantic Token Encoding

- **Risk**: Zed expects specific delta encoding for semantic tokens
- **Verification**: compare harness token output against real Zed rendering

### FG-04: Diagnostic Lifecycle

- **Risk**: diagnostics not cleared when file is closed or errors are fixed
- **Verification**: verify diagnostics are published with empty array when cleared

### FG-05: Completion Item Resolve

- **Risk**: Zed may or may not send completionItem/resolve
- **Verification**: test both paths (with and without resolve)

### FG-06: WASM Extension Loading

- **Risk**: zed-al WASM binary fails to load in Zed due to missing WASI imports
- **Verification**: build and load test in Zed dev mode

### FG-07: DAP Adapter Discovery

- **Risk**: get_dap_binary returns wrong path or unsupported adapter type
- **Verification**: test with multiple launch.json configurations

## Adversarial Evolution Protocol

### Continuous Gap Finding

1. After every WP, run the full stress test suite.
2. Any test that has never failed is suspicious -- review if the adversarial input is strong enough.
3. Any success-only entry in proof_of_functionality.toml triggers an audit.

### Fidelity Regression Detection

1. After any change to al-lsp handlers, re-run all Zed simulation tests.
2. Compare LSP responses byte-for-byte against golden files.
3. New LSP features must include both a Zed simulation test and a stress test.

### Gap Reporting

When a gap is found:

1. Add entry to this atlas with a unique ID (ST-XX or FG-XX).
2. Create a failing test in al-test-harness.
3. Report to PM as Priority-0 blocking issue.
4. Do not mark the related WP as complete until the gap is closed.
