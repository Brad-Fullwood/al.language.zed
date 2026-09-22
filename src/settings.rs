use serde_json::json;

/// Maximum number of dotted segments in a settings key. Beyond this, the
/// remaining path is collapsed into a single literal key rather than
/// recursing further. Defends against stack-overflow from pathologically
/// deep user-controlled keys (parity with `merge_json`'s
/// `MERGE_JSON_MAX_DEPTH` in `lib.rs`).
const MAX_SETTINGS_KEY_DEPTH: usize = 64;

/// User settings use dotted keys like "al.enableCodeAnalysis" or flat keys
/// like "enableCodeAnalysis". This function strips the "al." prefix if present
/// and merges the values into the config object.
///
/// Special handling for nested keys like "al.formatting.maxLineLength" (a real
/// `AlConfig` leaf — `formatting` is a struct with `maxLineLength: u32`, not a
/// scalar) and "al.inlayHints.parameterNames" (a plain bool; there is no
/// `.enabled` sub-leaf).
pub fn apply_al_settings_to_config(
    config: &serde_json::Value,
    user_settings: &serde_json::Value,
) -> serde_json::Value {
    let settings_obj = match user_settings.as_object() {
        Some(obj) => obj,
        None => return config.clone(),
    };

    let mut result = config.clone();

    for (key, value) in settings_obj {
        // Zed settings may wrap AL options in an `al` object. Merge its
        // children at the same level as dotted and unprefixed settings.
        if key == "al" {
            if let Some(nested) = value.as_object() {
                for (child_key, child_value) in nested {
                    let effective = child_key.strip_prefix("al.").unwrap_or(child_key);
                    let parts: Vec<&str> = effective.split('.').collect();
                    set_nested_value(&mut result, &parts, child_value);
                }
                continue;
            }
        }

        let effective_key = key.strip_prefix("al.").unwrap_or(key);

        let parts: Vec<&str> = effective_key.split('.').collect();
        set_nested_value(&mut result, &parts, value);
    }

    // These launch settings are consumed by the extension itself. They are NOT
    // al-lsp server config, so they must not ride along into the server's
    // settings. Every input shape (flat, `al.`-dotted, nested `al: {…}`) lands
    // as a top-level key in `result`, so a top-level remove covers all of them.
    if let Some(obj) = result.as_object_mut() {
        obj.remove("useOfficialLsp");
        obj.remove("useOfficialDap");
        obj.remove("dotnetPath");
    }

    result
}

fn set_nested_value(target: &mut serde_json::Value, path: &[&str], value: &serde_json::Value) {
    set_nested_value_inner(target, path, value, 0);
}

/// Recursive worker that tracks the current nesting `depth` so the cap
/// limits *recursion*, not path length. Up to `MAX_SETTINGS_KEY_DEPTH`
/// levels nest as ordinary objects; only when the cap is reached do we
/// collapse the remaining segments into a single literal key. This bounds
/// stack usage from pathologically deep user-controlled keys while
/// preserving the intended nesting for everything under the cap.
fn set_nested_value_inner(
    target: &mut serde_json::Value,
    path: &[&str],
    value: &serde_json::Value,
    depth: usize,
) {
    if path.is_empty() {
        return;
    }

    // A user can supply the same key both as a scalar (`"al.formatting": "x"`)
    // — or any other non-object value, e.g. an array — and as a path nested
    // under it (`"al.formatting.maxLineLength": 100`). Bailing out here (the
    // old behavior) silently dropped the nested setting with no error or
    // fallback. Instead, coerce the intermediate value into an object so the
    // more specific (deeper) key always wins over the shallower collision,
    // rather than vanishing.
    if !target.is_object() {
        *target = json!({});
    }
    let obj = target
        .as_object_mut()
        .expect("target was just coerced into an object above");

    // Single remaining segment: insert it directly. At/over the depth cap,
    // stop recursing and store the remaining path as one joined literal key.
    if path.len() == 1 || depth >= MAX_SETTINGS_KEY_DEPTH {
        obj.insert(path.join("."), value.clone());
        return;
    }

    let child = obj.entry(path[0].to_string()).or_insert_with(|| json!({}));
    set_nested_value_inner(child, &path[1..], value, depth + 1);
}

/// The program and arguments the extension hands Zed for the language server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerLaunch {
    /// A program to run instead of the one the extension resolves, or `None`
    /// when the extension chooses `al-lsp` itself. Always `None` today.
    pub program: Option<String>,
    /// The arguments the server starts with.
    pub args: Vec<String>,
}

/// Decide what the language server session runs, given the settings Zed merged.
///
/// `LspSettings::for_worktree` returns the user's settings and the worktree's
/// `.zed/settings.json` already merged, and `zed_extension_api` 0.7 exposes no
/// way to ask for the user-level value alone. So a `binary` block arriving
/// here may be one a cloned repository wrote, and that block names a program
/// and its arguments, which together are the whole payload: a
/// `.zed/settings.json` holding
/// `{"lsp":{"al-lsp":{"binary":{"path":"/bin/sh","arguments":["-c","curl … | sh"]}}}}`
/// would run on open.
///
/// The extension runs in Zed's WASM sandbox with no filesystem and no process,
/// so it cannot read `trusted-projects.json`, and `binary.path` decides whether
/// al-lsp runs at all, so al-lsp cannot be the one to refuse it either. The
/// rule is therefore the extension's own and it is flat: `binary.path` and
/// `binary.arguments` are ignored, whoever wrote them. al-lsp is chosen by
/// this extension (session cache, then `al-lsp` on PATH, then the cached or
/// downloaded release), and its arguments come from `al.useOfficialLsp`.
///
/// To run a specific build, put it on PATH. Zed itself blocks project settings
/// in an untrusted worktree from v0.218.2-pre (advisory GHSA-29cp-2hmh-hcxj);
/// this rule is what an older Zed does not give us, and it holds on every Zed.
///
/// See `Docs/features/project-trust.md` and
/// `Docs/current-limitations.md#zed-worktree-settings-and-executable-paths`.
pub fn resolve_server_launch(
    _settings_binary_path: Option<&str>,
    _settings_binary_arguments: Option<&[String]>,
    user_settings: Option<&serde_json::Value>,
) -> ServerLaunch {
    ServerLaunch {
        program: None,
        args: resolve_server_args(user_settings),
    }
}

/// Resolve the al-lsp launch arguments from the AL settings block.
///
/// `al.useOfficialLsp: true` (flat, dotted, or nested under `"al"`) delegates
/// the session to Microsoft's official AL Language Server via `al-lsp
/// --official-lsp` (requires ALTool v17+ on the machine). Otherwise the
/// built-in native server runs over stdio.
///
/// `binary.arguments` is not a source here: see [`resolve_server_launch`].
pub fn resolve_server_args(user_settings: Option<&serde_json::Value>) -> Vec<String> {
    let use_official = user_settings
        .and_then(|s| {
            s.get("useOfficialLsp")
                .or_else(|| s.get("al.useOfficialLsp"))
                .or_else(|| s.get("al").and_then(|al| al.get("useOfficialLsp")))
        })
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if use_official {
        vec!["--official-lsp".to_string()]
    } else {
        vec!["--stdio".to_string()]
    }
}

/// Resolve which DAP backend flag `al-lsp` should launch with.
///
/// Mirrors [`resolve_server_args`] (the LSP `useOfficialLsp` toggle), keeping
/// the "native-first, opt-in official" stance consistent across backends:
///
/// 1. Default: the native BC debug adapter (`--dap`) - our own implementation
///    that speaks DAP and talks to BC over REST + SignalR directly.
/// 2. `al.useOfficialDap: true` (flat, dotted, or nested under `"al"`)
///    delegates to Microsoft's EditorServices.Host proxy (`--dap-legacy`).
///
/// There is no automatic fallback: if the native adapter fails it surfaces an
/// error rather than silently switching to the Microsoft proxy.
pub fn resolve_dap_backend_flag(user_settings: Option<&serde_json::Value>) -> &'static str {
    let use_official = user_settings
        .and_then(|s| {
            s.get("useOfficialDap")
                .or_else(|| s.get("al.useOfficialDap"))
                .or_else(|| s.get("al").and_then(|al| al.get("useOfficialDap")))
        })
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if use_official {
        "--dap-legacy"
    } else {
        "--dap"
    }
}

/// Resolve `al.dotnetPath` from every settings shape accepted by the extension.
///
/// The value is exported to child processes as `AL_DOTNET_PATH`, keeping the
/// existing CLI/environment contract while making the same override available
/// from Zed settings. Empty and non-string values are ignored so they cannot
/// replace a working `dotnet` lookup with an unusable command.
pub fn resolve_dotnet_path(user_settings: Option<&serde_json::Value>) -> Option<String> {
    user_settings
        .and_then(|settings| {
            settings
                .get("dotnetPath")
                .or_else(|| settings.get("al.dotnetPath"))
                .or_else(|| settings.get("al").and_then(|al| al.get("dotnetPath")))
        })
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(ToOwned::to_owned)
}

/// Whether `path` names a program the worktree itself carries.
///
/// `LspSettings::for_worktree` returns the user's settings and the worktree's
/// `.zed/settings.json` already merged, and `zed_extension_api` 0.7 exposes no
/// way to ask for the user-level value alone (`wit::get_settings`, which takes
/// the location, is private to the crate). So the extension cannot tell who
/// wrote `binary.path` or `dotnetPath`. It can tell where the program lives,
/// which is the part that matters: a relative path resolves against the
/// worktree, and an absolute path under the worktree root is a file the clone
/// brought with it.
///
/// Zed itself blocks project settings until a worktree is trusted, from
/// v0.218.2-pre (advisory GHSA-29cp-2hmh-hcxj). This check is what an older
/// Zed does not give us.
///
/// A false answer is not an authorisation. It says only that the program is
/// not a file the clone carried, which leaves every program the machine
/// already has. The language server's own program and arguments are decided by
/// [`resolve_server_launch`], which ignores the settings outright; this check
/// covers `dotnetPath`, where al-lsp does the rest (`trust::enforce_dotnet_path`).
///
/// See `Docs/current-limitations.md#zed-worktree-settings-and-executable-paths`.
/// Both spellings are compared as component lists rather than as text, because
/// `/home/me/src/../src/SomeApp/tools/al-lsp` and
/// `/home/me/src/SomeApp/tools/al-lsp` name one file and diverge at the fourth
/// character. Nothing here touches the filesystem: the extension runs as a
/// WASM module with no path API, so a path reached through a *symlinked*
/// ancestor is still outside what this can see.
pub fn is_worktree_resident_program(path: &str, worktree_root: &str) -> bool {
    let path = path.trim();
    if path.is_empty() {
        return false;
    }
    if !is_absolute_path(path) {
        // A relative path resolves against the worktree, wherever it points.
        return true;
    }
    let Some(program) = normalised_components(path) else {
        // `..` above the filesystem root names nothing. Refuse rather than
        // guess what the author meant.
        return true;
    };
    let Some(root) = normalised_components(worktree_root.trim()) else {
        return true;
    };
    if root.is_empty() || program.len() < root.len() {
        return false;
    }
    // A Windows path is matched without regard to case, because the filesystem
    // is: `c:\users\me` and `C:\Users\Me` are the same directory.
    let ignore_case = looks_like_windows(path) || looks_like_windows(worktree_root);
    root.iter().zip(&program).all(|(root, program)| {
        if ignore_case {
            root.eq_ignore_ascii_case(program)
        } else {
            root == program
        }
    })
}

/// Whether `path` starts at a filesystem root: `/…`, `\…`, or a drive such as
/// `C:\…` or `C:/…`.
fn is_absolute_path(path: &str) -> bool {
    if path.starts_with('/') || path.starts_with('\\') {
        return true;
    }
    let bytes = path.as_bytes();
    bytes.len() > 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

/// Whether the path is written the way Windows writes one, which is how the
/// extension knows to compare it without regard to case. The extension is a
/// WASM module and cannot ask the host what it runs on.
fn looks_like_windows(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.contains('\\') || (bytes.len() > 1 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
}

/// The components of `path`, with `.` dropped, repeated separators collapsed
/// and `..` folded into the component before it.
///
/// `None` when a `..` climbs above the first component, which is a path no
/// comparison can make sense of.
fn normalised_components(path: &str) -> Option<Vec<&str>> {
    let mut components: Vec<&str> = Vec::new();
    for part in path.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                components.pop()?;
            }
            name => components.push(name),
        }
    }
    Some(components)
}

/// Select and normalize the AL settings needed by the separate DAP process
/// when it performs a launch build.
///
/// Zed gives the extension the live global/project settings, but `al-lsp
/// --dap` is a new process and cannot query them. Passing only compile and
/// package-selection keys avoids configuration drift without exporting
/// unrelated editor state.
pub fn compile_settings_for_child(user_settings: Option<&serde_json::Value>) -> serde_json::Value {
    const KEYS: &[&str] = &[
        "codeAnalyzers",
        "enableExternalRulesets",
        "ruleSetPath",
        "assemblyProbingPaths",
        "outputAnalyzerStatistics",
        "packageCachePath",
        "appLocalFolderPaths",
        "compilationOptions",
        "incrementalBuild",
        "useOfficialCompiler",
    ];

    let normalized = user_settings
        .map(|settings| apply_al_settings_to_config(&json!({}), settings))
        .unwrap_or_else(|| json!({}));
    let mut selected = serde_json::Map::new();
    if let Some(object) = normalized.as_object() {
        for key in KEYS {
            if let Some(value) = object.get(*key) {
                selected.insert((*key).to_string(), value.clone());
            }
        }
    }
    serde_json::Value::Object(selected)
}
