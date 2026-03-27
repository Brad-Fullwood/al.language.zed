# Zed AL Extension — Complete End-to-End Test Plan

**Date:** 2026-03-25
**Status:** Ready for execution
**Scope:** Every feature of the AL language server tested through live Zed IDE

---

## Agent Orchestration Model

This test plan is executed by a **three-agent supervised loop**:

### Agent Roles

| Agent | Role | Model | Description |
|-------|------|-------|-------------|
| **Supervisor** (main) | Orchestrator | opus | Reviews all tester/fixer results. Designs fixes. Detects "testing to pass instead of fail". Investigates gaps. Adds new tests. Never edits code or runs tests directly. |
| **Tester** | Test executor | opus | Runs tests via `al-zed-test` crate against live Zed. Reports pass/fail with evidence (screenshots, logs). Reports any untested features discovered. Never fixes code. |
| **Fixer** | Code fixer | opus | Receives fix designs from Supervisor. Implements fixes. Reports any untested code paths discovered. Never runs tests. |

### Execution Loop

```
┌─────────────────────────────────────────────────┐
│                  SUPERVISOR                      │
│  1. Dispatch test batch to Tester               │
│  2. Review Tester results:                      │
│     - Verify failures are REAL (not test bugs)  │
│     - Verify passes are REAL (not weak asserts) │
│     - Check for "testing to pass" patterns      │
│  3. For each real failure:                      │
│     - Design a fix specification                │
│     - Dispatch to Fixer                         │
│  4. When Fixer completes:                       │
│     - Add re-test tasks to Tester               │
│  5. Process gap reports from both agents:       │
│     - Investigate unexpected features/code      │
│     - Design new tests, add to Tester backlog   │
│  6. Repeat until all tests pass                 │
└─────────────────────────────────────────────────┘
```

### Anti-Patterns the Supervisor Must Catch

- **Weak assertions**: Tester reports "pass" but the verification is just "screenshot taken" with no content check
- **Testing the test**: Tester modifies code to make tests pass instead of verifying real behavior
- **Ignoring failures**: Tester skips a test instead of reporting failure
- **Superficial fixes**: Fixer changes test expectations instead of fixing the actual code
- **Missing coverage**: A feature works but has no test — both agents should flag these

### Gap Reporting

Both Tester and Fixer MUST report discoveries:

**Tester reports:**
- Functions/actions seen in Zed that aren't in this test plan
- LSP log entries for methods not covered by any test
- UI elements (gutter icons, status bar items, panels) not tested

**Fixer reports:**
- Code paths found during fixing that have no corresponding test
- Error handling branches that aren't exercised
- Configuration options that aren't tested

The Supervisor investigates each gap, designs tests, and adds them to the Tester's backlog.

---

## Prerequisites

### Environment

- **Zed** running with AL extension installed, on Hyprland/Wayland
- **Cursor** (VS Code fork) running with AL Language extension for comparison tests
- **tesseract** installed: `sudo pacman -S tesseract tesseract-data-eng`
- **al-lsp** on PATH or configured in both editors
- Both editors must have the test project open

### Test Project

Path: `/home/bradf/Dev/Software/Zed/Zed AL Extension/crates/al-test-harness/data/test_al_project/`

Contents:
- `app.json` — BC 26.5, Cloud target, ID range 50100-50199
- `.alpackages/` — 5 BC standard library .app files (10,808 symbols)
- `src/HelloWorld.al` — codeunit 50100: DoSomething (8 params, unused var x), badName(), empty OnRun, TODO comment
- `src/Table50100.al` — table 50100 "Test Customer": 5 fields, OnValidate trigger, 2 keys
- `src/Page50100.al` — page 50100 "Test Customer Card": Card, SourceTable, 4 fields, CalcBalance action
- `src/Enum50100.al` — enum 50100 "Test Status": 4 values (blank, Open, Released, Closed)
- `src/Interface50100.al` — interface "ITest Processor": 3 procedure signatures
- `src/CodeunitWithEvents.al` — codeunit 50101: [IntegrationEvent] procedures, event calls
- `src/PageExtension50100.al` — pageextension extends "Test Customer Card"
- `src/TableExtension50100.al` — tableextension extends "Test Customer"
- `src/ErrorCases.al` — deliberately malformed AL (missing end, no semicolons)
- `src/DeepNesting.al` — codeunit 50103: 8 levels deep nesting
- `src/MultiProcedure.al` — codeunit 50104: 9+ procedures, global vars, cross-type refs

### BC Connection Details (for debug/snapshot/test tasks)

```json
{
    "environmentType": "Sandbox",
    "tenant": "1b7a9471-d313-4aa4-99e8-7a6c762c8e7d"
}
```

### al-zed-test API Quick Reference

```rust
let zed = ZedTest::connect().await?;           // Find Zed window
zed.open_file(&path).await?;                    // Open file via zeditor CLI
zed.focus()?;                                   // Focus Zed window (hyprctl)
zed.send_keys("ctrl+shift+p")?;                // Key combo (wtype)
zed.send_keys("f12")?;                         // Go-to-definition
zed.send_keys("shift+f12")?;                   // Find references
zed.send_keys("f2")?;                          // Rename
zed.send_keys("ctrl+.")?;                      // Code actions
zed.send_keys("ctrl+shift+i")?;               // Format document
zed.send_keys("ctrl+shift+o")?;               // Document symbols
zed.send_keys("ctrl+k ctrl+0")?;              // Fold all
zed.send_keys("ctrl+k ctrl+j")?;              // Unfold all
zed.type_text("some text")?;                   // Type literal text
zed.goto_line(42)?;                            // Go to line
zed.trigger_completion()?;                      // Ctrl+Space
zed.run_command("zed: open settings")?;        // Command palette
zed.screenshot_to(&path).await?;               // Capture PNG
zed.screenshot().await?;                        // Capture PNG bytes
zed.ocr().await?;                              // Screenshot + tesseract
zed.clipboard()?;                              // Read clipboard
zed.set_clipboard("text")?;                    // Write clipboard
zed.lsp_log_tail(20).await?;                   // Last N log lines
zed.wait_for_lsp_log("pattern", 10000).await?; // Wait for pattern
zed.wait(500).await;                           // Sleep ms
zed.close_tab()?;                              // Ctrl+W
zed.save_all()?;                               // Ctrl+Shift+S
```

### Screenshot Convention

All screenshots: `/tmp/al-zed-e2e/{section}-{test-id}.png`

Create directory first: `mkdir -p /tmp/al-zed-e2e`

### Log Anchoring Pattern

Before every test that checks LSP logs:
```rust
use al_zed_test::lsp_log;
let offset = lsp_log::current_offset().await?;
// ... perform action ...
let new_lines = lsp_log::since_offset(offset).await?;
```

---

## Section 1: Highlighting — Zed vs Cursor Comparison

Compare syntax highlighting character-by-character between Zed and Cursor on the same AL files. Both editors should produce matching token colors for the same source.

**Exception:** Zed displays inlay hints (parameter name labels inline at call sites). This is DESIRED behavior unique to Zed and should be noted as expected, not flagged as a difference.

**Method:** Open the same file in both editors at the same font size. Screenshot both. Use Claude Vision to compare token-by-token. Report any differences with file:line:column and token text.

### H-01: HelloWorld.al — Core Keywords

**File:** `src/HelloWorld.al`
**Steps:**
1. Open HelloWorld.al in Zed: `zed.open_file(&test_dir.join("src/HelloWorld.al")).await?`
2. Wait 2s for LSP: `zed.wait(2000).await`
3. Screenshot Zed: `zed.screenshot_to(Path::new("/tmp/al-zed-e2e/H-01-zed.png")).await?`
4. Open same file in Cursor (manually or via `cursor` CLI)
5. Screenshot Cursor: capture via grim targeting Cursor window
6. Compare using Claude Vision

**Verify these tokens have matching colors:**
- `codeunit` (keyword)
- `50100` (number)
- `"Hello World"` (string — object name)
- `procedure` (keyword.function)
- `DoSomething` (function.definition)
- `a: Integer` — `a` (parameter), `Integer` (type.builtin)
- `var` (keyword.modifier)
- `x` (variable.declaration)
- `begin` / `end` (keyword.control)
- `// TODO: implement properly` (comment)
- `trigger` (keyword.function)
- `OnRun` (function.definition)
- `Message` (function.call / builtinFunction)
- `'Hello World'` (string — literal)

**Expected Zed-only:** Inlay hints showing parameter names at `DoSomething` call sites (if any exist in other files calling it).

### H-02: Table50100.al — Table-Specific Highlighting

**File:** `src/Table50100.al`
**Steps:** Same open/screenshot/compare pattern
**Verify:**
- `table` (keyword — object keyword)
- `field(1; "No."; Code[20])` — `field` (keyword), `1` (number), `"No."` (string/identifier), `Code` (type.builtin), `[20]` (brackets + number)
- `DataClassification = CustomerContent` — property name + property value
- `trigger OnValidate()` — trigger keyword + name
- `keys { key(PK; "No.") { Clustered = true; } }` — `keys` keyword, `key` keyword, `Clustered` property, `true` (constant.builtin)

### H-03: Page50100.al — Page Layout Highlighting

**File:** `src/Page50100.al`
**Verify:**
- `PageType = Card` — property + enum-like value
- `SourceTable = "Test Customer"` — property + string reference
- `area(Content)` — layout keyword + area name
- `group(General)` — layout keyword
- `field("No."; Rec."No.")` — field keyword, string, Rec reference
- `action(CalcBalance)` — action keyword
- `ApplicationArea = All` — property + value

### H-04: Enum50100.al — Enum Highlighting

**File:** `src/Enum50100.al`
**Verify:**
- `enum` keyword
- `Extensible = true` — property + boolean
- `value(0; " ")` — value keyword, ordinal, blank string
- `value(1; "Open")` — value keyword, ordinal, string
- `Caption = 'Open'` — property + caption string

### H-05: CodeunitWithEvents.al — Attributes

**File:** `src/CodeunitWithEvents.al`
**Verify:**
- `[IntegrationEvent(false, false)]` — attribute brackets, attribute name, boolean params
- `var InputValue: Text` — var modifier keyword in parameter
- `var IsHandled: Boolean` — var modifier in parameter

### H-06: DeepNesting.al — Control Flow Keywords

**File:** `src/DeepNesting.al`
**Verify all nested keywords colored consistently:**
- `if` / `then` / `else` / `begin` / `end`
- `case` / `of`
- `repeat` / `until`
- `while` / `do`
- `+=` / `-=` (operators)
- Verify indentation-based visual structure matches keyword colors

### H-07: MultiProcedure.al — Type References

**File:** `src/MultiProcedure.al`
**Verify:**
- `local procedure` — `local` modifier + `procedure` keyword
- `(): Boolean` — return type
- `Record "Test Customer"` — `Record` type keyword + quoted table name
- `Enum "Test Status"` — `Enum` type keyword + quoted enum name
- `GlobalCounter` vs `LocalVar` — global vs local variable (semantic tokens should differentiate)
- `exit()` — built-in function

### H-08: ErrorCases.al — Error Recovery

**File:** `src/ErrorCases.al`
**Verify:**
- Despite parse errors, keywords/strings/comments that CAN be parsed are still highlighted
- Error regions may show as plain text or with error styling
- File doesn't cause highlighting to completely break

### H-09: Semantic Tokens On vs Off

**Steps:**
1. Open HelloWorld.al in Zed
2. Screenshot WITH LSP running: `/tmp/al-zed-e2e/H-09-semantic-on.png`
3. Kill al-lsp: `pkill -f al-lsp` (or stop the language server via command palette)
4. Wait 2s
5. Screenshot WITHOUT LSP: `/tmp/al-zed-e2e/H-09-semantic-off.png`
6. Compare: tree-sitter-only should still color keywords, strings, comments, numbers
7. Semantic-on should ADDITIONALLY color: builtinFunction, globalVariable, localVariable, tableField, pageControl, etc.
8. Restart al-lsp (reopen project or restart language server)

### H-10: Extensions Highlighting

**Files:** `src/PageExtension50100.al`, `src/TableExtension50100.al`
**Verify:**
- `pageextension` / `tableextension` keywords
- `extends "Test Customer Card"` — extends keyword + target name
- `addafter(Name)` / `addlast(Processing)` — modification keywords

### H-11: Interface Highlighting

**File:** `src/Interface50100.al`
**Verify:**
- `interface` keyword
- Procedure signatures without bodies (no `begin`/`end`)
- Parameter types and return types

---

## Section 2: Hover — Zed vs Cursor Comparison

Compare hover popup content between Zed and Cursor. Zed should show equal or MORE information.

**Method:** Position cursor on identifier, trigger hover popup, screenshot. In Zed, use the keyboard shortcut for hover info or just wait with cursor positioned. Compare content between editors.

### HV-01: Procedure Name Hover

**File:** `HelloWorld.al`, line 4, cursor on `DoSomething`
**Steps:**
1. `zed.open_file(&test_dir.join("src/HelloWorld.al")).await?`
2. `zed.wait(2000).await`
3. `zed.goto_line(4)?; zed.wait(200).await`
4. Position cursor on `DoSomething`: `zed.send_keys("home")?` then right-arrow to column 14
5. Wait for hover: `zed.wait(1000).await`
6. Screenshot: `/tmp/al-zed-e2e/HV-01-zed.png`
7. Check LSP log for `textDocument/hover` response
8. Repeat in Cursor, screenshot as `/tmp/al-zed-e2e/HV-01-cursor.png`

**Expected:** Signature showing `procedure DoSomething(a: Integer; b: Integer; c: Integer; d: Integer; e: Integer; f: Integer; g: Integer; h: Integer)`

### HV-02: Local Variable Hover

**File:** `HelloWorld.al`, line 6, cursor on `x`
**Expected:** "local variable: Integer" or similar type info

### HV-03: Built-in Function Hover

**File:** `HelloWorld.al`, line 12, cursor on `Message`
**Expected:** Overload signatures from System app. Multiple signatures showing different parameter combinations.
**Zed should show >= Cursor:** Both should show overloads. Zed may show additional markdown formatting.

### HV-04: Table Field Hover

**File:** `Table50100.al`, line 5, cursor on `"No."`
**Expected:** Field type `Code[20]`, field number, data classification

### HV-05: Record Type Hover

**File:** `MultiProcedure.al`, line with `Record "Test Customer"`, cursor on `"Test Customer"`
**Expected:** Table info — name, ID, field count or similar

### HV-06: Enum Value Hover

**File:** `Enum50100.al`, cursor on `Open` value name
**Expected:** Enum member info with ordinal value 1

### HV-07: Attribute Hover

**File:** `CodeunitWithEvents.al`, cursor on `IntegrationEvent`
**Expected:** Attribute description or at minimum the attribute name recognized

### HV-08: Global Variable Hover

**File:** `MultiProcedure.al`, line 4, cursor on `GlobalCounter`
**Expected:** "global variable: Integer" — must distinguish from local variables

### HV-09: Var Parameter Hover

**File:** `MultiProcedure.al`, cursor on `var Counter` in `VarParams` procedure
**Expected:** "parameter (var): Integer" — must show the `var` modifier

### HV-10: Built-in exit() Hover

**File:** `MultiProcedure.al`, cursor on `exit`
**Expected:** Built-in function description

### HV-11: Keyword Hover (negative)

**File:** `HelloWorld.al`, cursor on `begin`
**Expected:** No hover popup or minimal keyword description. Should NOT show an error.

### HV-12: String Literal Hover (negative)

**File:** `HelloWorld.al`, cursor on `'Hello World'` string
**Expected:** No hover popup

---

## Section 3: Completions

### C-01: Keyword Completion

**File:** MultiProcedure.al, inside object body (between procedures)
**Steps:**
1. Open file, goto end of a procedure, press Enter twice to create space
2. Type `proc`
3. Wait 500ms for completion popup
4. Screenshot: verify `procedure` appears in completion list
5. Press Escape, undo changes

### C-02: Dot-Access Record Fields

**File:** Page50100.al, inside an action trigger
**Steps:**
1. Open file, navigate to inside `trigger OnAction()` body
2. Type `Rec.`
3. Wait for completion (auto-triggered by `.`)
4. Screenshot: verify table fields appear (No., Name, Balance, Contact Name, Active)
5. Verify in LSP log: `textDocument/completion` response contains field items
6. Escape and undo

### C-03: Enum Value Completion

**File:** MultiProcedure.al, inside `WithEnum` procedure
**Steps:**
1. Navigate to inside the procedure body
2. Type `"Test Status"::`
3. Wait for completion (auto-triggered by `::`)
4. Screenshot: verify enum values appear (Open, Released, Closed)
5. Escape and undo

### C-04: Built-in Function Completion

**File:** HelloWorld.al, inside OnRun trigger
**Steps:**
1. Navigate to inside empty `begin/end` of OnRun
2. Type `Mes`
3. Wait 500ms
4. Screenshot: verify `Message` overloads appear
5. Escape and undo

### C-05: Local Variable Completion

**File:** MultiProcedure.al, inside `WithParams` procedure
**Steps:**
1. Navigate into the procedure body after `LocalVar := a * 2;`
2. Type `Local`
3. Verify `LocalVar` appears in completions
4. Escape and undo

### C-06: Global Variable Completion

**File:** MultiProcedure.al, inside any procedure
**Steps:**
1. Type `Global`
2. Verify both `GlobalCounter` and `GlobalName` appear
3. Escape and undo

### C-07: Procedure Call Completion

**File:** MultiProcedure.al, inside `CallsOthers` procedure
**Steps:**
1. Navigate to end of `CallsOthers` body
2. Type `Simple`
3. Verify `SimpleProc` appears
4. Escape and undo

### C-08: Trigger Character `.` Auto-triggers

**Steps:**
1. In any procedure, type `Rec.` (no Ctrl+Space)
2. Verify completion popup appears automatically
3. Escape and undo

### C-09: Accept Completion with Tab

**Steps:**
1. Trigger completion showing `Message`
2. Press Tab (or Enter)
3. Verify `Message` was inserted into the editor
4. Undo

### C-10: Dismiss Completion with Escape

**Steps:**
1. Trigger completion
2. Press Escape
3. Verify popup disappeared
4. Verify no text was inserted

### C-11: Filter While Typing

**Steps:**
1. Trigger completion with `M` (shows many items)
2. Continue typing `es` → `Mes`
3. Verify list narrows to Message-related items
4. Escape and undo

### C-12: Completion in Error File

**File:** ErrorCases.al
**Steps:**
1. Open ErrorCases.al (has parse errors)
2. Navigate into a partially parsed procedure
3. Trigger completion
4. Verify some completions appear (keywords at minimum)
5. Verify no crash

### C-13: Completion Outside Procedure (negative)

**Steps:**
1. Position cursor at top level (between procedures, outside any block)
2. Trigger completion
3. Verify only appropriate items (procedure, trigger, var — not local variables)

### C-14: Completion in Comment (negative)

**Steps:**
1. Position cursor inside `// TODO: implement properly`
2. Trigger completion
3. Verify no completion popup appears (or minimal)

### C-15: Completion in String (negative)

**Steps:**
1. Position cursor inside `'Hello World'`
2. Trigger completion
3. Verify no completion popup

---

## Section 4: Go-to-Definition

### D-01: Local Procedure Call → Definition

**File:** MultiProcedure.al
**Steps:**
1. Open file, `goto_line(56)` (line with `SimpleProc();` call)
2. Position cursor on `SimpleProc`: `send_keys("home")`, then right-arrow to column 8
3. `send_keys("f12")` — go to definition
4. Wait 500ms
5. Screenshot: verify cursor moved to line 7 (`procedure SimpleProc()`)
6. Verify in LSP log: `textDocument/definition` response shows line 7

### D-02: Cross-File Type Reference

**File:** MultiProcedure.al, line with `Record "Test Customer"`
**Steps:**
1. Position cursor on `"Test Customer"` text
2. `send_keys("f12")`
3. Verify Zed opens `Table50100.al` and navigates to the table declaration
4. Screenshot

### D-03: Table Field from Page

**File:** Page50100.al, cursor on a field control's source field name
**Steps:**
1. Open Page50100.al
2. Position cursor on a field source reference (e.g., `Rec."No."`)
3. `send_keys("f12")`
4. Verify navigates to Table50100.al field declaration

### D-04: Enum Type Reference

**File:** MultiProcedure.al, cursor on `Enum "Test Status"`
**Steps:**
1. Position cursor on `"Test Status"`
2. `send_keys("f12")`
3. Verify navigates to Enum50100.al

### D-05: Local Variable → Declaration

**File:** MultiProcedure.al, cursor on `LocalVar` usage in procedure body
**Steps:**
1. Navigate to `LocalVar := a * 2;`
2. Position on `LocalVar`
3. `send_keys("f12")`
4. Verify jumps to `var` section where `LocalVar: Integer` is declared

### D-06: Global Variable → Declaration

**File:** MultiProcedure.al, cursor on `GlobalCounter` usage inside `SimpleProc`
**Steps:**
1. Navigate to `GlobalCounter += 1;` inside SimpleProc
2. Position on `GlobalCounter`
3. `send_keys("f12")`
4. Verify jumps to global `var` section (line ~4)

### D-07: Extension Base → Source

**File:** PageExtension50100.al, cursor on `extends "Test Customer Card"`
**Steps:**
1. Position cursor on `"Test Customer Card"`
2. `send_keys("f12")`
3. Verify navigates to Page50100.al

### D-08: Go-to-def on Keyword (negative)

**Steps:**
1. Position cursor on `begin` keyword
2. `send_keys("f12")`
3. Verify no navigation occurs (cursor stays on same line)

### D-09: Go-to-def on String Literal (negative)

**Steps:**
1. Position cursor inside a string literal
2. `send_keys("f12")`
3. Verify no navigation

---

## Section 5: Find References

### R-01: Procedure References

**File:** MultiProcedure.al, cursor on `SimpleProc` at declaration (line 7)
**Steps:**
1. Position cursor on `SimpleProc` procedure name at its declaration
2. `send_keys("shift+f12")` — find all references
3. Wait 1s
4. Screenshot references panel
5. Verify: at minimum 2 locations — declaration (line 7) + call (line 56)

### R-02: Global Variable References

**File:** MultiProcedure.al, cursor on `GlobalCounter` declaration
**Steps:**
1. Position cursor on `GlobalCounter` in var section
2. `send_keys("shift+f12")`
3. Verify: declaration + usage in `SimpleProc` + usage in `WithReturn`

### R-03: Cross-File References

**File:** Table50100.al, cursor on table name `"Test Customer"`
**Steps:**
1. Position cursor on the table name
2. `send_keys("shift+f12")`
3. Verify: references from Page50100.al (SourceTable), MultiProcedure.al (Record type), extensions

### R-04: Field References

**File:** Table50100.al, cursor on field `"No."`
**Steps:**
1. Position cursor on `"No."` field name
2. `send_keys("shift+f12")`
3. Verify: references from Page50100.al field controls

---

## Section 6: Document Symbols & Workspace Symbols

### Document Symbols

### S-01: Codeunit Symbols

**File:** MultiProcedure.al
**Steps:**
1. Open file
2. `send_keys("ctrl+shift+o")` — open document symbol picker
3. Wait 500ms, screenshot
4. Verify: all 9+ procedures listed (SimpleProc, WithReturn, WithParams, LocalHelper, VarParams, MultiReturn, WithRecord, CallsOthers, WithEnum, LongSignature)
5. Verify: global variables listed (GlobalCounter, GlobalName)
6. Press Escape

### S-02: Table Symbols

**File:** Table50100.al
**Steps:**
1. `send_keys("ctrl+shift+o")`
2. Verify: table name, all 5 fields, triggers, keys visible in hierarchy
3. Escape

### S-03: Page Symbols

**File:** Page50100.al
**Steps:**
1. `send_keys("ctrl+shift+o")`
2. Verify: page, layout areas, field controls, actions visible
3. Escape

### S-04: Enum Symbols

**File:** Enum50100.al
**Steps:**
1. `send_keys("ctrl+shift+o")`
2. Verify: enum name + all 4 values
3. Escape

### S-05: Error File Symbols

**File:** ErrorCases.al
**Steps:**
1. `send_keys("ctrl+shift+o")`
2. Verify: partial symbols extracted despite parse errors — at minimum the codeunit name
3. Escape

### Workspace Symbols

### S-06: Search by Name

**Steps:**
1. `send_keys("ctrl+t")` — workspace symbol search (or `ctrl+shift+t`)
2. Type `Customer`
3. Wait 500ms, screenshot
4. Verify: Table50100, Page50100, extensions appear
5. Escape

### S-07: Search Procedure

**Steps:**
1. `send_keys("ctrl+t")`
2. Type `SimpleProc`
3. Verify: procedure from MultiProcedure.al appears
4. Escape

### S-08: Case-Insensitive Search

**Steps:**
1. `send_keys("ctrl+t")`
2. Type `test customer` (lowercase)
3. Verify: matches found (case-insensitive)
4. Escape

---

## Section 7: Diagnostics

### DG-01: AL-L001 — Empty Begin/End

**File:** HelloWorld.al, `DoSomething` procedure has empty `begin/end`
**Verify:** Warning diagnostic with squiggle on the empty block
**LSP log:** `publishDiagnostics` contains code `"AL-L001"`

### DG-02: AL-L005 — Unused Variable

**File:** HelloWorld.al, `x: Integer` declared but never used
**Verify:** Warning squiggle on the variable declaration

### DG-03: AL-L006 — Empty Trigger

**File:** HelloWorld.al, `OnRun()` trigger has empty body
**Verify:** Hint diagnostic (subtle indicator)

### DG-04: AL-L007 — TODO Comment

**File:** HelloWorld.al, `// TODO: implement properly`
**Verify:** Info diagnostic on the comment

### DG-05: AL-L016 — Procedure Naming

**File:** HelloWorld.al, `badName` not PascalCase
**Verify:** Warning on the procedure name

### DG-06: AL-L004 — Deep Nesting

**File:** DeepNesting.al
**Verify:** Warning triggered at excessive nesting depth (threshold: 5, file has 8)

### DG-07: AL-L009 — Excessive Parameters

**File:** HelloWorld.al, `DoSomething` has 8 params (threshold: 7)
**Verify:** Warning on procedure declaration

### DG-08: AL-L003 — Missing Semicolon

**File:** ErrorCases.al, `x := 1` without semicolon
**Verify:** Error diagnostic (parser error)

### DG-09: AL-L017 — Hardcoded String

**File:** HelloWorld.al, `'Hello World'` string literal
**Verify:** Info diagnostic suggesting Label variable

### DG-10: Parse Errors in ErrorCases.al

**Verify:** Multiple diagnostics shown for malformed AL (missing end, extra begin)

### DG-11: Diagnostics Clear After Fix

**Steps:**
1. Open HelloWorld.al, note diagnostics
2. Add `x := 1;` inside DoSomething body (fixes AL-L001 empty block, uses variable x)
3. Wait 1s for diagnostics to update
4. Verify: AL-L001 and AL-L005 diagnostics disappear
5. Undo changes
6. Verify: diagnostics reappear

### DG-12: Clean File — Zero Diagnostics

**Steps:**
1. Create a well-formed procedure with no issues (or find one in MultiProcedure.al)
2. Verify: no diagnostic squiggles on clean code

---

## Section 8: Code Actions

### Quick-Fix Actions

### CA-01: AL-L001 — Add Placeholder Statement

**File:** HelloWorld.al, on the empty `begin/end` in DoSomething
**Steps:**
1. Position cursor on the empty block diagnostic
2. `send_keys("ctrl+.")` — trigger code actions
3. Wait 500ms, screenshot
4. Verify: "Add placeholder Error statement" option appears
5. Select it (Enter)
6. Verify: `Error('Not implemented');` or similar inserted between begin/end
7. Undo

### CA-02: AL-L005 — Remove Unused Variable

**File:** HelloWorld.al, on the unused `x` variable
**Steps:**
1. Position cursor on `x: Integer` diagnostic
2. `send_keys("ctrl+.")`
3. Verify: option to remove unused variable
4. Apply, verify line removed
5. Undo

### CA-03: AL-L016 — Fix PascalCase

**File:** HelloWorld.al, on `badName` procedure
**Steps:**
1. Position cursor on `badName`
2. `send_keys("ctrl+.")`
3. Verify: option to rename to PascalCase
4. Apply, verify renamed to `BadName`
5. Undo

### Source Actions

### CA-04: Add Procedure Documentation

**File:** MultiProcedure.al, on `WithParams` procedure
**Steps:**
1. Position cursor on `procedure WithParams`
2. `send_keys("ctrl+.")`
3. Look for "Add procedure documentation" option
4. Apply, verify XML doc comment inserted above with `<param>` entries for `a` and `b`
5. Undo

### CA-05: Wrap in Region

**Steps:**
1. Select a block of code (Shift+arrow or Shift+Down multiple times)
2. `send_keys("ctrl+.")`
3. Look for "Wrap in region"
4. Apply, verify `//region` and `//endregion` markers added
5. Undo

### CA-06: Make Procedure Local

**File:** MultiProcedure.al, on a non-local procedure
**Steps:**
1. Position cursor on `procedure SimpleProc()`
2. `send_keys("ctrl+.")`
3. Look for "Make procedure local"
4. Apply, verify `local` keyword prepended
5. Undo

---

## Section 9: Formatting

### FMT-01: Full Document Format

**File:** Create a copy with intentionally wrong indentation
**Steps:**
1. Open MultiProcedure.al
2. Manually mess up indentation on a few lines (add extra spaces)
3. `send_keys("ctrl+shift+i")` — format document
4. Wait 500ms
5. Verify: indentation corrected to consistent levels
6. Screenshot before/after
7. Undo

### FMT-02: Format Preserves Structure

**File:** DeepNesting.al (complex nesting)
**Steps:**
1. Open file
2. `send_keys("ctrl+shift+i")`
3. Verify: all nesting levels have correct indentation
4. Verify: no content changes (only whitespace)

### FMT-03: Format via Command Palette

**Steps:**
1. `zed.run_command("editor: format")?`
2. Verify: formatting applied

---

## Section 10: Signature Help & Inlay Hints

### Signature Help

### SH-01: Procedure Call Signature

**File:** MultiProcedure.al, inside CallsOthers
**Steps:**
1. Navigate after `WithParams(` on line 58
2. Wait 500ms for signature help popup
3. Screenshot: verify shows `a: Integer; b: Text` with active parameter highlighted
4. Check LSP log for `textDocument/signatureHelp`

### SH-02: Built-in Function Signature

**Steps:**
1. Type `Message(` in a procedure body
2. Verify signature help shows overloads
3. Undo

### SH-03: Parameter Advance

**Steps:**
1. Inside a function call, type first argument and `,`
2. Verify active parameter advances to second param in popup
3. Undo

### Inlay Hints

### IH-01: Parameter Name Hints at Call Sites

**File:** MultiProcedure.al, on `WithParams(42, 'test')` call (line 58)
**Steps:**
1. Open file, navigate to line 58
2. Wait 1s for inlay hints to render
3. Screenshot
4. Verify: inline labels `a:` and `b:` appear before the arguments
5. Check LSP log for `textDocument/inlayHint`

### IH-02: Multi-Parameter Hints

**File:** MultiProcedure.al, on `MultiReturn(GlobalCounter)` or long-signature calls
**Steps:**
1. Navigate to a call with multiple arguments
2. Verify each parameter has an inline label

---

## Section 11: Rename

### RN-01: Rename Local Variable

**File:** MultiProcedure.al
**Steps:**
1. Position cursor on `LocalVar` in `WithParams` procedure
2. `send_keys("f2")` — rename
3. Type `MyVar`
4. Press Enter
5. Verify: all occurrences of `LocalVar` in the procedure updated to `MyVar`
6. Check LSP log for `textDocument/rename`
7. Undo (`ctrl+z` multiple times)

### RN-02: Rename Procedure

**File:** MultiProcedure.al
**Steps:**
1. Position cursor on `SimpleProc` at declaration
2. `send_keys("f2")`
3. Type `EasyProc`
4. Press Enter
5. Verify: declaration AND call site (line 56) both updated
6. Undo

### RN-03: Rename Global Variable

**Steps:**
1. Position cursor on `GlobalCounter`
2. `send_keys("f2")`
3. Type `Counter`
4. Verify: all usages across all procedures updated
5. Undo

### RN-04: Rename Rejected on Keyword (negative)

**Steps:**
1. Position cursor on `begin` keyword
2. `send_keys("f2")`
3. Verify: no rename dialog appears (prepare_rename fails)

---

## Section 12: Folding & Indentation

### Folding

### FL-01: Fold Procedure

**File:** MultiProcedure.al
**Steps:**
1. Navigate to `SimpleProc` declaration
2. Click fold indicator in gutter (or use keyboard shortcut)
3. Verify: procedure body hidden, fold indicator shows
4. Screenshot

### FL-02: Fold All

**Steps:**
1. `send_keys("ctrl+k")` then `send_keys("ctrl+0")` — fold all
2. Wait 500ms
3. Screenshot: verify all procedures/blocks collapsed

### FL-03: Unfold All

**Steps:**
1. `send_keys("ctrl+k")` then `send_keys("ctrl+j")` — unfold all
2. Verify: everything expanded

### Indentation

### IN-01: Auto-Indent After begin

**Steps:**
1. Position at end of a `begin` line
2. Press Enter
3. Verify: new line is indented one level deeper than `begin`
4. Undo

### IN-02: Auto-Outdent on end

**Steps:**
1. Type `end;` on an indented line
2. Verify: line outdents to match the corresponding `begin`
3. Undo

---

## Section 13: Insight Engine & Analysis (al-cli)

All tests run from Zed's integrated terminal (`ctrl+backtick` to open).

### AN-01: Event Trace

**Command:** `al trace OnBeforeProcess --json`
**Verify:** JSON output showing propagation chain from publisher to any subscribers

### AN-02: Dead Code Detection

**Command:** `al dead-code --json`
**Verify:** JSON listing unused procedures, unused fields, orphaned subscribers (if any)

### AN-03: Entry Points

**Command:** `al entrypoints --json`
**Verify:** Lists procedures with no incoming calls

### AN-04: Complexity Metrics

**Command:** `al metrics MultiProcedure.al --json`
**Verify:** Per-procedure cyclomatic and cognitive complexity scores

### AN-05: SQL Pattern Scan

**Command:** `al sql-scan --json`
**Verify:** SQL anti-pattern results (or empty if none found)

### AN-06: Impact Analysis

**Command:** `al impact SimpleProc --json`
**Verify:** Shows that `CallsOthers` calls `SimpleProc`

### AN-07: Call Graph Export

**Command:** `al graph --format dot`
**Verify:** Valid DOT format output with nodes and edges

### AN-08: Dependency Graph

**Command:** `al deps --json`
**Verify:** Shows 5 implicit BC dependencies

### AN-09: Insight Stats

**Command:** `al insight-stats --json`
**Verify:** Returns node count and edge count > 0

### AN-10: Lint Rules List

**Command:** `al rules --json`
**Verify:** All 22 lint rules listed with codes, descriptions, default severity

---

## Section 14: al-explorer TUI Testing

### EX-01: Launch Explorer

**Steps:**
1. Open terminal in Zed: `send_keys("ctrl+backtick")`
2. Type: `al-explorer /path/to/test_al_project`
3. Wait 3s for TUI to render
4. Screenshot

### EX-02: Object Browser Navigation

**Steps:**
1. In explorer, verify package list visible
2. Navigate to a package (arrow keys)
3. Press Enter to expand
4. Verify object list shows
5. Navigate to "Test Customer" table
6. Verify detail pane shows fields

### EX-03: Search

**Steps:**
1. Type `/` or focus search box
2. Type `Customer`
3. Verify: filtered results

### EX-04: Open in Zed

**Steps:**
1. Select an object
2. Press Enter (or whatever the "open" key is)
3. Verify: Zed opens the corresponding file

### EX-05: Event Chain View

**Steps:**
1. Switch to Event Chain view (key shortcut — check al-explorer help)
2. Search for an event name
3. Verify: chain results displayed

### EX-06: Exit

**Steps:**
1. Press `q`
2. Verify: clean exit back to terminal prompt

---

## Section 15: al-cli Command Testing

Run each from Zed terminal. Verify JSON output structure.

### Symbol Queries

- **CLI-01:** `al search Customer --json` → matches objects
- **CLI-02:** `al object table "Test Customer" --json` → table details
- **CLI-03:** `al by-id table 50100 --json` → Table50100
- **CLI-04:** `al packages --json` → 5+ packages listed
- **CLI-05:** `al composed "Test Customer" --json` → base + extensions merged
- **CLI-06:** `al events OnBeforeProcess --json` → event publishers
- **CLI-07:** `al subscribers OnBeforeProcess --json` → subscribers

### Language Features

- **CLI-08:** `al hover src/HelloWorld.al:4:14 --json` → procedure signature
- **CLI-09:** `al definition src/MultiProcedure.al:56:8 --json` → line 7
- **CLI-10:** `al references src/MultiProcedure.al:7:14 --json` → 2+ locations
- **CLI-11:** `al completions src/MultiProcedure.al:22:8 --json` → completion items
- **CLI-12:** `al symbols src/MultiProcedure.al --json` → document symbols
- **CLI-13:** `al tokens src/HelloWorld.al --json` → semantic tokens
- **CLI-14:** `al hints src/MultiProcedure.al --json` → inlay hints

### Code Quality

- **CLI-15:** `al lint src/HelloWorld.al --json` → AL-L001, L005, L006, L007, L016
- **CLI-16:** `al lint --all --json` → all workspace diagnostics
- **CLI-17:** `al format src/HelloWorld.al --check --json` → format diff
- **CLI-18:** `al fix src/HelloWorld.al --dry-run --json` → fix preview
- **CLI-19:** `al rules --json` → 22 rules
- **CLI-20:** `al parse src/HelloWorld.al --json` → parse stats
- **CLI-21:** `al sort-members src/MultiProcedure.al --dry-run --json` → sort preview

### Toolchain

- **CLI-22:** `al setup --json` → toolchain status
- **CLI-23:** `al doctor --json` → health check
- **CLI-24:** `al version` → version string
- **CLI-25:** `al error-codes --json` → error code list (if bridge available)
- **CLI-26:** `al builtins --json` → built-in types (if bridge available)

---

## Section 16: Terminal Tasks

Run tasks from Zed's command palette.

### TK-01: Parse Current File

**Steps:**
1. Open HelloWorld.al
2. `zed.run_command("task: spawn")?` or use the task runner
3. Select "AL: Parse Current File"
4. Verify: terminal shows parse result with node count

### TK-02: Lint File

**Steps:**
1. Run AL lint task from language tasks
2. Verify: lint output in terminal

### TK-03: Format File

**Steps:**
1. Run AL format task
2. Verify: formatting applied or "already formatted" message

### TK-04: Doctor

**Steps:**
1. Run "AL: Doctor" or similar health check task
2. Verify: outputs toolchain status, bridge status

### TK-05: Open Object Explorer

**Steps:**
1. Run "AL: Open Object Explorer" from language tasks
2. Verify: al-explorer launches in the terminal panel

---

## Section 17: Extension Lifecycle

### LC-01: Fresh Project Open

**Steps:**
1. Close test project in Zed
2. Reopen: `zed.open_file(&test_dir).await?`
3. Wait 10s
4. Verify LSP log: `initialized`, `Scanned workspace`, `Loaded symbol packages`
5. Open a file, verify diagnostics appear

### LC-02: Multiple Files Open

**Steps:**
1. Open 5 different AL files simultaneously
2. Wait 5s
3. Verify: each file gets diagnostics (check log for 5 `did_open` + 5 `publishDiagnostics`)

### LC-03: Edit Cycle

**Steps:**
1. Open HelloWorld.al
2. Add `x := 42;` in DoSomething body
3. Wait 1s, verify diagnostics update (AL-L001 should clear, AL-L005 should clear)
4. Undo
5. Wait 1s, verify diagnostics return

### LC-04: File Close

**Steps:**
1. Open a file, note diagnostics
2. Close tab: `send_keys("ctrl+w")`
3. Verify LSP log shows `did_close`
4. Verify diagnostics panel clears for that file

### LC-05: Settings Hot-Reload

**Steps:**
1. `zed.run_command("zed: open settings")?`
2. Add/change an AL setting (e.g., `"lsp": { "al-lsp": { "settings": { "al.maxProcedureLines": 50 } } }`)
3. Save
4. Verify LSP log shows `did_change_configuration`
5. Verify: AL-L002 threshold changed (procedures > 50 lines now flagged)
6. Revert settings change

### LC-06: LSP Crash Recovery

**Steps:**
1. Verify LSP is working (hover on something)
2. Kill al-lsp: run `pkill -f "al-lsp.*stdio"` in terminal
3. Wait 5s
4. Verify: Zed detects the crash and restarts al-lsp
5. Open a file, verify features resume (hover, completions)
6. Check log for new `initialized` entry

### LC-07: Rapid Typing

**Steps:**
1. Open MultiProcedure.al
2. Navigate into a procedure body
3. Type rapidly: `abcdefghijklmnop` (16 chars fast)
4. Wait 2s
5. Verify: no crash, diagnostics still update, completions still work
6. Undo

---

## Section 18: Snippets & Text Objects

### Snippets

### SN-01: Procedure Snippet

**Steps:**
1. Position in object body (between procedures)
2. Type `tprocedure`
3. Press Tab
4. Verify: procedure skeleton expands with tab stops for name, parameters, body
5. Navigate tab stops, type values
6. Undo

### SN-02: If Statement Snippet

**Steps:**
1. Inside a procedure body, type `tif`
2. Press Tab
3. Verify: `if ... then begin ... end;` skeleton expands
4. Undo

### SN-03: Case Statement Snippet

**Steps:**
1. Type `tcaseof`
2. Press Tab
3. Verify: case statement skeleton
4. Undo

### SN-04: Event Subscriber Snippet

**Steps:**
1. Type `teventsub`
2. Press Tab
3. Verify: `[EventSubscriber(...)]` procedure skeleton with attribute
4. Undo

### SN-05: Codeunit Snippet

**Steps:**
1. In a new empty file (or at top level)
2. Type `tcodeunit`
3. Press Tab
4. Verify: full codeunit skeleton with ID, name, triggers
5. Undo

### Runnables

### RU-01: Event Publisher Gutter Icon

**File:** CodeunitWithEvents.al
**Steps:**
1. Open file
2. Look at gutter on lines with `[IntegrationEvent]` procedures
3. Screenshot: verify run icon/indicator present
4. These should be tagged as `al-event-publisher` per runnables.scm

### RU-02: Test Gutter Icon (if test codeunit exists)

**Steps:**
1. If a `[Test]` procedure exists in the project, open that file
2. Verify gutter run icon for test procedures

---

## Summary Report Format

After all tests complete, the Tester agent produces:

```markdown
# Zed AL Extension E2E Test Report — {date}

## Overview
| Section | Total | Pass | Fail | Skip | Notes |
|---------|-------|------|------|------|-------|
| 1. Highlighting | 11 | ? | ? | ? | |
| 2. Hover | 12 | ? | ? | ? | |
| ... | ... | ... | ... | ... | |
| **TOTAL** | **~120** | **?** | **?** | **?** | |

## Failures

### {test-id}: {test-name}
**Expected:** ...
**Actual:** ...
**Screenshot:** /tmp/al-zed-e2e/{id}.png
**LSP Log:** ...
**Root Cause Hypothesis:** ...

## Gaps Discovered
- {description of untested feature/code path found}

## Recommendations
- {suggested new tests or fixes}
```

The Supervisor reviews this report, designs fixes for failures, dispatches to Fixer, then re-tests.
