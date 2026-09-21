---
name: bc-base-app-source
description: Use whenever you need to read the AL source or body of a Business Central procedure, trigger or object - what a standard BC procedure does, how Microsoft implements something, show me the code for procedure P. Works for workspace code and for code that only exists inside a .app package such as Base Application, System Application or a third-party app. Use it instead of unzipping or decompiling a .app, and instead of grepping .al files for a procedure body.
---

# Read AL source, including from .app packages

The `.app` packages in `.alpackages` carry the original AL source for most
objects. `source` extracts it.

Run every command from the project directory you are already in. Do not `cd`
first: the daemon binds to the directory the command runs in, and the plugin
directory is not the project.

## Two steps, always

1. `search` for the exact object name.
2. `source` with `--procedure` for the one member you need.

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json search "Sales-Post"
```

```json
[{"kind":"Codeunit","id":80,"name":"Sales-Post","package":"Base Application","source_availability":"embedded_source"}]
```

`source_availability` says what you will get back:

- `workspace_source` and `embedded_source`: the real AL source.
- `generated_outline`: signatures only, because that package shipped without
  source.
- `metadata_only`: names and types only.

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json source "Sales-Post" --procedure RunWithCheck
```

```json
{"k":"Codeunit","id":80,"n":"Sales-Post","proc_name":"RunWithCheck","src":"package",
 "source_availability":"embedded_source","pkg":"Base Application",
 "sig":"internal procedure RunWithCheck(var SalesHeader2: Record \"Sales Header\")",
 "code":"internal procedure RunWithCheck(var SalesHeader2: Record \"Sales Header\")\r\n    var\r\n ..."}
```

4.7 KB. That is the answer for "what does BC do when it posts a sales
document", and it replaces decompiling a 45 MB `.app`.

`--trigger OnRun` reads a trigger instead. `--package "Base Application"`
disambiguates when two packages define the same object name.

## When you do not know the procedure name

A wrong name now names the ones that exist:

```json
{"error": "procedure 'PostSalesDoc' was not found in object 'Sales-Post'. It declares: PostSalesLines, PostAssocItemJnlLine, … (code -32602)"}
```

To see all of them with signatures and line ranges and no bodies:

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json source "Sales-Post" --list-procedures
```

```json
{"k":"Codeunit","id":80,"n":"Sales-Post","pkg":"Base Application","total":607,
 "members":[{"name":"RunWithCheck","kind":"procedure",
   "signature":"internal procedure RunWithCheck(var SalesHeader2: Record \"Sales Header\")",
   "startLine":118,"endLine":141}]}
```

Filter it when the object is large:

```bash
... al-explorer --json source "Sales-Post" --list-procedures \
  | jq -r '.members[] | select(.name | test("post.*sales"; "i")) | .signature'
```

## Never read a whole package object

`source "Sales-Post"` with no `--procedure` returns 837,509 bytes, the entire
codeunit. It will fill your context and it is almost never the question. Use
`--list-procedures` to pick, then `--procedure` to read. When you genuinely need
to scan the body, grep inside the pipe:

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json source "Sales-Post" \
  | jq -r '.code' | grep -n "SalesShptHeader.Insert" | head -5
```

## Workspace source

The same command reads the project's own objects, with `"src":"workspace"` and a
file and line range you can open with Read:

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json source "Work Order Helper" --procedure SchedulePost \
  | jq -r '.range.f, .sig, .code'
```

```
WorkOrderHelper.Codeunit.al
procedure SchedulePost(var Staging: Record "Work Order Staging")
procedure SchedulePost(var Staging: Record "Work Order Staging")
    begin
        Staging.Status := Staging.Status::Posting;
        Staging.Modify();
    end;
```

## Do not

- Unzip, extract or decompile a `.app` file.
- Read `source` without `--procedure` or `--list-procedures` on a package
  object.
- Ask for a procedure name you have not confirmed in `--list-procedures`.
