# Zed Integration & Settings Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deep Zed integration — settings schema with autocomplete, recommended settings auto-apply, pull diagnostics, gutter runnables for tests/events, and CodeAnalysis bridge diagnostics — making the AL extension feel native to Zed and matching or exceeding Cursor's feature set.

**Architecture:** Three layers: (1) WASM extension (`src/`) handles Zed integration — settings schemas, recommended config popup, binary management; (2) al-lsp server (`crates/al-lsp/`) handles LSP protocol — pull diagnostics, CodeAnalysis bridge wiring; (3) Static config (`languages/al/`) handles tree-sitter integration — tasks.json with runnable tags, runnables.scm. Each task is independently deployable.

**Tech Stack:** Rust, zed_extension_api (v0.8.0 via git), tower-lsp, al-semantic (.NET bridge), tree-sitter

---

## File Structure

| File | Responsibility | Tasks |
|------|---------------|-------|
| `Cargo.toml` (root) | Extension API dependency | T1 |
| `extension.toml` | Extension manifest, lib version | T1 |
| `src/lib.rs` | WASM extension entry — settings schemas, recommended config popup | T1, T2 |
| `src/settings.rs` | Settings application logic + recommended defaults | T2 |
| `schemas/settings.json` | JSON Schema for LSP settings (already exists) | T1 |
| `languages/al/tasks.json` | Task templates with runnable tags | T3 |
| `languages/al/runnables.scm` | Tree-sitter runnable captures (already correct) | T3 (verify only) |
| `crates/al-lsp/src/server.rs` | LSP capabilities, pull diagnostics handler | T4, T5 |
| `crates/al-lsp/src/diagnostics.rs` | Diagnostic conversion, bridge diagnostic integration | T5 |
| `crates/al-core/src/config.rs` | AlConfig struct with defaults | T2 |

---

## Task 1: Upgrade Extension API to v0.8.0 + Settings Schema

**Files:**
- Modify: `Cargo.toml` — change `zed_extension_api` dependency
- Modify: `extension.toml` — update `lib.version`
- Modify: `src/lib.rs` — implement schema methods
- Reference: `schemas/settings.json` — existing schema content

- [ ] **Step 1: Update Cargo.toml to use zed_extension_api from git**

In `Cargo.toml`, change:
```toml
# FROM:
zed_extension_api = "0.7.0"
# TO:
zed_extension_api = { git = "https://github.com/zed-industries/zed", branch = "main" }
```

- [ ] **Step 2: Update extension.toml lib version**

```toml
[lib]
kind = "Rust"
version = "0.8.0"
```

- [ ] **Step 3: Build WASM to verify the API compiles**

```bash
cargo check -p zed-al --target wasm32-wasip1 2>&1 | tail -10
```

If v0.8.0 WIT interface isn't available yet on main, the build will fail. In that case, stay on v0.7.0 and skip the schema methods (document as a gap for when v0.8.0 publishes). Proceed to Step 6.

- [ ] **Step 4: Implement `language_server_workspace_configuration_schema`**

In `src/lib.rs`, add to the `Extension` impl:

```rust
fn language_server_workspace_configuration_schema(
    &mut self,
    _language_server_id: &zed::LanguageServerId,
    _worktree: &zed::Worktree,
) -> Option<serde_json::Value> {
    let schema = include_str!("../schemas/settings.json");
    serde_json::from_str(schema).ok()
}
```

- [ ] **Step 5: Implement `language_server_initialization_options_schema`**

```rust
fn language_server_initialization_options_schema(
    &mut self,
    _language_server_id: &zed::LanguageServerId,
    _worktree: &zed::Worktree,
) -> Option<serde_json::Value> {
    Some(serde_json::json!({
        "type": "object",
        "properties": {
            "workspacePath": {
                "type": "string",
                "description": "Path to the AL project root (auto-detected from workspace)"
            },
            "setActiveWorkspace": {
                "type": "boolean",
                "default": true,
                "description": "Set this workspace as active in the AL language server"
            }
        }
    }))
}
```

- [ ] **Step 6: Build and verify**

```bash
cargo build -p zed-al --target wasm32-wasip1 --release 2>&1 | tail -5
```

- [ ] **Step 7: Commit**

---

## Task 2: Recommended Settings Auto-Apply Popup

**Files:**
- Modify: `src/lib.rs` — add popup trigger after workspace init
- Modify: `src/settings.rs` — add recommended defaults constant and apply logic
- Modify: `crates/al-core/src/config.rs` — ensure defaults match recommendations

When the extension loads for the first time on an AL project, show a popup: "Apply recommended AL settings?" If yes, write optimized settings to Zed's global `settings.json`.

- [ ] **Step 1: Define the recommended settings payload in `src/settings.rs`**

```rust
/// Recommended Zed settings for optimal AL development.
/// These leverage Zed-native features and our LSP capabilities.
pub const RECOMMENDED_SETTINGS: &str = r#"{
  "document_folding_ranges": "on",
  "colorize_brackets": true,
  "completions": {
    "lsp": true,
    "lsp_fetch_timeout_ms": 1000,
    "lsp_insert_mode": "replace",
    "words": "enabled",
    "words_min_length": 3
  },
  "document_symbols": "on",
  "go_to_definition_fallback": "none",
  "lsp": {
    "al-lsp": {
      "settings": {
        "al.enableCodeAnalysis": true,
        "al.backgroundCodeAnalysis": true,
        "al.diagnosticsScope": "project",
        "al.diagnosticsTrigger": "continuous",
        "al.codeAnalyzers": ["CodeCop", "AppSourceCop", "UICop", "PerTenantCop"],
        "al.enableNativeLint": true,
        "al.enableCodeActions": true,
        "al.inlayHints.parameterNames": true
      },
      "enable_lsp_tasks": true
    }
  },
  "languages": {
    "AL": {
      "semantic_tokens": "combined",
      "completions": {
        "lsp": true,
        "lsp_fetch_timeout_ms": 1000,
        "lsp_insert_mode": "replace",
        "words": "enabled",
        "words_min_length": 3
      },
      "document_folding_ranges": "on",
      "colorize_brackets": true,
      "document_symbols": "on",
      "language_servers": ["al-lsp"],
      "debuggers": ["al"],
      "tasks": {
        "prefer_lsp": true,
        "enabled": true
      }
    }
  },
  "diagnostics": {
    "lsp_pull_diagnostics": {
      "enabled": true,
      "debounce_ms": 150
    },
    "button": true,
    "include_warnings": true,
    "inline": {
      "enabled": true,
      "update_debounce_ms": 150,
      "padding": 4,
      "min_column": 0,
      "max_severity": null
    }
  }
}"#;
```

- [ ] **Step 2: Add settings application logic**

The WASM extension API does NOT have direct file write access to `~/.config/zed/settings.json`. Instead, we use the LSP's `window/showMessageRequest` to prompt the user, and if they accept, we execute the `zed: open settings` command and provide instructions, OR we use `workspace/applyEdit` to write a config file.

Actually, the cleanest approach: the al-lsp server handles this. On first `initialize`, if no AL settings are detected in the workspace configuration, the server sends a `window/showMessageRequest` with "Apply recommended AL settings?" → Yes/No. If Yes, the server writes the settings via a command that the WASM extension handles.

In `crates/al-lsp/src/server.rs`, after workspace initialization:

```rust
// Check if AL settings are configured
let config = self.workspace.config.read().await;
let settings_configured = !config.code_analyzers.is_empty()
    && config.enable_code_analysis;
drop(config);

if !settings_configured {
    let response = self.client.show_message_request(
        MessageType::INFO,
        "Apply recommended AL development settings to Zed?".to_string(),
        Some(vec![
            MessageActionItem { title: "Yes".to_string(), ..Default::default() },
            MessageActionItem { title: "No".to_string(), ..Default::default() },
        ]),
    ).await;

    if let Ok(Some(action)) = response {
        if action.title == "Yes" {
            // Execute the settings apply command
            self.client.send_request::<lsp::request::ExecuteCommand>(
                ExecuteCommandParams {
                    command: "al.applyRecommendedSettings".to_string(),
                    arguments: vec![],
                    ..Default::default()
                },
            ).await.ok();
        }
    }
}
```

The actual settings writing is handled by the `al.applyRecommendedSettings` execute command, which reads and merges settings into the Zed settings file.

- [ ] **Step 3: Implement the settings apply command handler**

In the `execute_command` handler in `server.rs`:

```rust
"al.applyRecommendedSettings" => {
    // Read current settings, merge recommended, write back
    let settings_path = dirs::config_dir()
        .map(|d| d.join("zed/settings.json"))
        .unwrap_or_default();
    // ... read, merge, write logic
}
```

- [ ] **Step 4: Register the command in capabilities**

Add `"al.applyRecommendedSettings"` to `execute_command_provider.commands`.

- [ ] **Step 5: Test — verify popup appears on fresh project open**
- [ ] **Step 6: Commit**

---

## Task 3: Wire Runnables Tags to Task Templates (Gutter Run Buttons)

**Files:**
- Modify: `languages/al/tasks.json` — add entries matching runnables.scm tags
- Verify: `languages/al/runnables.scm` — already has correct captures

The `runnables.scm` defines tags `al-test`, `al-event-publisher`, `al-event-subscriber` but NO tasks.json entries match these tags. This is why gutter run buttons don't appear.

- [ ] **Step 1: Add matching task entries to tasks.json**

Add these entries to the existing `languages/al/tasks.json` array:

```json
{
  "label": "AL: Run Test '$ZED_SYMBOL'",
  "command": "al",
  "args": ["test", "--procedure", "$ZED_SYMBOL", "--json"],
  "tags": ["al-test"],
  "reveal": "always"
},
{
  "label": "AL: Debug Test '$ZED_SYMBOL'",
  "command": "al",
  "args": ["debug", "test", "--procedure", "$ZED_SYMBOL"],
  "tags": ["al-test"],
  "reveal": "always"
},
{
  "label": "AL: Trace Event '$ZED_SYMBOL'",
  "command": "al",
  "args": ["trace", "$ZED_SYMBOL", "--json"],
  "tags": ["al-event-publisher"],
  "reveal": "always"
},
{
  "label": "AL: Find Subscribers for '$ZED_SYMBOL'",
  "command": "al",
  "args": ["subscribers", "$ZED_SYMBOL", "--json"],
  "tags": ["al-event-publisher"],
  "reveal": "always"
},
{
  "label": "AL: Show Event Source for '$ZED_SYMBOL'",
  "command": "al",
  "args": ["events", "$ZED_SYMBOL", "--json"],
  "tags": ["al-event-subscriber"],
  "reveal": "always"
}
```

- [ ] **Step 2: Verify runnables.scm captures fire correctly**

Open a file with `[Test]` procedures in Zed. Verify:
- Gutter run icon appears next to `[Test]` procedures
- Gutter run icon appears next to `[IntegrationEvent]` procedures
- Clicking the icon shows the task options

- [ ] **Step 3: Test each task runs correctly from the gutter**

- Click run on a `[Test]` procedure → terminal shows `al test --procedure TestName`
- Click run on an `[IntegrationEvent]` procedure → terminal shows event trace results
- Click run on an `[EventSubscriber]` procedure → terminal shows event source info

- [ ] **Step 4: Commit**

---

## Task 4: Implement Pull Diagnostics (`textDocument/diagnostic`)

**Files:**
- Modify: `crates/al-lsp/src/server.rs` — add `diagnosticProvider` capability + handler
- Modify: `crates/al-lsp/src/diagnostics.rs` — shared diagnostic generation logic

The user has `lsp_pull_diagnostics: { enabled: true, debounce_ms: 150 }` in their Zed settings. We need to implement the server side.

- [ ] **Step 1: Add `diagnosticProvider` to server capabilities**

In `server.rs` `initialize()`, add to `ServerCapabilities`:

```rust
diagnostic_provider: Some(DiagnosticServerCapabilities::Options(DiagnosticOptions {
    identifier: Some("al-lsp".to_string()),
    inter_file_dependencies: true,
    workspace_diagnostics: true,
    work_done_progress_options: WorkDoneProgressOptions::default(),
})),
```

- [ ] **Step 2: Implement `textDocument/diagnostic` handler**

```rust
async fn diagnostic(
    &self,
    params: DocumentDiagnosticParams,
) -> Result<DocumentDiagnosticReportResult> {
    let uri = &params.text_document.uri;
    let diags = diagnostics::compute_diagnostics(&self.workspace, uri).await;
    Ok(DocumentDiagnosticReportResult::Report(
        DocumentDiagnosticReport::Full(RelatedFullDocumentDiagnosticReport {
            related_documents: None,
            full_document_diagnostic_report: FullDocumentDiagnosticReport {
                result_id: None,
                items: diags,
            },
        }),
    ))
}
```

- [ ] **Step 3: Extract shared diagnostic computation into `diagnostics.rs`**

Move the lint + parse error logic from `schedule_diagnostics()` into a shared `compute_diagnostics()` function that both push and pull paths use:

```rust
pub async fn compute_diagnostics(
    workspace: &Workspace,
    uri: &Url,
) -> Vec<Diagnostic> {
    let text = match workspace.documents.get_text_arc(uri) {
        Some(t) => t,
        None => {
            // Try file_index for non-open files
            if let Ok(path) = uri.to_file_path() {
                match workspace.file_index.files.get(&path) {
                    Some(entry) => entry.value().clone().into(),
                    None => return vec![],
                }
            } else {
                return vec![];
            }
        }
    };

    let source = text.as_bytes();
    let parse_result = al_core::syntax::AlParser::parse_quick(&text);
    let mut diags = Vec::new();

    for err in &parse_result.errors {
        diags.push(syntax_error_to_diagnostic(err, source));
    }

    let lint_result = al_core::syntax::lint(&parse_result.tree, &text);
    for lint in &lint_result {
        diags.push(lint_to_diagnostic(lint, source));
    }

    diags
}
```

- [ ] **Step 4: Keep push diagnostics for instant feedback**

`schedule_diagnostics()` stays as-is for instant feedback on typing. Pull diagnostics complement it — Zed uses pull when it needs fresh data (tab switch, save, etc.).

- [ ] **Step 5: Test — verify pull diagnostics work**

```bash
# Zed should now pull diagnostics on tab switch and show them for files not yet opened
```

- [ ] **Step 6: Commit**

---

## Task 5: Wire CodeAnalysis Bridge Diagnostics

**Files:**
- Modify: `crates/al-lsp/src/diagnostics.rs` — add bridge diagnostic call
- Modify: `crates/al-lsp/src/server.rs` — call bridge in publish_diagnostics flow

The `.NET CodeAnalysis` bridge (`SemanticBridge::analyze()`) can call Microsoft's analyzer DLLs (CodeCop, AppSourceCop, UICop, PerTenantCop, and third-party like LinterCop) directly — no compiler process needed. This gives us the same diagnostics Cursor shows (permission set warnings, event publisher restrictions, etc.).

- [ ] **Step 1: Add bridge diagnostics to `compute_diagnostics`**

After the lint diagnostics in `compute_diagnostics()`, add:

```rust
// CodeAnalysis bridge diagnostics (if enabled and bridge available)
let config = workspace.config.try_read().map(|c| c.clone());
if let Some(config) = config {
    if config.enable_code_analysis && config.background_code_analysis {
        if let Some(bridge_guard) = crate::semantic::try_get_bridge(workspace) {
            if let Some(bridge) = bridge_guard.as_ref() {
                if let Ok(path) = uri.to_file_path() {
                    let req = al_semantic::AnalyzeRequest {
                        file: path,
                        analyzers: config.code_analyzers.clone(),
                    };
                    match bridge.analyze(req).await {
                        Ok(entries) => {
                            for entry in entries {
                                diags.push(bridge_diagnostic_to_lsp(&entry, source));
                            }
                        }
                        Err(e) => {
                            tracing::debug!(error = %e, "bridge analyze failed");
                        }
                    }
                }
            }
        }
    }
}
```

- [ ] **Step 2: Implement `bridge_diagnostic_to_lsp` converter**

```rust
fn bridge_diagnostic_to_lsp(entry: &al_semantic::DiagnosticEntry, _source: &[u8]) -> Diagnostic {
    Diagnostic {
        range: Range {
            start: Position { line: entry.line.saturating_sub(1), character: entry.column },
            end: Position { line: entry.end_line.saturating_sub(1), character: entry.end_column },
        },
        severity: Some(match entry.severity.as_str() {
            "Error" => DiagnosticSeverity::ERROR,
            "Warning" => DiagnosticSeverity::WARNING,
            "Info" | "Information" => DiagnosticSeverity::INFORMATION,
            _ => DiagnosticSeverity::HINT,
        }),
        code: entry.code.as_ref().map(|c| NumberOrString::String(c.clone())),
        source: Some("al-compiler".to_string()),
        message: entry.message.clone(),
        ..Default::default()
    }
}
```

- [ ] **Step 3: Verify `AnalyzeRequest` struct matches bridge API**

```bash
grep -A 10 "pub struct AnalyzeRequest" crates/al-semantic/src/lib.rs
grep -A 10 "pub struct DiagnosticEntry" crates/al-semantic/src/lib.rs
```

Adjust the field names to match the actual structs.

- [ ] **Step 4: Test — verify CodeAnalysis diagnostics appear in Zed**

Open CodeunitWithEvents.al (which has `Rec: Record "Customer Posting Group"` in an event publisher). The bridge should flag this with a diagnostic about event publishers not having local variables.

- [ ] **Step 5: Test — verify third-party analyzer support**

Configure a custom analyzer in settings:
```json
"al.codeAnalyzers": ["CodeCop", "/path/to/BusinessCentral.LinterCop.dll"]
```

Verify the custom DLL's diagnostics appear alongside standard ones.

- [ ] **Step 6: Commit**

---

## Task 6: Wire Remaining Settings + Per-Rule Lint Overrides

**Files:**
- Modify: `crates/al-syntax/src/lint.rs` — check per-rule overrides
- Modify: `crates/al-core/src/config.rs` — ensure all defaults are sensible

- [ ] **Step 1: Wire `native_lint_rules` per-rule overrides in lint walker**

In `lint.rs`, the `lint()` function should accept a config parameter and skip rules disabled by the user:

```rust
pub fn lint_with_config(tree: &Tree, text: &str, config: &LintConfig) -> Vec<LintDiagnostic> {
    if !config.enabled {
        return vec![];
    }
    let mut diagnostics = Vec::new();
    // ... existing lint logic, but check config.rules before emitting:
    if config.is_rule_enabled("AL-L001") {
        // ... check AL-L001
    }
}
```

- [ ] **Step 2: Wire `diagnostics_trigger` setting**

In `schedule_diagnostics()`, check if trigger is `OnSave` — if so, only run on `did_save` not `did_change`:

```rust
let trigger = self.workspace.config.read().await.diagnostics_trigger;
if trigger == DiagnosticsTrigger::OnSave {
    // Skip — diagnostics will run on did_save instead
    return;
}
```

- [ ] **Step 3: Test per-rule override**

Configure `"al.nativeLintRules": { "AL-L008": false }` (disable magic number warnings). Verify AL-L008 no longer fires.

- [ ] **Step 4: Commit**

---

## Verification Criteria

After all tasks:

1. **Settings autocomplete**: Typing in `"lsp": { "al-lsp": { "settings": { }}}` shows suggestions (when v0.8.0 publishes)
2. **Recommended settings popup**: First open of AL project prompts to apply optimal settings
3. **Gutter run buttons**: `[Test]` procedures show ▶ icon; `[IntegrationEvent]` procedures show ▶ icon
4. **Pull diagnostics**: Zed fetches diagnostics on demand alongside our push diagnostics
5. **CodeAnalysis diagnostics**: Permission set warnings, event publisher restrictions, type errors from MS analyzers appear
6. **Per-rule config**: Individual lint rules can be disabled via settings
7. **All settings wired**: Every setting in `schemas/settings.json` has a corresponding behavior

---

## Research Notes (for reference during implementation)

### Runnables — No Upstream Changes Needed

The `runnables.scm` + `tasks.json` tags system works TODAY for WASM extensions. Go, Python, Elixir all use this — none use `experimental/runnables`. The LSP runnables path (`experimental/runnables`) requires a Rust-side `ContextProvider.lsp_task_source()` which is NOT exposed to WASM extensions (Zed issue #47068, PR #45932 pending). For now, tree-sitter tags are sufficient and match what other language extensions use.

### Pull vs Push Diagnostics

Both work simultaneously. Push (`publishDiagnostics`) for instant typing feedback. Pull (`textDocument/diagnostic`) for on-demand fresh data. Zed uses pull when `diagnosticProvider` is declared in capabilities. Our native lint is fast enough for push; CodeAnalysis bridge is better suited for pull (heavier, client controls timing).

### Extension API v0.8.0

`language_server_workspace_configuration_schema()` and `language_server_initialization_options_schema()` are in Zed's main branch but not published to crates.io. Use git dependency for dev; switch to registry version for public release.
