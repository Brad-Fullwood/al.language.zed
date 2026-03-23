# Code Review Fixes — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix 10 validated issues from analysis.md covering security hardening, correctness bugs (including a live bug in DAP seq handling), robustness improvements, and code simplification.

**Architecture:** All changes are leaf-level — no cross-crate dependency changes. Each task modifies 1-2 files in a single crate. Tasks are fully independent and can be parallelized.

**Tech Stack:** Rust, tokio, tree-sitter, serde, tower-lsp/lsp-types

---

## Task 1: Bounded Line Reading in Daemon

The daemon reads JSON-RPC messages via `lines().next_line()` which buffers an entire line into memory before the `MAX_MESSAGE_SIZE` check at line 194. A malicious local process could send a multi-GB line without a newline, causing OOM before the check fires. 64 concurrent connections × unbounded = catastrophic.

**Files:**
- Modify: `crates/al-lsp/src/daemon/mod.rs:183-197`

- [ ] **Step 1: Write the failing test**

Add a test in `crates/al-lsp/src/daemon/mod.rs` (or a new test module) that sends a line exceeding `MAX_MESSAGE_SIZE` without a newline and verifies the connection is dropped without allocating the full buffer. Since the daemon is a Unix socket server, write an integration-style test.

Actually, the bounded reader is best tested as a unit. Create a helper function `read_bounded_line` and test it directly:

```rust
#[cfg(test)]
mod bounded_read_tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn rejects_line_exceeding_limit() {
        let limit = 1024;
        // Create a stream with a line larger than the limit (no newline)
        let data = vec![b'A'; limit + 100];
        let mut reader = tokio::io::BufReader::new(&data[..]);
        let result = read_bounded_line(&mut reader, limit).await;
        assert!(result.is_err() || result.unwrap().is_none());
    }

    #[tokio::test]
    async fn accepts_line_within_limit() {
        let limit = 1024;
        let mut data = vec![b'A'; 500];
        data.push(b'\n');
        let mut reader = tokio::io::BufReader::new(&data[..]);
        let result = read_bounded_line(&mut reader, limit).await;
        assert!(result.is_ok());
        let line = result.unwrap().unwrap();
        assert_eq!(line.len(), 500);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p al-lsp bounded_read`
Expected: FAIL — `read_bounded_line` doesn't exist yet.

- [ ] **Step 3: Implement bounded line reader**

Replace the unbounded `lines().next_line()` pattern with a manual bounded read. Add this function near line 175:

```rust
/// Read a single newline-delimited line, enforcing a byte limit during reading.
/// Returns `Ok(None)` on EOF, `Err` if the line exceeds `max_bytes`.
async fn read_bounded_line<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    max_bytes: usize,
) -> Result<Option<String>, std::io::Error> {
    let mut buf = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if buf.is_empty() { Ok(None) } else {
                String::from_utf8(buf)
                    .map(Some)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
            };
        }
        if let Some(pos) = available.iter().position(|&b| b == b'\n') {
            buf.extend_from_slice(&available[..pos]);
            reader.consume(pos + 1);
            if buf.len() > max_bytes {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("line exceeds {max_bytes} byte limit"),
                ));
            }
            return String::from_utf8(buf)
                .map(Some)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e));
        }
        let len = available.len();
        buf.extend_from_slice(available);
        reader.consume(len);
        if buf.len() > max_bytes {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("line exceeds {max_bytes} byte limit"),
            ));
        }
    }
}
```

Then update `handle_connection` (lines 183-197) to use it:

```rust
let mut reader = BufReader::new(reader);
// ... (remove: let mut lines = BufReader::new(reader).lines();)

// In the loop, replace:
//   while let Some(line) = lines.next_line().await? {
//       if line.len() > MAX_MESSAGE_SIZE { ... }
// With:
loop {
    let line = match read_bounded_line(&mut reader, MAX_MESSAGE_SIZE).await {
        Ok(Some(line)) => line,
        Ok(None) => break, // EOF
        Err(e) => {
            tracing::warn!(error = %e, "daemon: dropping connection");
            break;
        }
    };
    let line = line.trim().to_string();
    if line.is_empty() { continue; }
    // ... rest unchanged
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p al-lsp bounded_read`
Expected: PASS

- [ ] **Step 5: Run full test suite**

Run: `cargo test --workspace --exclude zed-al`
Expected: All pass. The daemon's behavior is identical for well-formed messages.

- [ ] **Step 6: Commit**

```bash
git add crates/al-lsp/src/daemon/mod.rs
git commit -m "security: bound daemon line reads to prevent OOM on oversized messages"
```

---

## Task 2: Bounded Content-Length in DAP Framing

`read_dap_body` at `framing.rs:17` allocates `vec![0u8; content_length]` where `content_length` comes from the wire with no upper bound. A malformed `Content-Length: 9999999999` causes immediate OOM.

**Files:**
- Modify: `crates/al-dap-client/src/framing.rs:15-20`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn rejects_oversized_content_length() {
    // Content-Length claims 200MB — must reject before allocating
    let data = b"Content-Length: 209715200\r\n\r\n";
    let mut reader = BufReader::new(&data[..]);
    let result = read_dap_body(&mut reader).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p al-dap-client rejects_oversized`
Expected: FAIL — currently allocates 200MB then hits `read_exact` EOF error (not the right error).

- [ ] **Step 3: Add Content-Length cap**

Add a constant and guard in `framing.rs`:

```rust
/// Maximum DAP message body size (20 MB). DAP messages are small JSON;
/// anything larger is malformed or malicious.
const MAX_DAP_BODY_SIZE: usize = 20 * 1024 * 1024;
```

In `read_dap_body`, after line 16:

```rust
pub async fn read_dap_body<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> Result<Vec<u8>, std::io::Error> {
    let content_length = read_headers(reader).await?;
    if content_length > MAX_DAP_BODY_SIZE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Content-Length {content_length} exceeds maximum {MAX_DAP_BODY_SIZE}"),
        ));
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).await?;
    Ok(body)
}
```

Also add the same bound to `read_headers` for the header-line reads — add after line 28:

```rust
let mut line = String::new();
let n = reader.read_line(&mut line).await?;
if n > 8192 {
    return Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "DAP header line exceeds 8KB",
    ));
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p al-dap-client`
Expected: All pass including the new test.

- [ ] **Step 5: Commit**

```bash
git add crates/al-dap-client/src/framing.rs
git commit -m "security: cap DAP Content-Length to 20MB to prevent OOM allocation"
```

---

## Task 3: Secure OAuth Cache Directory

`save_cached_token` in `oauth.rs:643-644` creates the OAuth cache directory with `create_dir_all` using the process umask (typically `0o755`). This makes `~/.cache/al-lsp/oauth/` world-readable, leaking tenant names to other local users. The token files themselves are already `0o600`.

**Files:**
- Modify: `crates/al-symbols/src/oauth.rs:643-644`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(unix)]
#[test]
fn oauth_directory_has_restricted_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let cache_dir = dir.path().join("oauth");
    // Simulate what save_cached_token does for directory creation
    create_secure_dir(&cache_dir).unwrap();
    let perms = std::fs::metadata(&cache_dir).unwrap().permissions();
    assert_eq!(perms.mode() & 0o777, 0o700, "OAuth cache dir must be owner-only");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p al-symbols oauth_directory`
Expected: FAIL — `create_secure_dir` doesn't exist.

- [ ] **Step 3: Implement secure directory creation**

Add a helper and use it in `save_cached_token`:

```rust
/// Create a directory with owner-only permissions (0o700 on Unix).
#[cfg(unix)]
fn create_secure_dir(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}

#[cfg(not(unix))]
fn create_secure_dir(path: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)
}
```

Then replace lines 643-644:

```rust
// Before:
//   if let Some(parent) = path.parent() {
//       let _ = std::fs::create_dir_all(parent);
//   }

// After:
if let Some(parent) = path.parent() {
    let _ = create_secure_dir(parent);
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p al-symbols oauth`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/al-symbols/src/oauth.rs
git commit -m "security: set OAuth cache directory to 0o700 to prevent tenant name leakage"
```

---

## Task 4: Fix LSP JSON Construction (Correctness Bug)

`lsp_dispatch.rs` manually constructs JSON for LSP types instead of using `serde_json::to_value()`. This causes a **correctness bug**: `format!("{:?}", e.kind)` serializes `CompletionKind::Function` as the string `"Function"` instead of the LSP integer `3`. Same bug for `CodeActionKind`.

The al-core query types (`Position`, `Range`, `Location`) already derive `Serialize`. `CompletionEntry`, `CompletionKind`, `CodeActionEntry`, `CodeActionKind`, `TextEdit`, and `WorkspaceEdit` do not — they need it added with correct LSP integer mapping.

**Files:**
- Modify: `crates/al-core/src/queries/completions.rs:10-38` — add `Serialize` derive + LSP integer repr for `CompletionKind`
- Modify: `crates/al-core/src/queries/code_actions.rs:13-27` — add `Serialize` derive + LSP string repr for `CodeActionKind`
- Modify: `crates/al-core/src/queries/mod.rs:194-204` — add `Serialize` to `TextEdit` and `WorkspaceEdit`
- Modify: `crates/al-lsp/src/daemon/lsp_dispatch.rs:20-183` — replace manual JSON with `serde_json::to_value`

- [ ] **Step 1: Write the failing test for CompletionKind serialization**

In `crates/al-core/src/queries/completions.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_kind_serializes_to_lsp_integer() {
        // LSP spec: Function = 3, Field = 5, Variable = 6, Class = 7, Module = 9
        assert_eq!(serde_json::to_value(CompletionKind::Function).unwrap(), 3);
        assert_eq!(serde_json::to_value(CompletionKind::Field).unwrap(), 5);
        assert_eq!(serde_json::to_value(CompletionKind::Variable).unwrap(), 6);
        assert_eq!(serde_json::to_value(CompletionKind::Class).unwrap(), 7);
        assert_eq!(serde_json::to_value(CompletionKind::Module).unwrap(), 9);
        assert_eq!(serde_json::to_value(CompletionKind::Keyword).unwrap(), 14);
        assert_eq!(serde_json::to_value(CompletionKind::Snippet).unwrap(), 15);
        assert_eq!(serde_json::to_value(CompletionKind::Property).unwrap(), 10);
        assert_eq!(serde_json::to_value(CompletionKind::Method).unwrap(), 2);
        assert_eq!(serde_json::to_value(CompletionKind::Enum).unwrap(), 13);
        assert_eq!(serde_json::to_value(CompletionKind::EnumMember).unwrap(), 20);
        assert_eq!(serde_json::to_value(CompletionKind::Value).unwrap(), 12);
        assert_eq!(serde_json::to_value(CompletionKind::Text).unwrap(), 1);
        assert_eq!(serde_json::to_value(CompletionKind::Struct).unwrap(), 22);
        assert_eq!(serde_json::to_value(CompletionKind::Reference).unwrap(), 18);
    }

    #[test]
    fn completion_entry_serializes() {
        let entry = CompletionEntry {
            label: "MyProc".to_string(),
            kind: CompletionKind::Function,
            detail: Some("detail".to_string()),
            documentation: None,
            insert_text: None,
            sort_text: Some("0001".to_string()),
        };
        let v = serde_json::to_value(&entry).unwrap();
        assert_eq!(v["label"], "MyProc");
        assert_eq!(v["kind"], 3);
        assert_eq!(v["detail"], "detail");
        assert_eq!(v["sortText"], "0001");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p al-core completion_kind_serializes`
Expected: FAIL — `CompletionKind` doesn't implement `Serialize`.

- [ ] **Step 3: Add Serialize to CompletionKind with LSP integer mapping**

In `crates/al-core/src/queries/completions.rs`, implement custom serialization:

```rust
/// Completion item kinds (transport-agnostic).
///
/// Serialized as LSP CompletionItemKind integers per the specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionKind {
    Text,        // 1
    Method,      // 2
    Function,    // 3
    Field,       // 5
    Variable,    // 6
    Class,       // 7
    Module,      // 9
    Property,    // 10
    Value,       // 12
    Enum,        // 13
    Keyword,     // 14
    Snippet,     // 15
    Reference,   // 18
    EnumMember,  // 20
    Struct,      // 22
}

impl serde::Serialize for CompletionKind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let n: u32 = match self {
            Self::Text => 1,
            Self::Method => 2,
            Self::Function => 3,
            Self::Field => 5,
            Self::Variable => 6,
            Self::Class => 7,
            Self::Module => 9,
            Self::Property => 10,
            Self::Value => 12,
            Self::Enum => 13,
            Self::Keyword => 14,
            Self::Snippet => 15,
            Self::Reference => 18,
            Self::EnumMember => 20,
            Self::Struct => 22,
        };
        serializer.serialize_u32(n)
    }
}
```

Add `#[derive(serde::Serialize)]` to `CompletionEntry` with `#[serde(rename = "sortText")]` on `sort_text` and `#[serde(rename = "insertText")]` on `insert_text`:

```rust
#[derive(Debug, Clone, serde::Serialize)]
pub struct CompletionEntry {
    pub label: String,
    pub kind: CompletionKind,
    pub detail: Option<String>,
    pub documentation: Option<String>,
    #[serde(rename = "insertText")]
    pub insert_text: Option<String>,
    #[serde(rename = "sortText")]
    pub sort_text: Option<String>,
}
```

- [ ] **Step 4: Run completion tests**

Run: `cargo test -p al-core completion_kind_serializes`
Expected: PASS

- [ ] **Step 5: Add Serialize to CodeActionKind**

In `crates/al-core/src/queries/code_actions.rs`:

```rust
impl serde::Serialize for CodeActionKind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let s = match self {
            Self::QuickFix => "quickfix",
            Self::Refactor => "refactor",
            Self::Source => "source",
        };
        serializer.serialize_str(s)
    }
}
```

Add `#[derive(serde::Serialize)]` to `CodeActionEntry` and `DiagnosticInfo`. Add `#[serde(rename = "isPreferred")]` on `is_preferred`.

- [ ] **Step 6: Add Serialize to TextEdit and WorkspaceEdit**

In `crates/al-core/src/queries/mod.rs`:

```rust
#[derive(Debug, Clone, serde::Serialize)]
pub struct TextEdit {
    pub range: Range,
    #[serde(rename = "newText")]
    pub new_text: String,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct WorkspaceEdit {
    pub changes: Vec<(Url, Vec<TextEdit>)>,
}
```

- [ ] **Step 7: Replace manual JSON in lsp_dispatch.rs**

Replace each manual JSON construction with `serde_json::to_value`. Key transformations:

**Hover** (lines 24-30): `r` already has `Range` which derives `Serialize`:
```rust
let value = result.and_then(|r| serde_json::to_value(&r).ok());
```

**Definition** (lines 38-46):
```rust
let value = result.map(|locations| serde_json::to_value(&locations).unwrap_or_default());
```

**References** (lines 55-61):
```rust
let value = serde_json::to_value(&locations).unwrap_or_default();
```

**Implementations** (lines 69-75): Same as references.

**Completions** (lines 83-88):
```rust
let value = serde_json::to_value(&entries).unwrap_or_default();
```

**Code actions** (lines 178-181):
```rust
let value = serde_json::to_value(&actions).unwrap_or_default();
```

**Rename** (lines 116-127): This one is trickier because `WorkspaceEdit` serializes `changes` as a `Vec<(Url, Vec<TextEdit>)>` but LSP wants a `Map<string, TextEdit[]>`. Add a custom `Serialize` for `WorkspaceEdit` that produces the map format, or serialize manually here. Custom serialize is preferred:

```rust
impl serde::Serialize for WorkspaceEdit {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(1))?;
        let changes: serde_json::Map<String, serde_json::Value> = self.changes.iter().map(|(uri, edits)| {
            (uri.as_str().to_string(), serde_json::to_value(edits).unwrap_or_default())
        }).collect();
        map.serialize_entry("changes", &changes)?;
        map.end()
    }
}
```

**Semantic tokens** (lines 148-154) and **signature help** (lines 96-107): These use custom structs. Check if they derive Serialize; if not, add it. If the struct fields don't match LSP JSON keys, add `#[serde(rename)]`.

- [ ] **Step 8: Run all tests**

Run: `cargo test --workspace --exclude zed-al`
Expected: All pass. The daemon JSON output format has changed (integer kinds instead of string kinds) — verify e2e tests pass since the LSP server.rs path uses tower-lsp types directly and is unaffected.

- [ ] **Step 9: Commit**

```bash
git add crates/al-core/src/queries/completions.rs crates/al-core/src/queries/code_actions.rs crates/al-core/src/queries/mod.rs crates/al-lsp/src/daemon/lsp_dispatch.rs
git commit -m "fix: serialize CompletionKind/CodeActionKind as LSP integers instead of Debug strings"
```

---

## Task 5: Convert Recursive AST Walkers to Iterative

10 recursive tree-sitter walkers use native Rust recursion. While AL files are typically shallow (~15 levels), deeply nested code could theoretically overflow the stack. The idiomatic tree-sitter approach is `TreeCursor` with `goto_first_child()`/`goto_next_sibling()`/`goto_parent()` — zero-allocation, no recursion.

**All recursive walkers to convert:**

| Function | File | Line |
|----------|------|------|
| `collect_errors_recursive` | `al-syntax/src/parser.rs` | 99 |
| `extract_structural_ranges` | `al-syntax/src/folding.rs` | 34 |
| `walk_and_lint` | `al-syntax/src/lint.rs` | 122 |
| `find_hardcoded_strings` | `al-syntax/src/lint.rs` | 754 |
| `find_refs_recursive` | `al-syntax/src/navigation.rs` | 224 |
| `count_call_refs_recursive` | `al-syntax/src/navigation.rs` | 270 |
| `collect_var_symbols_recursive` | `al-syntax/src/symbols.rs` | 860 |
| `collect_test_procs_recursive` | `al-core/src/queries/tests.rs` | 104 |
| `collect_procs_recursive` | `al-core/src/queries/test_coverage.rs` | 190 |
| `collect_identifiers_recursive` | `al-core/src/queries/test_coverage.rs` | 336 |
| `collect_procs_recursive` | `al-core/src/queries/duplicates.rs` | 114 |

**Strategy:** Each walker follows the same pattern — check node kind, do work, recurse into children. Convert each to use `TreeCursor` with an explicit descent loop.

**Files:**
- Modify: `crates/al-syntax/src/parser.rs:92-123`
- Modify: `crates/al-syntax/src/folding.rs:34-90`
- Modify: `crates/al-syntax/src/lint.rs:122-245, 754-788`
- Modify: `crates/al-syntax/src/navigation.rs:224-268, 270-300`
- Modify: `crates/al-syntax/src/symbols.rs:860+`
- Modify: `crates/al-core/src/queries/tests.rs:104+`
- Modify: `crates/al-core/src/queries/test_coverage.rs:190+, 336+`
- Modify: `crates/al-core/src/queries/duplicates.rs:114+`

- [ ] **Step 1: Start with `collect_errors_recursive` — write test**

This function already uses `TreeCursor` but with recursion layered on top. Existing tests in `parser.rs` cover its behavior. Add a deep-nesting stress test:

```rust
#[test]
fn test_deeply_nested_does_not_stackoverflow() {
    let mut parser = AlParser::new();
    // Generate 500-deep nested if/then/begin/end
    let mut code = String::from("codeunit 1 Test { trigger OnRun() { ");
    for _ in 0..500 {
        code.push_str("if true then begin ");
    }
    for _ in 0..500 {
        code.push_str("end; ");
    }
    code.push_str("} }");
    // Must not panic with stack overflow
    let result = parser.parse(&code);
    // Errors are expected (malformed), but it should complete
    let _ = result;
}
```

- [ ] **Step 2: Run test to verify current behavior**

Run: `cargo test -p al-syntax test_deeply_nested`
Expected: May pass or may stack overflow depending on default stack size. Either way, establishes baseline.

- [ ] **Step 3: Convert `collect_errors_recursive` to iterative**

Replace the recursive function with `TreeCursor`-based iteration:

```rust
fn collect_errors(tree: &Tree, _text: &str) -> Vec<SyntaxError> {
    let mut errors = Vec::new();
    let mut cursor = tree.walk();
    let mut did_visit = false;
    loop {
        if !did_visit {
            let node = cursor.node();
            if node.is_error() || node.is_missing() {
                errors.push(SyntaxError {
                    message: if node.is_missing() {
                        format!("Missing {}", node.kind())
                    } else {
                        "Syntax error".to_string()
                    },
                    range: node.range(),
                });
            }
        }
        // Depth-first: try child, then sibling, then parent's sibling
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
    errors
}
```

Remove the old `collect_errors_recursive` function.

- [ ] **Step 4: Run all al-syntax tests**

Run: `cargo test -p al-syntax`
Expected: All pass.

- [ ] **Step 5: Convert `extract_structural_ranges` (folding.rs)**

Same pattern: replace recursion with cursor loop. The `match node.kind()` block runs on each first visit. The cursor-based DFS replaces the child iteration at lines 86-89.

- [ ] **Step 6: Convert `walk_and_lint` (lint.rs)**

This is the most complex walker due to `if_depth` tracking. Use an explicit `Vec<usize>` stack for depth:

```rust
fn walk_and_lint(root: Node, source: &[u8], text: &str, config: &LintConfig, diagnostics: &mut Vec<LintDiagnostic>) {
    let mut cursor = root.walk();
    let mut if_depth_stack: Vec<usize> = vec![0];
    let mut did_visit = false;
    loop {
        if !did_visit {
            let node = cursor.node();
            let if_depth = *if_depth_stack.last().unwrap_or(&0);
            // ... existing match block (same as current lines 132-237) ...
            // For if_statement: push if_depth+1 before descending
        }
        if !did_visit && cursor.goto_first_child() {
            // Push depth context for if nodes
            let parent_kind = cursor.node().parent().map(|n| n.kind());
            if matches!(parent_kind, Some("if_statement" | "empty_if_statement")) {
                if_depth_stack.push(*if_depth_stack.last().unwrap_or(&0) + 1);
            } else {
                if_depth_stack.push(*if_depth_stack.last().unwrap_or(&0));
            }
            continue;
        }
        if cursor.goto_next_sibling() {
            did_visit = false;
            continue;
        }
        if cursor.goto_parent() {
            if_depth_stack.pop();
            did_visit = true;
            continue;
        }
        break;
    }
}
```

Note: `walk_and_lint` has a special `return` at line 183 for `if_statement` to recurse with incremented depth. In the iterative version, this is handled by the depth stack naturally.

Also convert `find_hardcoded_strings` (line 754) to iterative. This function recurses into children looking for `"string"` or `"verbatim_string"` nodes. Keep it as a separate function (NOT inlined into `walk_and_lint`) but replace its internal recursion with a `TreeCursor` loop:

```rust
fn find_hardcoded_strings(root: Node, source: &[u8], diagnostics: &mut Vec<LintDiagnostic>) {
    let mut cursor = root.walk();
    let mut did_visit = false;
    loop {
        if !did_visit {
            let node = cursor.node();
            if node.kind() == "string" || node.kind() == "verbatim_string" {
                // ... existing string check logic from lines 756-781 ...
                // (check inner text, skip empty/format strings, check parent context)
            }
        }
        if !did_visit && cursor.goto_first_child() { continue; }
        if cursor.goto_next_sibling() { did_visit = false; continue; }
        if cursor.goto_parent() {
            if cursor.node() == root { break; }
            did_visit = true;
            continue;
        }
        break;
    }
}
```

`walk_and_lint` continues to call `find_hardcoded_strings(node, source, diagnostics)` at line 751 — only its internal traversal changes.

- [ ] **Step 7: Convert remaining recursive walkers in navigation.rs, symbols.rs, tests.rs, test_coverage.rs, duplicates.rs**

Each follows the same cursor-based DFS pattern. Apply the same transformation to:
- `find_refs_recursive` → iterative
- `count_call_refs_recursive` → iterative
- `collect_var_symbols_recursive` → iterative
- `collect_test_procs_recursive` → iterative
- `collect_procs_recursive` (test_coverage.rs) → iterative
- `collect_identifiers_recursive` (test_coverage.rs) → iterative
- `collect_procs_recursive` (duplicates.rs) → iterative

- [ ] **Step 8: Run full test suite**

Run: `cargo test --workspace --exclude zed-al`
Expected: All pass.

- [ ] **Step 9: Run clippy**

Run: `cargo clippy --workspace --exclude zed-al`
Expected: No new warnings.

- [ ] **Step 10: Commit**

```bash
git add crates/al-syntax/src/parser.rs crates/al-syntax/src/folding.rs crates/al-syntax/src/lint.rs crates/al-syntax/src/navigation.rs crates/al-syntax/src/symbols.rs crates/al-core/src/queries/tests.rs crates/al-core/src/queries/test_coverage.rs crates/al-core/src/queries/duplicates.rs
git commit -m "robustness: convert all recursive AST walkers to iterative TreeCursor traversal"
```

---

## Task 6: Move SemanticBridge Init Off Async Executor

`SemanticBridge::new()` (synchronous .NET CLR initialization) is called while holding a `tokio::sync::RwLock` write guard in both `get_or_init_bridge` (line 226) and `restart_bridge` (line 277). This blocks the tokio executor thread during CLR init, starving other async tasks.

**Files:**
- Modify: `crates/al-core/src/semantic.rs:217-240, 246-280`

- [ ] **Step 1: Write test verifying non-blocking init**

This is difficult to unit test directly. Instead, verify the refactor compiles and existing semantic tests pass. Add a doc comment explaining the design.

- [ ] **Step 2: Refactor `get_or_init_bridge` to use `spawn_blocking`**

```rust
pub async fn get_or_init_bridge(workspace: &Workspace) -> Option<...> {
    // ... fast path unchanged (read lock check) ...

    // Slow path: initialize on a blocking thread
    let toolchain = workspace.toolchain.read().await.clone()?;
    let mut write_guard = workspace.semantic.write().await;

    // Double-check after acquiring write lock
    if write_guard.is_some() {
        return Some(write_guard.downgrade());
    }

    let ca_path = toolchain.code_analysis.clone();
    let version = toolchain.version.clone();

    // Release write lock before blocking init
    drop(write_guard);

    let bridge_result = tokio::task::spawn_blocking(move || {
        SemanticBridge::new(&ca_path, &version)
    }).await;

    // Re-acquire write lock and insert
    let mut write_guard = workspace.semantic.write().await;

    // Double-check again (another task may have init'd while we were blocking)
    if write_guard.is_some() {
        return Some(write_guard.downgrade());
    }

    match bridge_result {
        Ok(Ok(bridge)) => {
            tracing::info!("Semantic bridge initialized");
            *write_guard = Some(bridge);
            Some(write_guard.downgrade())
        }
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "Failed to initialize semantic bridge");
            if let Some(sink) = workspace.notify_sink.get() {
                sink(&format!("AL semantic bridge failed to initialize: {e}"));
            }
            None
        }
        Err(e) => {
            tracing::warn!(error = %e, "Semantic bridge init task panicked");
            None
        }
    }
}
```

**Note:** `DotNetHost` has `unsafe impl Send` in `al-semantic/src/host.rs:37`, so `SemanticBridge` is `Send` and `spawn_blocking` will compile.

- [ ] **Step 3: Apply same pattern to `restart_bridge`**

Same transformation: drop write lock → `spawn_blocking` for `SemanticBridge::new` → re-acquire write lock → insert.

- [ ] **Step 4: Run tests**

Run: `cargo test --workspace --exclude zed-al`
Expected: All pass.

- [ ] **Step 5: Commit**

```bash
git add crates/al-core/src/semantic.rs
git commit -m "robustness: move SemanticBridge::new to spawn_blocking to avoid blocking async executor"
```

---

## Task 7: Add Initialization Readiness Gate

When files are opened immediately after the LSP server starts, handlers return empty results because the workspace isn't initialized yet. Add a readiness signal so handlers can await initialization.

**Files:**
- Modify: `crates/al-lsp/src/server.rs` — add `init_notify: Arc<tokio::sync::Notify>` field, signal on init completion, await in key handlers

- [ ] **Step 1: Add `Notify` field to `AlServer`**

```rust
// In AlServer struct:
init_notify: Arc<tokio::sync::Notify>,
```

Initialize in constructor. Signal in `initialized()` after `initialize_workspace` completes (modify the spawned task).

- [ ] **Step 2: Add `await_ready` helper**

```rust
async fn await_ready(&self) {
    if self.init_done.load(Ordering::Acquire) {
        return; // Already initialized
    }
    // Wait with timeout — don't block indefinitely
    tokio::time::timeout(
        std::time::Duration::from_secs(30),
        self.init_notify.notified(),
    ).await.ok();
}
```

- [ ] **Step 3: Call `await_ready` in key handlers**

Add `self.await_ready().await;` at the top of these workspace-dependent handlers in `server.rs`:
- `hover` (line 434)
- `completion` (line 447)
- `goto_definition` (line 464)
- `references` (line 479)
- `document_symbol` (line 493)
- `folding_range` (line 530)
- `semantic_tokens_full` (line 542)
- `signature_help` (line 560)
- `code_action` (line 572)
- `rename` (line 586)
- `symbol` (line 612)
- `inlay_hint` (line 626)

Do NOT add it to `did_open`, `did_change`, `did_close`, `did_save`, `did_change_configuration`, or `formatting` — these must work before workspace loading completes.

- [ ] **Step 4: Signal completion in the init task**

In the `initialized` method, after `initialize_workspace` completes:

```rust
let notify = self.init_notify.clone();
let handle = tokio::spawn(async move {
    workspace::initialize_workspace(ws, client, root_uri).await;
    notify.notify_waiters();
});
```

- [ ] **Step 5: Run e2e tests**

Run: `cargo test -p al-test-harness`
Expected: All pass. The test harness already polls for readiness, so the new gate is transparent to it.

- [ ] **Step 6: Commit**

```bash
git add crates/al-lsp/src/server.rs
git commit -m "robustness: add initialization readiness gate so handlers await workspace loading"
```

---

## Task 8: Reduce CLI Command Boilerplate

~45 CLI command functions repeat the identical connect/request/format scaffold. Extract a generic helper.

**Files:**
- Modify: `crates/al-cli/src/commands/lsp.rs`
- Modify: `crates/al-cli/src/commands/build.rs`
- Modify: `crates/al-cli/src/commands/debug.rs`
- Modify: `crates/al-cli/src/commands/insight.rs`
- Create or modify: `crates/al-cli/src/commands/mod.rs` — add helper function

- [ ] **Step 1: Add the generic helper**

In `crates/al-cli/src/commands/mod.rs` (or a shared location):

```rust
use std::process::ExitCode;
use crate::daemon_client::DaemonClient;

/// Execute a daemon JSON-RPC request and handle the common connect/request/format lifecycle.
///
/// - `method`: the JSON-RPC method name
/// - `params`: optional request parameters
/// - `json`: if true, print raw JSON; if false, call `format_fn` for human output
/// - `project_dir`: optional project directory override for daemon connection
/// - `format_fn`: called with the result value for human-readable output
pub fn run_command<F>(
    method: &str,
    params: Option<serde_json::Value>,
    json: bool,
    project_dir: Option<&str>,
    format_fn: F,
) -> ExitCode
where
    F: FnOnce(&serde_json::Value),
{
    let mut client = match crate::connect(project_dir) {
        Ok(c) => c,
        Err(e) => return crate::report_error(&e, json),
    };
    match client.request(method, params) {
        Ok(result) => {
            if json {
                crate::print_json(&result);
            } else {
                format_fn(&result);
            }
            ExitCode::SUCCESS
        }
        Err(e) => crate::report_error(&e, json),
    }
}
```

- [ ] **Step 2: Migrate 3 simple commands in lsp.rs as proof of concept**

Pick the simplest commands (e.g., `cmd_version`, `cmd_search`, `cmd_packages`) and convert them to use `run_command`. For example:

```rust
// Before (cmd_search, ~30 lines):
pub fn cmd_search(query: &str, limit: usize, json: bool) -> ExitCode {
    let mut client = match connect(None) { Ok(c) => c, Err(e) => return report_error(&e, json) };
    let params = serde_json::json!({ "query": query, "limit": limit });
    match client.request("search", Some(params)) {
        Ok(result) => { if json { print_json(&result); } else { /* 15 lines of formatting */ } ExitCode::SUCCESS }
        Err(e) => report_error(&e, json),
    }
}

// After (~5 lines):
pub fn cmd_search(query: &str, limit: usize, json: bool) -> ExitCode {
    let params = serde_json::json!({ "query": query, "limit": limit });
    run_command("search", Some(params), json, None, |result| {
        // 15 lines of human formatting
    })
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p al-cli`
Expected: PASS (if CLI tests exist) or `cargo build -p al-cli` succeeds.

- [ ] **Step 4: Migrate remaining commands**

Convert all remaining commands across `lsp.rs`, `build.rs`, `debug.rs`, `insight.rs`. Some commands use `project_dir` — pass it through. Some commands have early returns or multi-step logic — those may not fit the helper and can stay manual.

- [ ] **Step 5: Run full build and tests**

Run: `cargo build --workspace --exclude zed-al && cargo test --workspace --exclude zed-al`
Expected: All pass.

- [ ] **Step 6: Commit**

```bash
git add crates/al-cli/src/commands/
git commit -m "simplify: extract CLI command boilerplate into run_command helper"
```

---

## Task 9: Remove Dead Fallback Parser Code

`parser.rs:44-51` creates a fallback `Parser` when `self.parser.parse` returns `None`. Since no timeout or cancellation is set, `parse` never returns `None`. The code is confirmed dead.

**Files:**
- Modify: `crates/al-syntax/src/parser.rs:39-64`

- [ ] **Step 1: Replace fallback with expect**

```rust
pub fn parse(&mut self, text: &str) -> ParseResult {
    let tree = self.parser.parse(text, None)
        .expect("tree-sitter parse must succeed without timeout or cancellation");
    let errors = collect_errors(&tree, text);
    ParseResult { tree, errors }
}

pub fn parse_incremental(&mut self, text: &str, old_tree: &Tree) -> ParseResult {
    let tree = self.parser.parse(text, Some(old_tree))
        .expect("tree-sitter incremental parse must succeed without timeout or cancellation");
    let errors = collect_errors(&tree, text);
    ParseResult { tree, errors }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p al-syntax`
Expected: All pass.

- [ ] **Step 3: Commit**

```bash
git add crates/al-syntax/src/parser.rs
git commit -m "simplify: remove dead fallback parser code (no timeout/cancellation is set)"
```

---

## Task 10: Rewrite ensure_seq with serde_json (Correctness Bug + Live Bug)

Two correctness issues found:

1. **`al-dap-client/src/framing.rs:69`**: The `windows(6)` check for `"seq":` false-positives when `"seq":` appears inside a JSON string value (e.g., a watch expression like `Message('seq:%1', seq)`). The byte-level "optimization" saves nothing — the output is immediately parsed by `serde_json::from_slice` in `client.rs:64`.

2. **`al-lsp/src/dap/mod.rs:440`**: A private duplicate of `ensure_seq` using the **older `windows(5)` pattern** that was already fixed in `framing.rs`. This version false-positives on `"seq"` appearing as any JSON value. Additionally, it unconditionally increments the counter before checking for `{`, and always appends a trailing comma (breaks on empty objects). `patch_incoming` (line 414) already does a full serde_json parse/reserialize immediately after calling this broken `ensure_seq`, making the byte-level approach completely redundant.

**Files:**
- Modify: `crates/al-dap-client/src/framing.rs:69-94`
- Modify: `crates/al-lsp/src/dap/mod.rs:414-455`

- [ ] **Step 1: Add failing test for the false-positive bug**

In `crates/al-dap-client/src/framing.rs` tests:

```rust
#[test]
fn ensure_seq_not_fooled_by_seq_colon_in_string_value() {
    // A string value containing "seq": (with colon) must NOT suppress injection.
    // This happens when debugging AL expressions containing "seq" as a variable name.
    let body = br#"{"type":"response","body":{"expression":"\"seq\":42"}}"#;
    let counter = AtomicI64::new(7);
    let patched = ensure_seq(body, &counter);
    let value: serde_json::Value = serde_json::from_slice(&patched).unwrap();
    assert_eq!(value["seq"], 7, "seq must be injected despite \"seq\": appearing in a string value");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p al-dap-client ensure_seq_not_fooled_by_seq_colon`
Expected: FAIL — the `windows(6)` check matches `"seq":` inside the string value and returns without injecting.

- [ ] **Step 3: Rewrite `ensure_seq` in framing.rs with serde_json**

```rust
/// Patch a DAP message body to include a `seq` field if missing.
///
/// EditorServices.Host sometimes omits the required `seq` field from
/// its responses and events. This function injects one using proper
/// JSON parsing to avoid false positives from string values containing "seq".
pub fn ensure_seq(body: &[u8], counter: &AtomicI64) -> Vec<u8> {
    let mut value: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return body.to_vec(),
    };
    if let Some(obj) = value.as_object_mut() {
        if !obj.contains_key("seq") {
            obj.insert(
                "seq".to_string(),
                serde_json::Value::Number(counter.fetch_add(1, Ordering::Relaxed).into()),
            );
        }
    }
    serde_json::to_vec(&value).unwrap_or_else(|_| body.to_vec())
}
```

- [ ] **Step 4: Run all framing.rs tests**

Run: `cargo test -p al-dap-client`
Expected: All pass, including the new test and all existing `ensure_seq_*` tests.

- [ ] **Step 5: Fix the `al-lsp/src/dap/mod.rs` duplicate**

Delete the private `ensure_seq` function at lines 440-455. Fold the seq injection directly into `patch_incoming` since it already parses the JSON:

```rust
fn patch_incoming(body: &[u8], counter: &AtomicI64) -> Vec<u8> {
    let mut msg: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return body.to_vec(),
    };

    let obj = match msg.as_object_mut() {
        Some(o) => o,
        None => return body.to_vec(),
    };

    // Inject seq if missing (EditorServices.Host often omits it)
    if !obj.contains_key("seq") {
        obj.insert(
            "seq".to_string(),
            serde_json::Value::Number(counter.fetch_add(1, Ordering::Relaxed).into()),
        );
    }

    // Patch null string fields that Zed requires to be non-null
    for field in &["command", "event", "message", "type"] {
        if let Some(val) = obj.get(*field) {
            if val.is_null() {
                obj.insert(field.to_string(), serde_json::Value::String(String::new()));
                debug!("Patched null {field} → empty string in DAP message");
            }
        }
    }

    serde_json::to_vec(&msg).unwrap_or_else(|_| body.to_vec())
}
```

This eliminates the duplicate, fixes the `windows(5)` bug, and removes a redundant parse-serialize cycle.

- [ ] **Step 6: Run all tests**

Run: `cargo test --workspace --exclude zed-al`
Expected: All pass.

- [ ] **Step 7: Commit**

```bash
git add crates/al-dap-client/src/framing.rs crates/al-lsp/src/dap/mod.rs
git commit -m "fix: rewrite ensure_seq with serde_json to fix false-positive on seq in string values

The byte-level windows(6) check for \"seq\": false-positives when the
pattern appears inside a JSON string value (e.g. watch expressions).
The al-lsp duplicate used the older windows(5) pattern — an active bug.

Folded seq injection into patch_incoming to eliminate a redundant
parse-serialize cycle."
```
