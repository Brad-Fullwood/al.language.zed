# R1b review: al-analysis scaffold.rs and generators.rs

Produced by a sub-reviewer on 2026-09-21. All paths are under `crates/al-analysis/src/`.
Checked and found sound: path traversal in `materialize_custom_template` (`guard_relative`
covers source and substituted destination, symlinks rejected), no reachable panics.

### [BUG] Apostrophe in the project name produces an unparseable starter codeunit
- where: scaffold.rs:788-800
- severity: high
- scenario: `al new --name "Brad's App"`. Line 789 escapes with `al_escape_name` (doubles `"` only), then line 795 puts the result inside a single-quoted AL string: `Message('Hello from Brad's App!');`. The file does not parse. Affects the `default`, `pte` and `appsource` templates. `--name 'My"App'` prints `My""App`.
- fix: escape for the literal context with `config.name.replace('\'', "''")`, as `generate_azure_openai_codeunit` at line 872 already does. Add a parse test with that name.
- status: fixed 63b47d6a

### [BUG] Field names with parentheses, `%` or a leading digit produce invalid control and column names
- where: generators.rs:255-277, used at 194/200 and 216/222
- severity: high
- scenario: `al generate page --table "Cust. Ledger Entry"`. `al_identifier("Amount (LCY)")` returns `amount(LCY)`, so the page has `field(amount(LCY); Rec."Amount (LCY)")` and does not compile. `Line Discount %` gives `lineDiscount%`. `generate_report_columns` has the same defect.
- fix: keep only `[A-Za-z0-9_]`, prefix a letter when the result starts with a digit or is empty. Test `Amount (LCY)`, `Line Discount %`, and a round-trip parse.
- status: fixed d375712c

### [BUG] Scaffolding overwrites an existing .gitignore and src files
- where: scaffold.rs:169-216
- severity: high
- scenario: the only collision check is `dir/app.json` (line 170). `al new --dir <git repo with a .gitignore>` replaces `.gitignore` through `atomic_write` with no warning. Same for `src/HelloWorld.Codeunit.al`, `.zed/debug.json`, and every path under a custom template's `files/` (line 499).
- fix: check every destination before writing and fail with the list of files that would be replaced, or require `--force`.
- status: fixed 38915e1c

### [BUG] Derived object names exceed AL's 30-character limit
- where: scaffold.rs:802-960
- severity: medium
- scenario: `al new --template copilot --name "Sales Order Copilot"` emits `codeunit 50100 "Sales Order Copilot Copilot Participant"` (39 characters, compiler error AL0305). Limits by template: copilot and helper break above 10 characters of project name, agent job handler above 12, library 22, agent 24, test 25, api 26.
- fix: validate `name + suffix <= 30` per template in `create_project` and reject with a message naming the limit.
- status: fixed 021b2c51

### [BUG] Generated test codeunit declares a dependency the project does not have
- where: generators.rs:152-166
- severity: medium
- scenario: `al generate test` always emits `var Assert: Codeunit "Library Assert";` (line 159). Without Microsoft's test library the file fails to compile. With it, the variable is unused. `generate_test_codeunit` in scaffold.rs:816-831 emits the same stub without it.
- fix: drop the `var Assert` block, or emit it only when the project depends on the test library.
- status: fixed d98b159b

### [BUG] Negative and base-range object IDs reach the generators
- where: generators.rs:54-166, codegen.rs:293-299
- severity: medium
- scenario: `al generate page --id -5 --table Customer` emits `page -5 "NewPage"`. `extract_i32` accepts negatives. `--id 18` is also accepted.
- fix: reject non-positive IDs in `dispatch_generate`. Warn when the ID is outside the `idRanges` in `app.json`.
- status: fixed d98b159b

### [BUG] Two public procedures can collapse into one duplicate test stub name
- where: generators.rs:231-249, 279-283
- severity: medium
- scenario: a codeunit with `procedure PostSale()` and `procedure "Post Sale"()`. `sanitize_identifier` maps both to `PostSale`, so `procedure TestPostSale()` is emitted twice.
- fix: de-duplicate stub names with a numeric suffix.
- status: fixed d98b159b

### [BUG] API template entity names do not match its source table
- where: scaffold.rs:919-960
- severity: low
- scenario: `EntityName = 'item'; EntitySetName = 'items';` over `SourceTable = Customer`. The endpoint `/items` returns customers. `APIPublisher = 'defaultPublisher'` and `APIGroup = 'defaultGroup'` are placeholders that AppSourceCop flags.
- fix: use `'customer'` and `'customers'`, and derive publisher and group from the project config.
- status: fixed adb67d45

### [BUG] A project name that looks like a placeholder gets substituted twice
- where: scaffold.rs:445-463
- severity: low
- scenario: `--name "{{publisher}}"` with a custom template. `substitute` applies pairs in sequence, so the inserted `{{publisher}}` is replaced by the publisher in the next pass.
- fix: one pass over the input that looks each `{{token}}` up in a map.
- status: fixed adb67d45

### [SLOP] Unreachable fallback and a filter for a name shape that is never produced
- where: generators.rs:268-276 and 173
- severity: low
- scenario: after the `out.is_empty()` early return, the `None => String::new()` arm at line 273 is dead. No code in al-symbols produces a `$`-prefixed field name, so the filter at line 173 guards nothing.
- fix: remove both.
- status: fixed adb67d45

### [SIMPLIFY] Duplicate placeholder stub and parse-test helper
- where: generators.rs:251-253 and scaffold.rs:816-831, generators.rs:552-559 and scaffold.rs:1279-1286
- severity: low
- scenario: `default_test_stub()` and `generate_test_codeunit` emit the same text, asserted separately in both test modules. `assert_al_parses` is copied into both.
- fix: `generate_test_codeunit` calls `default_test_stub()`. Move `assert_al_parses` to a shared `#[cfg(test)]` module.
- status: fixed adb67d45

### [UNVERIFIED] Items the reviewer could not confirm — all three checked, all three wrong
- The copilot template's `Codeunit::"Copilot Chat"` subscription and the two-argument
  `GenerateTextCompletion(Prompt, Completion)`: both are fabricated. "Build the Copilot capability
  in AL" says chat with Copilot is not extensible and the AI module cannot influence it, so there
  is no `OnGenerateCompletion` to subscribe to; a capability is a value an extension adds to the
  `Copilot Capability` enum and registers at install time. Every `GenerateTextCompletion` overload
  on codeunit "Azure OpenAI" (7771) takes a `SecretText` prompt and a
  `var AOAIOperationResponse: Codeunit "AOAI Operation Response"` and returns the completion as
  `Text`. The template now emits a `Copilot Capability` enumextension and a codeunit that calls
  the documented overload and checks `IsSuccess`.
  - status: fixed 187b1741
- An empty `repeater(Group) { }` when every non-system field is a FlowField: reachable, and
  `collect_normal_fields` now falls back to the FlowFields when excluding them would leave the
  page with no controls at all.
  - status: fixed 187b1741
- `UsageCategory = Lists` on Card and Document pages: wrong on both counts. "Page types and
  layouts" says an entity-oriented page must not contain a repeater, so `Card` and `Document`
  now open a `group(General)`. The `UsageCategory` property page has no card value and a card is
  opened from its list through `CardPageId`, so the property is left off a `Card` and set to
  `Documents` on a `Document`.
  - status: fixed 187b1741

## Review complete
