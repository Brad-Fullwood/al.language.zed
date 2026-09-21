---
name: bc-workspace-health
description: Audit a Business Central extension before a build or deploy - duplicate or out-of-range object IDs, SQL anti-patterns, dead code, missing ApplicationArea, DataClassification or tooltips, permission set coverage, complexity and duplicate code. Use before compiling, before deploying, when asked to clean up or review an AL app, or when a cop or analyzer warning needs fixing.
---

# Audit an AL extension

Every check here is workspace-scoped and small. Run them from the AL project
directory. None of them needs a compile.

## The pre-flight set

Run these four first. Together they take under a second and catch the failures
that would otherwise surface as a failed compile or a rejected deploy.

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json native-check
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json sql-scan
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json dead-code
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json arch-lint
```

`native-check` reports duplicate object IDs, IDs outside `app.json`'s
`idRanges`, and duplicate object names, as `AL-NC*` codes:

```json
[{"code":"AL-NC005","severity":"warning","objectType":"pageextension","objectId":50101,
  "objectName":"Sales Order Pageext","file":"src/DeeplyNestedActions.al",
  "message":"pageextension 'Sales Order Pageext' extends page 'Sales Order', which ..."}]
```

`sql-scan` names the pattern, object, procedure and line:

```json
[{"kind":"findSetWithoutFilters","message":"FindSet() without filters causes full table scan",
  "object":"Sales Order Pageext","procedure":"OnAction","file":"src/DeeplyNestedActions.al","line":79}]
```

`dead-code` covers unused procedures, unreferenced fields and orphaned
subscribers. Read `confidence` and `note`: a public procedure with zero
references may still be called from another extension.

These are this toolchain's own `AL-NC*` and `AL-NL*` rules. They do not replace
Microsoft's CodeCop, UICop, AppSourceCop or PerTenantExtensionCop, which need
the compiler.

## Annotation gaps

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json audit-data \
  | jq -c '[.[] | {table, field, risk}]'
```

```json
[{"table":"Test Customer","field":"No.","risk":"unclassified"}]
```

Three fixes apply mechanically. Each takes `--dry-run`, which is what to use
first:

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer add-data-classification --value CustomerContent --dry-run
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer add-application-area --value All --dry-run
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer add-tooltips --from-table --dry-run
```

`add-tooltips` copies the tooltip text from the base table's symbols, so page
captions match Microsoft's wording rather than being invented.

## Permission set coverage

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json permission-audit \
  | jq -c '[.coverage[] | select(.covered == false) | {kind, id, name}]'
```

```json
[{"kind":"codeunit","id":50101,"name":"Test Event Publisher"},{"kind":"codeunit","id":50103,"name":"Deep Nesting"}]
```

Generate a covering set with `al-explorer permissions --name "<App> Full"
--id <free id>`, taking the ID from the `bc-object-id-allocator` skill.

## Complexity and duplication

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json metrics --all \
  | jq -c '[.[].procedures[] | select(.cognitive > 15) | {name, cyclomatic, cognitive}]'
```

Keep the `select`. Unfiltered `metrics --all` is 18 KB on a small app and grows
with the project.

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json duplicates \
  | jq -c '[.[] | {a: .first.procedure, b: .second.procedure, lines: .lineCount}]'
```

## Diagnostics on one file

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json lint src/WorkOrderHelper.Codeunit.al
```

`lint` waits on the dependency source index. On a project with Base Application
loaded the first call takes about 20 seconds and can hit the 30-second client
timeout. Run `al-explorer --json packages` first and retry once.

## Formatting

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer format --all --check
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer sort-members --all --dry-run
```

`sort-members` puts `var`, triggers and procedures in canonical order.
`organize-files --dry-run` renames files to `<Type><Id>.<Name>.al`.

## Free the daemon when you are done

The daemon holds up to 2.9 GB resident once it has indexed a project with Base
Application, and nothing releases it:

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer daemon-shutdown
```

The next call after a shutdown can race the dying socket and fail with
`Connection reset by peer`. Retry once; it starts a fresh daemon.

## Do not

- Run a full compile to find duplicate IDs or unclassified fields. These checks
  take milliseconds.
- Apply `add-application-area`, `add-tooltips` or `add-data-classification`
  without `--dry-run` first.
- Present `native-check` results as CodeCop or AppSourceCop results.
