# Snapshot debugging

Snapshot debugging records what a Business Central session does at the lines you choose (snappoints)
while the session runs, then lets you read the call stack and variables afterwards. It needs no
stopped session and no paused user, so it suits a Production-like error that is hard to reproduce
while you watch. This is Business Central's snapshot debugger. The `test-snapshot` commands and the
`al_testsnapshot` tools are a separate feature: they record variables while one test method runs and
are described in [native-test-runtime](./native-test-runtime.md).

Code: `crates/al-dap/src/dap/bc_debug/snapshot_debugger.rs` (REST calls),
`snapshot_capture.rs` (the steps the daemon and Zed share), `native_dap/capture.rs` (capture in Zed),
`native_dap/replay/` (replay in Zed), `crates/al-lsp/src/server/daemon/snapshot_dispatch.rs` (daemon
method `snapshot`), and the fields in `debug_adapter_schemas/al.json`.

## Capture in Zed

Add an `attach` scenario with `"snapshot": "capture"` to `.zed/debug.json`:

```json
[
  {
    "adapter": "al",
    "label": "Snapshot the next web client session",
    "request": "attach",
    "environmentType": "Sandbox",
    "environmentName": "Sandbox",
    "tenant": "00000000-0000-0000-0000-000000000000",
    "snapshot": "capture",
    "breakOnNext": "WebClient",
    "userId": "user@contoso.com",
    "snapshotVerbosity": "SnapPoint",
    "snapshotOutputPath": ".snapshots"
  }
]
```

Set Zed breakpoints in AL files, then start the scenario. Each breakpoint becomes a snappoint and a
breakpoint condition becomes the snappoint condition. The capture arms when Zed sends
`configurationDone`, so the breakpoints are fixed from then on. Disconnect and attach again to change
them.

| Field | Meaning |
| --- | --- |
| `snapshot` | `capture` turns the attach into a recording. |
| `breakOnNext` | Required. The kind of session to record next: `WebClient`, `WebServiceClient`, `Background` or `ClientService`. `Agent` cannot be recorded. |
| `userId` | Record only this user's session. A user security ID (GUID) or a user name. Without it, the next session of any user on the tenant is recorded. A name is looked up through the Automation API, which works on SaaS only. |
| `sessionId` | Record the server session with this id instead of the next one. |
| `snapshotVerbosity` | `SnapPoint` (default) records variables at the snappoints and where an exception is held. `Full` also records every call. |
| `snapshotOutputPath` | Folder for the saved zip inside the project. Default `.snapshots`. A relative path is relative to the project root. |
| `snapshotPort` | On-premises only. The port of the snapshot debugger services, default 7083. |
| `allowProduction` | Allow a Production environment. Without it a Production target is refused before any sign-in. |

Snappoints are optional. With none, the default `SnapPoint` verbosity still records a method context
that holds an exception.

### What you see

The capture only records, so Zed shows no stack, no variables and no stepping. Progress arrives as
Output events:

- A line with the debugging context (a GUID) and the attach kind when the capture is armed.
- `Snapshot status: <status>` each time the server status changes (for example `Started`).
- A warning when the session has been `Started` for 8 minutes. The server drops a started session
  after 10 minutes, so finish it before then.
- A warning when the BaseApp version of the environment differs from the version the snappoints were
  set against (see below).

Disconnect to finish. The adapter downloads the snapshot, saves the zip under `snapshotOutputPath`
and prints `Snapshot saved to <path>`. If the finish fails, the session record stays on this machine and `al-explorer snapshot finish` retries it. When the adapter creates the output folder it also writes a `.gitignore` with
`*` into it. An existing folder is left alone. `al-explorer snapshot finish` does the same from a
terminal.

## Replay in Zed

Replay is a `launch` with `snapshotFileName`:

```json
[
  {
    "adapter": "al",
    "label": "Replay a snapshot",
    "request": "launch",
    "snapshotFileName": ".snapshots/3033906a-fe68-4bee-85e3-bbb2ebc91e1b.zip"
  }
]
```

A relative path is relative to the project root. Nothing is compiled or published and no token is
read, so replay works offline. The adapter reads the zip and walks the recorded statements with the
stepping rules of Microsoft's debug adapter. It supports continue, next, stepIn, stepOut and
breakpoints (continue runs to the next snappoint or breakpoint). Variables exist only at snappoints
and at a context that holds an error. An error stop has reason `exception`, the message as text and a
Locals entry named `_ALException`. That error stop is built from the decompiled host's error node and
is untested against a real recording with an exception.

Each frame opens the first source that exists, in the order Microsoft's host uses: the workspace
file when the workspace declares the object, then the source stored in the zip (written read-only to
the `snapshot-sources` cache folder), then the source of a dependency package in `.alpackages`, the
same file that go to definition opens. A generated outline is never used. An object with none of
these gets a frame without source and one `output` line. When the workspace file differs from the
zip's source of the same object, one `output` warning says line numbers can be off. A file that
cannot be made read-only is reported in one `output` line. Differences from Microsoft's adapter: the
`stopped` reason is the DAP value `step`, `initialized` is sent by `initialize`, no extra `stopped`
follows `terminated`, and `output` text carries no timestamps.

On the live Sandbox test, the replay of a web client capture matched Microsoft's host in continue,
next, stepIn and breakpoint modes.

With `--dap-legacy` (the setting `al.useOfficialDap`) a `launch` with `snapshotFileName` goes to
Microsoft's `EditorServices.Host`, which replays it. The proxy does not compile, publish or edit that
request.

## Snappoints in Base Application code

A snappoint is an object type, an object id, a line and an optional condition. In Zed, set the
breakpoint in a file and the adapter finds the object around the line. For code you do not own, open
the object with go to definition. Zed shows the package source from the virtual source cache, and
breakpoints in that file become snappoints in the real object. A symbol outline (built from symbols
when the package ships no source) has no real line numbers, so a breakpoint in it is refused.

From the CLI or MCP, give the object form `<type>:<id or name>:<line>[?condition]`, for example
`page:42:2530`, or a file form `<file>:<line>[?condition]`. Types are table, report, codeunit,
xmlport, page, query, pageextension, tableextension and reportextension. Lines are 1-based.

Line numbers must match the code the server runs. The adapter compares the BaseApp version of the
environment (read through the Automation API) with the package the snappoint came from and with the
BaseApp in `.alpackages`, and warns when they differ.

## Command line

All commands run through the daemon and use a launch configuration from `.zed/debug.json` or
`.vscode/launch.json` for the server, tenant and sign-in. `--config` names it, the first configuration is used when it is left out.

```sh
# start: snappoints on page 42 for the next web client session of one user
al-explorer snapshot start --config "Sandbox" --break-on-next WebClient \
  --user user@contoso.com --snap page:42:2530 --snap page:42:2531

# status: the latest session, or --context <guid>
al-explorer snapshot status

# finish: download and save the zip, default folder .snapshots
al-explorer snapshot finish --out .snapshots

# list: sessions started from this machine
al-explorer snapshot list

# show: stops of a zip with stack and variables
al-explorer snapshot show .snapshots/3033906a-fe68-4bee-85e3-bbb2ebc91e1b.zip --stop 1 --fields "No.,Description"
```

`start` requires `--break-on-next` and takes `--config` (default: the first configuration), `--snap` (repeatable, optional),
`--user`, `--session-id`, `--verbosity snappoint|full` (default `snappoint`) and `--allow-production`.
`status` and `finish` take `--context`, `finish` also `--out`. `show` takes `--vars`, `--fields`,
`--stop` and `--json`. The session record is kept on this machine, so `status` and `finish` find the
latest session without arguments.

## MCP

The `al_snapshot` tool takes `action` (`start`, `status`, `finish`, `list`, `show`) and the same
parameters under camelCase names: `config`, `snaps`, `breakOnNext`, `userId`, `sessionId`,
`verbosity`, `allowProduction`, `accessToken`, `context`, `outputDir`, `path`, `vars`, `fields`, `stop`
and `json`. `breakOnNext` is required for `start`. See [mcp-tools](../reference/mcp-tools.md).

## Limits

- The server drops a debugging context that no session attached to within 30 minutes, and a started
  session 10 minutes after it started. The error "the server no longer knows this debugging context"
  means one of these happened or the session was finished elsewhere.
- `WebServiceClient` capture did not work on the BC 29.0 Sandbox used for testing. The server dropped
  the debugging context before the first status call in every attempt. The likely cause is that the
  attach and status calls, made with the same user's token, count as that user's next web service
  session. This is not proven. `WebClient` capture works.
- A per-tenant extension is recorded only when its `app.json` has
  `"resourceExposurePolicy": { "allowDebugging": true }`. Microsoft's default is false, and without
  it the extension's code is not recorded.
- Production is refused unless `allowProduction` is set, before any sign-in.
- Microsoft's platform snapshot MCP proxy needs the environment's MCP Server Configuration. On the
  test Sandbox its upstream `tools/list` answered error -32603. This project does not use that proxy.
- A snapshot zip holds customer data. Keep it out of git. The adapter writes a `.gitignore` into a
  folder it creates. Replay extracts the zip's sources to the system temp folder, not the project.
- Capture is authorised like any other attach, so a refused target fails in the debug console. A
  replay launch reads a local zip, sends no credential and skips that check.
