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
| `al.compilationOptions` | Appended verbatim to the `alc` command line. An entry that names a file alc loads from is refused (see below) |
| `al.ruleSetPath` outside the project | Reads a file from anywhere as `/ruleset:` |
| `al.assemblyProbingPaths` | Directories the analyzer search walks and `/assemblyprobingpaths:` names |
| `al.packageCachePath` outside the project | Chooses where `.app` symbol packages are read from, and reaches `/packagecachepath:` |
| `al.appLocalFolderPaths` outside the project | Same, for additional symbol folders |
| `al.nugetFeeds` | Every symbol package, including Base Application, comes from these URLs |
| `al.useOnlyCustomFeeds` | Removes Microsoft's feeds, leaving only the configured ones |
| `launch.json` / `debug.json` on-premises `server` | Receives the cached Business Central token and, with `acceptInvalidCerts`, decides whether TLS is verified |
| A `launch.json` or `debug.json` the parser rejects | Zed can still offer its scenarios to the debug adapter, so its servers are unknown to the record |
| `al.dotnetPath` | Names the `dotnet` host the toolchain spawns |
| `lsp.al-lsp.binary.path`, `.arguments`, `.env` | Name a program, its command line and its environment |
| `lsp.al-lsp.initialization_options` | Reaches the language server as configuration |

The four Zed `lsp.al-lsp` keys are in the digest but are not applied from here: the extension
ignores `binary.path` and `binary.arguments` outright (see Limits). They are in the digest so
a record made while `binary.path` said `/bin/sh` goes stale when the arguments change, and so
a future extension API that exposes `binary.env` does not widen an existing record silently.

The servers of both `.zed/debug.json` and `.vscode/launch.json` are in the record, because
Zed offers the scenarios of both. `environmentType` and `authentication` are read without
regard to case, as the debug adapter reads them. A launch file that still fails to parse is
recorded by its hash, so an existing record goes stale when the file changes, and
`al-explorer trust` refuses to record trust until the file is fixed: the servers it names
cannot be listed for review.

`${CodeCop}`, `${AppSourceCop}`, `${UICop}`, `${PerTenantExtensionCop}` and their bare
spellings are built-in tokens: the toolchain resolves them to Microsoft's own assemblies, so
they are not gated. Every caller passes the toolchain's file for them, never the name, so a
file of that name in the project is not loaded.

A repository can also point outside itself without a setting, by committing `.alpackages`
as a symbolic link. A symbol folder written inside the project that resolves outside it is
treated like `al.packageCachePath` outside the project: until the project is trusted its
packages are not read, `downloadSymbols` and the editor's symbol download refuse to write
into it, the editor shows why the symbols are missing in place of the download prompt, and the
daemon does not count it as a containment root. `al_project::trust::escapes_untrusted_project` is the one
check. The record lists each such folder as `linked package folder` with the directory it
resolves to. That covers `.alpackages` and every `al.packageCachePath` or
`al.appLocalFolderPaths` entry written inside the project, in the project's settings files or in
`~/.config/al-lsp/settings.json`. `trust
--show` shows where the folder leads, and a commit that adds the link or points it somewhere
else makes the record stale, which takes the target out of the containment roots again. A
link added after the grant used to leave the record trusted, and the daemon then accepted a
path anywhere under its target.

The folders the analyzer search walks in a trusted project are listed the same way:
`.netpackages`, `packages` and each `al.assemblyProbingPaths` entry written inside the project.
A commit that turns `.netpackages` into a link to a directory another user fills makes the
record stale, so the search skips the project's folders until the project is trusted again.
Before, the record did not change, and the search followed the link and loaded that user's
file.

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
covers exactly what was printed. `trust` and `trust --show` print each value whole with
control characters escaped and no length cap, so a link's target and its digest read to the
end, however long the path is.

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
in `plugin/` or `scripts/` runs this command, and nothing should. For the same reason the refusal
for a digest that no longer matches leaves out the current digest: it says nothing was recorded
and asks for `al-explorer trust --show` in a terminal, where a person reads the new values.

This is not a boundary against a program that already runs as the user: such a program can
write `trusted-projects.json` itself. What the design controls is that no surface an agent
reaches (the daemon, the MCP tools, a refusal, a skill) grants trust or tells it how to.

A revoke takes effect on the next request, in every process that applies these settings.
The daemon, and the MCP server through it, fingerprint the trust store, the user settings
file, both repository settings files, both launch files and the `dotnet` host they run
before each request, up to seven `stat` calls, and re-evaluate when any of them moved. They
used to decide once at startup and keep that configuration until they exited, which is up to
`AL_DAEMON_IDLE_SECS` after the last request and never while an editor keeps them busy.

The fingerprint also stamps the tree under each probing path the repository sets: it reads
the settings files for the paths and takes the length and modification time of each file
there, with the walk the record's hash uses and its 50,000 entry cap for all the probing paths
together. The daemon hands those
paths to `alc`, and a `git pull` that replaced or added a file there changed no settings
file, so the daemon kept passing the probing path to every build while `trust --show` said
`stale`. Now the next request decides again and drops the path.

The language server Zed runs takes the same fingerprint before every command (build, Run
Test, symbol download) and before semantic analysis resolves analyzers, and gates the
editor's settings again when it moved. Gating only removes values, so a project trusted
while the language server runs takes effect at the next settings change or restart. The
debug adapter is a new process for each session and decides at launch.

The fingerprint stamps the `dotnet` muxer and not the runtime beside it, which the record
hashes. So a `dotnet` the project supplies, inside it or through a link written inside it, is
decided again before each spawn: every alc build (the daemon's, the language server's, the
debug adapter's launch compile, publish and `al-explorer build`) and the `--official-lsp`
start. A `git pull` that replaces `host/fxr/<version>/libhostfxr.so` makes the project stale,
`AL_DOTNET_PATH` is dropped, and the build runs `dotnet` from `PATH`. Before, the process kept
the variable until the fingerprint moved, and the next build ran the new library. When a
settings file stops parsing there is nothing to decide with, so such a `dotnet` is dropped the
same way, with a message naming the file. A `dotnet` written outside the project costs one
path check.

The record lives in `~/.config/al-lsp/trusted-projects.json` (or `$XDG_CONFIG_HOME/al-lsp/`),
outside every repository, mode 0600, written through a temp file and a rename. Each entry
holds the canonical project root and a SHA-256 of the privileged values. Change one of those
values in the repository and the digest stops matching, so the settings are ignored again
until you run `trust` a second time. `al-explorer trust --show` reports that as `stale`.

A privileged value that is a path into the project names a file the repository ships, and the
file is what runs. For an analyzer path, `al.dotnetPath`, `binary.path` and each analyzer
name that resolves to a DLL under `.netpackages`, `packages`, a relative probing path or an
absolute one spelled inside the project, the path is first resolved through any symbolic
link, since the loader opens the target and reads its neighbours beside the target. The
recorded value carries the resolved file's SHA-256 and, when the path is a link, where it
resolves. For a probing directory inside the project, written as `tools` or `./tools`, it
carries one hash over every file below it. A probing path written without `./` used to be
recorded as text alone. The record also covers what the file loads from beside it: for an
analyzer, one hash over every file in its directory and below, since .NET resolves an
analyzer's references and its native libraries (`.so`, `.dylib`) from its own directory, and
for `al.dotnetPath`, one hash over every file beside the muxer and every file under its
`host` and `shared` directories, where it finds `hostfxr` and the framework. `trust --show`
prints the resolved paths and the hashes. A commit that replaces one of those files, adds one
where the record saw none, or points a link somewhere else makes the record `stale`. An
analyzer at the project root puts every file in the project into its record, so keep an
analyzer in a directory of its own.

An analyzer path or a probing path the project's settings files write outside the project is
hashed the same way. The repository chooses `../shared/TeamCop.dll` or `/tmp/cops` the same
way it chooses `./tools`, and any user of the machine can create `/tmp/cops`. `trust --show`
prints `../shared/TeamCop.dll (resolves to /home/you/src/shared/TeamCop.dll; sha256:...; its
directory: ...)`, or `/tmp/cops (not present)` when nothing is there, so a file that changes
or appears there after the grant makes the record `stale`. Before, both were recorded as text,
and a `TeamCop.dll` that appeared under `/tmp/cops` after the grant loaded under the old
record. A record made before this change lists such a path as text and goes `stale` once. An
outside tree is held to the same caps as one inside: `/usr` as a probing path holds symbolic
links and more than 50,000 entries, so the record refuses it rather than hash it. The same
paths in `~/.config/al-lsp/settings.json` are the user's and stay out of the record.
`al.dotnetPath` and `binary.path` outside the project are recorded as written.

A path written inside the project that a link carries outside it is resolved and hashed the
same way. With `"al.assemblyProbingPaths": ["./tools"]` and `tools` a link to a directory
beside the clone, `trust --show` prints `./tools (resolves to /home/you/src/shared-tools; ...)`
with the hash of every file there, and a change to those files or a commit that points the
link somewhere else makes the record `stale`. Before, the record held `./tools` as text, which
reads as a folder inside the project, and the files at the target could change under it.

`al.compilationOptions` is recorded as text, so it cannot vouch for a file an entry names.
An entry that names a file or directory alc loads from is refused: `/analyzer:` and its
short form `/a:`, `/assemblyprobingpaths:`, `/ruleset:` and `/packagecachepath:`, with `/`
or `-` and in any case, as alc reads them, and an `@` response file, which alc reads as more
switches. Such an entry is recorded as `compilation option that names a file`, so an existing
record goes stale, and `al-explorer trust` refuses to record the project and names the
dedicated key to use instead: `al.codeAnalyzers`, `al.assemblyProbingPaths`, `al.ruleSetPath`
or `al.packageCachePath`, which record what they name. Switches that name no input, such as
`/nowarn:` or `/target:`, are recorded as text as before.

A tree the record cannot hash is not recorded. The walk does not follow a symbolic link inside
the tree, because the loader does and a commit could change the target without changing any
file the walk reads. It also stops after 50,000 entries, and one hash reads at most 256 MiB, of
one file or of every file in one tree together. The same caps hold for every path one decision
hashes together: at most 50,000 entries walked and 256 MiB read in all. A file whose length is
over what is left is refused before it is opened, and a file that reports a smaller length, as
`/proc` files report none, stops once the count of bytes read passes it. A path whose tree
holds a link, more entries or more bytes than that is recorded as `path the record cannot
hash`, with the path and the reason, such as `/tmp/bc-tools holds more than 256 MiB` or `t7
and the paths hashed before it hold more than 256 MiB together`, so an existing record goes
stale, and `al-explorer trust` refuses to record the project until the link is replaced by the
file it names, the file moves to a directory of its own, or the settings name fewer paths.
Before, every byte was read, so a link to `/proc/self` or to a large sparse file kept each
trust decision reading for minutes, before the project was trusted. Later, each path had a
budget of its own, so a settings file that named a thousand directories, each just under it,
had a thousand budgets read by every decision.

The hash opens each file without waiting (`O_NONBLOCK` on Unix) and reads it only when it is a
regular file once open, so a file swapped for a FIFO or a device after the walk is recorded as
unreadable. Before, the open of a FIFO blocked until something wrote to it. One decision hashes
each file and each tree once, however many values name it. An analyzer search checks the copy
each entry found against the record by hashing it again, and it reads a file several entries
share once, within the same budget.

## Settings you wrote yourself

A value the user supplies is not gated. That covers `~/.config/al-lsp/settings.json`, an
`AL_*` environment variable and a CLI flag. It also covers Zed and VS Code user settings, with
one qualification: the editor merges its user settings with the worktree's before handing them
to the language server, so the server cannot see which file a value came from. It resolves
that by reading the repository's own settings files and removing exactly the values they
contribute. A privileged value written only in user settings survives; one the repository also
asks for is gated until the project is trusted.

An analyzer name you write yourself, such as `BusinessCentral.LinterCop`, is looked up in the
project's own folders (`.netpackages`, `packages`, a relative `al.assemblyProbingPaths` entry
or an absolute one spelled inside the project) only when the project is trusted. Otherwise it
resolves from the NuGet cache, an absolute probing path outside the project or the editor
extension folders, and a name found only inside the project is refused with a message saying
so. A relative analyzer path names a file the repository ships and is refused the same way.
The name was the user's, but the repository chose which file answered to it, and that file is
loaded into alc and into the language server's semantic bridge.

In a trusted project, a file inside the project that an analyzer entry resolves to, the copy
found for a name or the file a path names, loads only when the trust record lists that file
with the hash it has now. The record learns entries from the project's settings files and
`~/.config/al-lsp/settings.json`, and lists the file each of them resolves to. An entry
written only in Zed or VS Code user settings, or passed to `al-explorer build` as a flag, is
not in the record. So a copy of a name that a later commit adds under `.netpackages`, or a
file it adds at a path such as `./tools/TeamCop.dll`, is refused with a message naming the
file. Before, the copy was found ahead of the NuGet cache and the file at the path was loaded.
To use a file in the project, write the entry in the project's settings or in
`~/.config/al-lsp/settings.json`. A copy under an absolute probing path spelled inside the
project used to be left out of the record, so its name was refused in a trusted project.

The file an analyzer path the project's settings write outside the project names, and the copy
a name finds under a probing path they write outside it, are the project's too. The search
loads such a file only when the record lists it with the hash it has now. The language server
gates its settings again only when a settings file or the trust store changes, so after the
record went stale it still held the repository's entry, and the search loaded the changed
file. The search now refuses it the same way as a file inside the project.

The rule holds through a link in the project. A copy found under `.netpackages`, `packages`
or a probing path relative to or spelled inside the project, and the file a path such as
`./tools/TeamCop.dll` names, belong to the project wherever a link carries them. The record
hashes such a file where it resolves, and the search loads it only when the record lists it.
Before, such a file loaded at once when its resolved path was outside the project, and a path
through such a link loaded even in an untrusted project. The rule is in
`al_project::analyzers::CustomAnalyzerSearch`, which every build, publish, debug launch and
semantic analysis goes through.

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

- A Business Central online target needs no trust. The native adapter and the daemon build its
  URL on Microsoft's host, `api.businesscentral.dynamics.com`, with the tenant and environment
  URL-encoded. Microsoft's own deployment library, which the EditorServices proxy hands the
  scenario to, builds it as `https://{applicationFamily}.api.bc.dynamics.com/...` without
  checking `applicationFamily`, so `collector.example/` would send the token to
  `collector.example`. The proxy refuses an `applicationFamily` that is not one DNS label.
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

The EditorServices proxy forwards the scenario to Microsoft's host as Zed sent it, so it
judges the scenario the way that host might read it. It judges a scenario as on-premises, the
rule Microsoft's deployment library applies, when `environmentType` is `OnPrem`, when
`environmentType` is missing and a `server` is named (Microsoft's template for your own server
has none), or when `authentication` is `Windows` or `UserPassword` in any case. With either of
those authentication values the library connects to `server` even for `Sandbox` or
`Production`. The trust record lists the servers of launch entries under the same rule. The
proxy refuses a scenario with an `environmentType` other than `OnPrem`, `Sandbox` or
`Production` (in any case), an `authentication` other than `Windows`, `UserPassword`, `AAD` or
`MicrosoftEntraID` (in any case, written alone, since the library also reads `2`, ` Windows`
and `AAD,Windows` as one of them), an `applicationFamily` that is not one DNS label (letters,
digits and `-`), a target key such as `server`, `environmentType` or `applicationFamily`
spelled in another case, and a field it cannot read, such as a `port` written as a string.

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
