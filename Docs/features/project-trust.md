# Project trust

`.vscode/settings.json`, `.zed/settings.json` and `.vscode/launch.json` are files a
repository carries. Cloning a repository means accepting whatever they say, and most of
what they say is harmless: formatting width, diagnostics scope, which lint rules run.

A few keys are not harmless. They name a .NET assembly the AL compiler loads into its own
process, raw switches appended to the `alc` command line, directories the compiler probes
for assemblies, the package feed every symbol is downloaded from, and the Business Central
server a cached bearer token is sent to. Believing those on the strength of a `git clone`
means a repository chooses what runs on the machine that opened it.

Values in that second group take effect only when the project root is trusted. Everything
else applies as it always has.

## What is gated

| Setting | Why it is privileged |
| --- | --- |
| `al.codeAnalyzers` entries that are not built-in tokens | A path entry becomes `/analyzer:<path>`; Roslyn loads the assembly and runs its type initialisers |
| `al.compilationOptions` | Appended verbatim to the `alc` command line, including `/analyzer:` and `/ruleset:` |
| `al.ruleSetPath` outside the project | Reads a file from anywhere as `/ruleset:` |
| `al.assemblyProbingPaths` | Directories the analyzer search walks and `/assemblyprobingpaths:` names |
| `al.packageCachePath` outside the project | Chooses where `.app` symbol packages are read from, and reaches `/packagecachepath:` |
| `al.appLocalFolderPaths` outside the project | Same, for additional symbol folders |
| `al.nugetFeeds` | Every symbol package, including Base Application, comes from these URLs |
| `al.useOnlyCustomFeeds` | Removes Microsoft's feeds, leaving only the configured ones |
| `launch.json` / `debug.json` on-premises `server` | Receives the cached Business Central token and, with `acceptInvalidCerts`, decides whether TLS is verified |
| `al.dotnetPath` | Names the `dotnet` host the toolchain spawns |
| `lsp.al-lsp.binary.path`, `.arguments`, `.env` | Name a program, its command line and its environment |
| `lsp.al-lsp.initialization_options` | Reaches the language server as configuration |

The four Zed `lsp.al-lsp` keys are in the digest but are not applied from here: the extension
ignores `binary.path` and `binary.arguments` outright (see Limits). They are in the digest so
a record made while `binary.path` said `/bin/sh` goes stale when the arguments change, and so
a future extension API that exposes `binary.env` does not widen an existing record silently.

`${CodeCop}`, `${AppSourceCop}`, `${UICop}`, `${PerTenantExtensionCop}` and their bare
spellings are built-in tokens: the toolchain resolves them to Microsoft's own assemblies, so
they are not gated.

A repository can also point outside itself without a setting, by committing `.alpackages`
as a symbolic link. A symbol folder written inside the project that resolves outside it is
treated like `al.packageCachePath` outside the project: until the project is trusted its
packages are not read, `downloadSymbols` refuses to write into it, and the daemon does not
count it as a containment root. `al_project::trust::escapes_untrusted_project` is the one
check.

Everything else in a repository's settings applies without trust: formatting, inlay hints,
`al.diagnosticsScope`, `al.enableNativeLint` and its per-rule overrides, `al.incrementalBuild`,
`al.useOfficialCompiler`, `al.maxDocumentSizeBytes`, a ruleset or package folder inside the
project.

## What untrusted looks like

The privileged values are dropped and everything else is applied. You get one message naming
the keys that were dropped:

```
This project is not trusted, so these settings from its own files were ignored:
  al.codeAnalyzers
  al.compilationOptions
They can load code, run programs or receive credentials. Their values are not repeated
here. To read them and decide, the user runs this in a terminal:
al-explorer trust --show /home/you/src/SomeApp
```

In Zed it arrives as a warning notification, in `al-explorer` on stderr, in the daemon and
the MCP server in the log, and the MCP server also puts it in the `instructions` it hands
the agent so the agent can pass it on.

The message carries key names and nothing else. A settings value is text the repository
wrote, and a JSON string holds newlines, so a value spelled as an instruction paragraph
would arrive in the MCP `instructions` field, which a client presents as the server's own
guidance. Key names come from a fixed list in `al_project::trust::ADVISORY_KEYS`; a key the
list does not hold is a launch configuration, whose name the repository also chose, and it
prints as `launch configuration server`. The values are read with `al-explorer trust
--show`, in a terminal.

The same rule covers every other message that quotes repository text. A refusal that names
a Business Central server, a dotnet host or a settings parse error puts it through
`al_project::trust::one_line`, which escapes control characters and caps the length, so no
repository byte can start a line of its own.

## Trusting a project

```bash
al-explorer trust --show          # what needs trust, and the current state
al-explorer trust                 # print the values, ask, then record them
al-explorer trust --revoke        # remove the record
```

`trust` prints every privileged value, then asks. Type `yes` to record them. The record
covers exactly what was printed.

The question is asked on the terminal device (`/dev/tty`, `CONIN$` on Windows), not on
stdin, and a call whose stdin is not a terminal is refused outright. stdin can be a pipe
while the process still has a controlling terminal, and a pipe is what a task, a hook, a
`build.rs` or an agent's Bash tool hands over. Being a command rather than a daemon method
was not enough on its own: the command used to write the record with stdin closed and no
terminal, so anything running as the user granted trust in one call.

A CI job has no terminal, so it answers with flags: `--yes --root <project> --digest
<sha256>`. The digest is the one `al-explorer trust --show` printed when a person read the
values, and it goes in the CI configuration. `--yes` without `--root` or `--digest` is
refused, `--root` naming a different path is refused, and a digest the current values no
longer match is refused, so a commit that changes a privileged value fails the job rather
than being trusted by it. `--yes --root` used to record whatever the repository held at that
moment.

The refusal a call with no terminal receives does not name these flags. That call is by
construction a script or an agent, and the refusal said how to make the same call succeed.
The flags are a step a person puts into a CI configuration, not an answer to a refusal. Nothing
in `plugin/` or `scripts/` runs this command, and nothing should.

This is not a boundary against a program that already runs as the user: such a program can
write `trusted-projects.json` itself. What the design controls is that no surface an agent
reaches (the daemon, the MCP tools, a refusal, a skill) grants trust or tells it how to.

A revoke takes effect on the next request. The daemon fingerprints the trust store, the
user settings file, both repository settings files, the launch file and the `dotnet` host it
runs before each request, six `stat` calls, and re-evaluates when any of them moved. It used to decide once at
startup and keep that configuration until it exited, which is up to `AL_DAEMON_IDLE_SECS`
after the last request and never while an editor keeps it busy.

The record lives in `~/.config/al-lsp/trusted-projects.json` (or `$XDG_CONFIG_HOME/al-lsp/`),
outside every repository, mode 0600, written through a temp file and a rename. Each entry
holds the canonical project root and a SHA-256 of the privileged values. Change one of those
values in the repository and the digest stops matching, so the settings are ignored again
until you run `trust` a second time. `al-explorer trust --show` reports that as `stale`.

A privileged value that is a path into the project names a file the repository ships, and
the file is what runs. For an analyzer path, `al.dotnetPath`, `binary.path` and each
analyzer name that resolves to a DLL under `.netpackages`, `packages` or a relative probing
path, the recorded value carries the file's SHA-256, and for a probing directory inside the
project one hash over every `.dll` below it. `trust --show` prints those hashes. A commit
that replaces one of those files, or adds one where the record saw none, makes the record
`stale`. A path outside the project is the user's machine and is recorded as written.

## Settings you wrote yourself

A value the user supplies is not gated. That covers `~/.config/al-lsp/settings.json`, an
`AL_*` environment variable and a CLI flag. It also covers Zed and VS Code user settings, with
one qualification: the editor merges its user settings with the worktree's before handing them
to the language server, so the server cannot see which file a value came from. It resolves
that by reading the repository's own settings files and removing exactly the values they
contribute. A privileged value written only in user settings survives; one the repository also
asks for is gated until the project is trusted.

An analyzer name you write yourself, such as `BusinessCentral.LinterCop`, is looked up in the
project's own folders (`.netpackages`, `packages`, a relative `al.assemblyProbingPaths`
entry) only when the project is trusted. Otherwise it resolves from the NuGet cache, an
absolute probing path or the editor extension folders, and a name found only inside the
project is refused with a message saying so. A relative analyzer path names a file the
repository ships and is refused the same way. The name was the user's, but the repository
chose which file answered to it, and that file is loaded into alc and into the language
server's semantic bridge. The rule sits in `al_project::analyzers::discover_custom_analyzer`,
which every build, publish, debug launch and semantic analysis goes through.

A credential you supply yourself is the same: `BC_USERNAME`, `BC_PASSWORD` and
`BC_ACCESS_TOKEN` apply without trust. What still needs trust is the *server* those
credentials are sent to when the repository's launch file chose it. See
[Credentials](#credentials).

## How agents are treated

The MCP server and the daemon cannot grant trust, and no request through them can supply a
privileged value inline:

- `al_call` reaches the whole daemon method table, and no method writes
  `trusted-projects.json`.
- `al_debug start` with an inline configuration cannot name an on-premises server of its own:
  a cached token goes only to a server the project's launch file names, and only when the
  project is trusted. A caller that wants an unlisted server supplies its own `accessToken`,
  which is its credential to spend.
- Trust is granted by `al-explorer trust`, which asks the terminal device and refuses a call
  whose stdin is not a terminal. An agent must not run it, and the messages that mention it
  say so: they name `al-explorer trust --show` as something the user runs, rather than
  ending with a command to paste.

An agent reads the repository. An AL comment, a symbol name in a dependency `.app` or a BC
response can tell an agent what to call next. If asking an agent to "build this to confirm it
compiles" were enough to load a repository's analyzer assembly, a repository could run code by
writing a sentence.

## Credentials

Ten daemon methods reach a credential the daemon holds, send the user's own to a server the
repository's launch file names, or send one to a server the request names, and all ten go
through one authorisation function: `debug` (the `start` command), `publish`,
`downloadSymbols` from a BC server, `tests.run`, `tests.run_batch`, `tests.run_auto`,
`tests.snapshot_capture`, `tests.snapshot_replay`, `snapshot` and `profiling`. The daemon's
dispatch table declares which methods those are, and a test holds this list and that
declaration together.

- Microsoft's Business Central online endpoints are always allowed. The endpoint is fixed, so
  a repository cannot redirect the token.
- An on-premises target is compared on scheme, host and port together. A launch configuration
  naming `https://erp.example.com` does not authorise `http://erp.example.com`.
- `http://` is refused for bearer and basic credentials unless the host is loopback. Set
  `AL_ALLOW_INSECURE_BC_HTTP=1` to allow a cleartext server elsewhere on a network you trust.
  An environment variable is a user-level decision, so it needs no project trust.
- A server written without a scheme (`bc.corp.example`, `bc.corp.example:7049`) is
  `https`. The authorisation and every request builder read it through one function,
  `al_bc::launch::server_with_scheme`, so the scheme that was judged is the scheme the
  request uses. To reach a cleartext server, write `http://` in the launch configuration,
  and set `AL_ALLOW_INSECURE_BC_HTTP=1` when the host is not loopback. A bare host used to be
  sent as `http://` while the check read it as `https`, so the cleartext rule passed a
  request that then sent Basic credentials in the clear.
- `acceptInvalidCerts` from the project's own debug configuration is honoured only for the
  same target and only when the project is trusted.

The Zed debug adapter (`al-lsp --dap`) and the EditorServices proxy (`al-lsp --dap-legacy`)
run the same authorisation on every `launch` and `attach`, before anything is compiled or
sent. Zed reads the debug scenario from the worktree's `.zed/debug.json` or from the user's
own debug settings, and the adapter cannot tell which, so the scenario is judged as a file
the repository carries: an on-premises server needs a trusted project, and
`acceptInvalidCerts` is honoured only when the project's launch file sets it for the same
server. A refused launch fails with the reason in the debug console.

`publish` and `tests.run*` are in that list although they never read the OAuth cache: they
send `BC_ACCESS_TOKEN`, or `BC_USERNAME` and `BC_PASSWORD`, from the environment. The
environment is the user's own decision, but which server receives it is the repository's, so
the target is authorised and the refusal says "Business Central credentials" rather than
naming a cached token. The Run Test code lens in the language server runs the same check
before it starts a live test.

`snapshot` and `profiling` take `serverUrl`, `username` and `password` from the request.
The credential is the caller's, but the server is a string an agent can choose through
`al_call`, so an inline `serverUrl` must match a launch configuration of a trusted project,
compared on scheme, host and the port the client connects to (the URL's own, or 443 or 80 by
scheme). Their `acceptInvalidCerts` is refused unless that launch configuration sets it for
the same server. A request with no `serverUrl` gets the daemon's loopback default, which no
caller chose, and `acceptInvalidCerts` is dropped for it.

### Where the caller brings its own credential

`debug start` with an explicit `accessToken` spends that token rather than the cached one,
so there is no cached credential to protect. `acceptInvalidCerts` is still refused unless the
project's own configuration asks for it and the project is trusted: turning off TLS
verification is about the target, not the token.

## Limits

The Zed extension reads `binary.path`, `binary.arguments` and `dotnetPath` from
`LspSettings::for_worktree`, and the 0.7 extension API gives it one merged value with no
provenance. The extension also cannot read this trust store: it runs in Zed's WASM sandbox,
with no filesystem and no process.

`binary.path` and `binary.arguments` name a program and its command line, and `binary.path`
decides whether al-lsp runs at all, so al-lsp cannot refuse them on the extension's behalf.
The extension ignores both, and the debug adapter path with them. Which `al-lsp` runs is the
extension's own decision: the session cache, then `al-lsp` on `PATH`, then the cached or
downloaded release. Put a specific build on `PATH` to use it.

`binary.arguments`, `binary.env` and `lsp.al-lsp.initialization_options` are in the digest
even so. A project trusted while `binary.path` said `/bin/sh` went stale only when the path
changed, never when the arguments did, and the arguments are the setting.

See [current limitations](../current-limitations.md#zed-worktree-settings-and-executable-paths)
for `dotnetPath`, which al-lsp does gate on its own side.
