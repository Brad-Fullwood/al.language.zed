# Daemon Method Reference

JSON-RPC methods handled by `server/daemon/dispatch_request`. The daemon transport, CLI, and
checkout-local contributor tasks route here; MCP calls the same dispatcher in-process. Every method
below is available through MCP's `al_call`, whether or not it also has a named MCP alias. Transport and lifecycle:
[daemon-protocol](../features/daemon-protocol.md).

## LSP-style (`lsp_dispatch.rs`)

`hover`, `definition`, `references`, `implementations`, `completions`, `signatureHelp`, `rename`,
`documentSymbols`, `foldingRanges`, `semanticTokens`, `inlayHints`, `codeActions`, `search`, `object`,
`byId`, `events`, `subscribers`, `composed`, `packages`, `deps`.

## Build, codegen, fixes, symbols/auth (`build_dispatch/`)

`lint`, `format`, `fix`, `fix.applicationArea`, `fix.tooltips`, `fix.dataClassification`, `rules`,
`parse`, `metrics`, `sqlPatterns`, `sortMembers`, `organizeFiles`, `source`, `eventSource`,
`location`, `permissions`, `compile`, `package`, `publish`, `newProject`, `errorCodes`, `builtinTypes`, `setup`,
`clearCache`, `authenticate`, `downloadSymbols`, `snapshot`, `profiling`, `generate`, `obsolete`,
`obsoleteUsages`, `packageDiff`,
`audit.dataClassification`, `permissions.audit`, `deps.graph`, `breaking`, `arch.lint`, `duplicates`,
`upgrade`, `profiler.hints`, `nativeCheck`, `freeIds`, `diag`.

XLIFF: `xlf.generate`, `xlf.refresh`, `xlf.untranslated`, `xlf.suggest`.

`publish` compiles the project and uploads it to the Business Central dev endpoint named by a
launch configuration in the project. Params: `config` (launch configuration name, optional),
`incremental` (boolean, default false). The target server comes only from the project's own
launch configuration.

`obsoleteUsages` lists the calls in the workspace to procedures that are obsolete, each with
`file`, `range` and a `message` naming the reason and tag. A name with any active definition is
left out rather than guessed at. `obsolete` lists every pending obsoletion in the loaded packages
instead.

`packageDiff` compares two versions of a dependency and keeps the changes the workspace uses.
Params: `from` and `to` (paths to the two `.app` files, inside the project or its package
folders; relative paths resolve against the project root), `all` (boolean, default false: also
return the changes nothing in the workspace uses). The result names both packages and carries
`totalChanges`, `breakingChanges`, `affectingWorkspace`, `possiblyAffecting` and `changes`, where
each change has the `kind`, `object`, `member`, `description` and `isBreaking` of `breaking` plus
`uses`, the workspace consumers whose receiver resolves to the changed object in `impact`'s row
shape, and `possibleUses`, name matches whose receiver did not resolve. A change to a member
counts only code that uses the member; extending the object is not a use of each of its members.

`freeIds` allocates inside the `idRanges` declared in `app.json`. Params: `kind` (object kind
keyword, omit for a per-kind summary), `object` (a table, tableextension, enum or enumextension
whose next free field number or enum ordinal is wanted; wins over `kind`, which then disambiguates
the name), `count` (1 to 100, default 1) and `includeUsed` (default false). Used numbers come from
every object declared in the workspace, including the second and later objects in a multi-object
file, plus the package objects that sit inside a declared range. A tableextension's fields must fall
inside `idRanges` and avoid the base table and every other extension of it that is visible; an
enumextension's ordinals work the same way. An exhausted range is an `INVALID_PARAMS` error naming
the range, and an `app.json` without `idRanges` returns a `warnings` entry.

`compile` is native by default. A native response includes `backend: "native"`, `validated: true`,
`verificationLevel: "native-syntax-project-binding-symbol-graph"`, `appPath` (or `null` on
rejection), and diagnostics with 1-based `line`/`column` plus exact native `endLine`/`endColumn`.
Workspace semantic/call-graph errors gate emission; warnings are returned with a successful build.
Set `al.useOfficialCompiler: true` to select the explicit Microsoft `alc` backend; there is no
silent fallback from native to Microsoft tooling.

Tests: `tests.discover`, `tests.run`, `tests.coverage`, `tests.run_batch`, `tests.run_auto`,
`tests.last_results`, `tests.affected`, `tests.classify`, `tests.snapshot_validate`,
`tests.snapshot_capture`, `tests.snapshot_replay`, `tests.snapshot_diff`, `tests.mutate`.

## Insight (`insight_dispatch.rs`)

`trace`, `traceChain`, `entrypoints`, `graphExport`, `insightStats`, `deadCode`, `impact`,
`tableImpact`, `suggestEvent`, `eventMap`.

## Debug (`debug_dispatch.rs`)

`debug` with `params.cmd` ∈ { `start`, `breakpoint`, `state`, `stack`, `variables`, `globals`,
`expand`, `eval`, `continue`, `step`, `history`, `stop` }. Inspection/evaluation commands accept
`frameId`; `expand` also requires `path`; `step` accepts `stepType: over|in|out`. This stateful method
backs both the CLI debug commands and the MCP `al_debug` tool; the process must remain alive between
calls.

## Projection: `limit`, `offset`, `fields`

Every list-returning method accepts `limit` (rows to return), `offset` (rows to skip) and `fields`
(an array of key names to keep on each row). They are applied once at the dispatch boundary
(`daemon/projection.rs`), so the behaviour is the same for all of them.

A method whose result is the array answers `{items, total, returned, offset, truncated}` when any
of the three is supplied, and the bare array when none is. A method whose result is an object
around one array keeps its other fields and gains `total`, `returned`, `offset` and `truncated`
beside them. `total` counts the rows before the window, and `truncated` is true when rows follow
the page, so a full page is never mistaken for a complete answer.

Root-array methods: `search`, `object`, `byId`, `events`, `subscribers`, `entrypoints`, `deadCode`,
`nativeCheck`, `trace`, `packages`, `sqlPatterns`, `obsolete`, `obsoleteUsages`, `rules`, `errorCodes`,
`builtinTypes`, `duplicates`, `arch.lint`, `audit.dataClassification`, `tests.discover`,
`profiler.hints`, `breaking`, `upgrade`.

Object-with-array methods, with the field projected: `impact` (`impacted`), `tableImpact`
(`objects`), `eventMap` (`events`), `suggestEvent` (`integrationPoints`), `traceChain` (`chains`),
`composed` (`extensions`), `tests.affected` (`affected`), `permissions.audit` (`coverage`),
`packageDiff` (`changes`).

MCP callers get `limit: 50` when they do not pass one, because a tool result goes straight into a
context window. An explicit `limit` always wins, including `limit: 0` for a count.

## Scope: `workspace`, `packages`, `all`

`impact`, `tableImpact`, `entrypoints` and `eventMap` accept `scope`. `workspace` keeps the rows
from the open project, `packages` keeps the rows from loaded `.app` files, and `all` keeps
everything. The result reports the `scope` it used and `outOfScopeCount`, so a short answer is not
read as a small workspace.

The filter is applied before `limit`, so a page counts rows that survived the scope. A daemon
caller that passes no `scope` gets every row, which is what these methods did before. MCP callers
default to `workspace`, because that is the code the project can change.

## Conventions & limits

- Lifecycle methods: `ping` returns an empty object, `status` returns daemon/workspace state,
  `shutdown` requests an orderly daemon stop, and `handshake` returns `{version, build, pid}` — the
  build this daemon was started from. A client compares `version` and `build` with its own before
  it uses a daemon it did not start, and replaces one that does not match, because a daemon from
  other code answers with that code's response shapes.
- `status` reports `memory` and `diag/summary` reports `process`, both
  `{residentBytes, peakResidentBytes}`. `residentBytes` is null off Linux. These are what the
  operating system sees, unlike the per-structure byte totals in `diag`, which count only
  allocations the workspace owns.
- `status` and `diag` report `sourceIndex` as `{state, packagesDone, packagesTotal, filesDone,
  elapsedMs}`, where `state` is `idle`, `building`, `ready` or `failed`. The dependency AL source
  index takes about a minute on Base Application, and `subscribers`, `composed`, `events`, `lint`,
  `trace`, `impact` and `entrypoints` all wait for it. The daemon and the MCP server start it in
  the background at startup, and the build is single-flight, so concurrent and retried callers join
  one build rather than starting their own.
- `status` reports `launchConfigError` when the project's debug configuration file could not be
  read. Symbol queries are unaffected by that; the Business Central connection commands are the
  ones that need the file.
- JSON-RPC 2.0 over newline-delimited frames; error codes include standard set plus `-32000`
  (code analysis), `-32001` (file not found) and `-32002` (the daemon will not touch this path).
  `null` results are serialized explicitly.
- `duplicates` clamps `minTokens` and `minSimilarity`; `graphExport` is capped at
  50k nodes+edges; `trace`/`traceChain` depth bounded; 64 MB max request line; ≤64 concurrent
  connections; 30-minute idle shutdown, skipped while a request is in flight or a debug session is
  open, and settable with `--idle-timeout-secs` or `AL_DAEMON_IDLE_SECS` (`0` never exits). A daemon
  also stops once its project directory no longer exists, whatever the idle window.
- Local-only IPC: Unix-domain socket at `$XDG_RUNTIME_DIR/al-lsp/{hash}.sock` (with platform
  runtime-directory fallbacks) on Linux/macOS; per-user named pipe on Windows.

## Paths and the project boundary

A `uri` or `file` parameter is resolved inside the loaded project: its root, the package cache, and
the directory each resolved `.app` came from. Anything else is refused with `-32002`, whose message
names the path and the project root. The same dispatchers answer MCP's `al_call`, where the caller
may be an agent and the path may be anything it asks for, so the boundary holds for every caller.

A read-only single-file method (`parse`, `lint`, `metrics`, `hover`, `definition`, `references`,
`implementations`, `completions`, `signatureHelp`, `rename`, `documentSymbols`, `foldingRanges`,
`semanticTokens`, `inlayHints`, `codeActions`) also accepts `text` beside the path. The daemon then
analyses that text and never opens the path, and the document it holds for the request is dropped
when the request is answered. `rename` belongs here because it returns a `WorkspaceEdit` for the
client to apply and writes nothing itself. `text` is refused for a path inside the project, where
the daemon's own copy is authoritative, and refused outright by any method that rewrites the file it
names (`format`, `fix*`, `sortMembers`, `organizeFiles`): supplied content is analysed, never
written back.

`al-explorer` uses that: on `-32002` from a read-only method it reads the file itself and asks
again with `text`, so `al-explorer parse ../elsewhere/Foo.al` works while the daemon still opens
nothing outside the project. A write command reports the refusal instead.

Common parameter shapes: position queries accept `uri` plus `{line, character}`; `breaking` and
`upgrade` accept `baselineSymbols`; `tests.snapshot_validate` accepts `snapshotPath`;
`tests.snapshot_replay` accepts `snapshotPath`, `bcVersion`, and optional `config`/`timeoutMs`; and
`tests.snapshot_diff` accepts `pathA` and `pathB`. Snapshot paths must resolve inside the current
project. `source` requires `name` and accepts the
disambiguators `kind`, `package`, `proc`, or `trigger` (`proc` and `trigger` are mutually exclusive);
it returns `source_availability` as `workspace_source`, `embedded_source`, `generated_outline`, or
`metadata_only`. `source` also accepts `listProcedures` (boolean), which returns
`{k, id, n, pkg, source_availability, members, total}` where each member carries `name`, `kind`,
`signature`, `startLine` and `endLine` and no body. A `proc` or `trigger` that does not exist is an
`INVALID_PARAMS` error naming the members the object does declare. A whole-object lookup of a
workspace object returns `range` with the declaring file's path and line span. `location` accepts
the same object identity selectors (`name`, `kind`, `package`, and `id`) and rejects ambiguous
matches. Other method shapes are defined beside their dispatcher and
mirrored by `al-explorer`; MCP passes the same object through `al_call`.

`deps.graph` rereads and validates the current `app.json`, reads dependency metadata from every
configured `.app` package, and fails explicitly on an unreadable/malformed manifest. Identity is the
app GUID; minimum versions are treated as compatible when a loaded version satisfies them.
