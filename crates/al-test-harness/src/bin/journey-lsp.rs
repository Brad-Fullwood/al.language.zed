//! Journey test, LSP half: asks al-lsp directly for the features the Zed
//! stage checks through the editor, and prints one line per feature:
//! `PASS|FAIL|SKIP <feature> <detail>`. Raw responses go to `--out` as JSON.
//!
//! Usage: `journey-lsp --project DIR --out DIR [--prime-only]`
//! The al-lsp binary is read from `AL_LSP_BIN` (see `al_lsp_binary`).
//! The project must contain `src/JourneyEditor.Codeunit.al`.

use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use al_test_harness::LspClient;
use serde_json::{json, Value};

const FILE: &str = "src/JourneyEditor.Codeunit.al";
/// Longest wait for the first didOpen acknowledgement.
const OPEN_LIMIT_MS: u64 = 120_000;

struct Report {
    out: PathBuf,
    lines: Vec<String>,
}

impl Report {
    fn line(&mut self, status: &str, feature: &str, detail: String) {
        let line = format!("{status} {feature} {detail}");
        println!("{line}");
        let _ = std::io::stdout().flush();
        self.lines.push(line);
        let _ = std::fs::write(self.out.join("lines.txt"), self.lines.join("\n") + "\n");
    }

    fn save(&self, name: &str, value: &Value) {
        let text = serde_json::to_string_pretty(value).unwrap_or_default();
        let _ = std::fs::write(self.out.join(format!("{name}.json")), text);
    }
}

fn arg(name: &str) -> String {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| {
            eprintln!("usage: journey-lsp --project DIR --out DIR");
            std::process::exit(2);
        })
}

/// Line and UTF-16-free column (the file is ASCII) of the `nth` match of `needle`, plus `shift`.
fn pos_of(text: &str, needle: &str, nth: usize, shift: usize) -> (u32, u32) {
    let (offset, _) = text
        .match_indices(needle)
        .nth(nth)
        .unwrap_or_else(|| panic!("{needle:?} not found in {FILE}"));
    let before = &text[..offset];
    let line = before.matches('\n').count() as u32;
    let col = offset - before.rfind('\n').map_or(0, |i| i + 1) + shift;
    (line, col as u32)
}

fn is_snippet(item: &Value) -> bool {
    item["insertTextFormat"] == json!(2) || item["kind"] == json!(15)
}

fn ms(start: Instant) -> u128 {
    start.elapsed().as_millis()
}

fn labels(items: &[Value]) -> Vec<String> {
    items
        .iter()
        .filter_map(|i| i["label"].as_str().map(str::to_string))
        .collect()
}

fn flatten_symbols(symbols: &[Value], out: &mut Vec<String>) {
    for s in symbols {
        if let Some(name) = s["name"].as_str() {
            out.push(name.to_string());
        }
        if let Some(children) = s["children"].as_array() {
            flatten_symbols(children, out);
        }
    }
}

fn edit_count(edit: &Value) -> (usize, usize, Vec<String>) {
    let mut count = 0;
    let mut files = 0;
    let mut texts = Vec::new();
    let mut take = |edits: &Value| {
        files += 1;
        for e in edits.as_array().into_iter().flatten() {
            count += 1;
            texts.push(e["newText"].as_str().unwrap_or("").to_string());
        }
    };
    if let Some(changes) = edit["changes"].as_object() {
        for edits in changes.values() {
            take(edits);
        }
    }
    for dc in edit["documentChanges"].as_array().into_iter().flatten() {
        take(&dc["edits"]);
    }
    (count, files, texts)
}

async fn run(project: PathBuf, out: PathBuf) {
    let mut r = Report {
        out: out.clone(),
        lines: Vec::new(),
    };
    let original = match std::fs::read_to_string(project.join(FILE)) {
        Ok(t) => t,
        Err(e) => {
            r.line("FAIL", "setup", format!("cannot read {FILE}: {e}"));
            return;
        }
    };

    let t = Instant::now();
    let mut c = match LspClient::spawn_with_snippets(&project, true).await {
        Ok(c) => c,
        Err(e) => {
            r.line("FAIL", "initialize", format!("spawn failed: {e}"));
            return;
        }
    };
    let init_ms = ms(t);
    let init = c.initialize_result().clone();
    r.save("initialize", &init);
    let caps = &init["capabilities"];
    let want = [
        "completionProvider",
        "hoverProvider",
        "definitionProvider",
        "renameProvider",
        "documentFormattingProvider",
        "documentSymbolProvider",
        "codeActionProvider",
        "referencesProvider",
    ];
    let missing: Vec<&str> = want
        .iter()
        .copied()
        .filter(|k| {
            caps.get(k)
                .is_none_or(|v| v.is_null() || *v == json!(false))
        })
        .collect();
    let has_pull = caps["diagnosticProvider"].is_object();
    if missing.is_empty() {
        r.line(
            "PASS",
            "lsp.initialize",
            format!(
                "all {} providers advertised, pullDiagnostics={has_pull}, spawn+index {} ms",
                want.len(),
                ms(t)
            ),
        );
    } else {
        r.line(
            "FAIL",
            "lsp.initialize",
            format!("missing capabilities: {} ({} ms)", missing.join(","), ms(t)),
        );
    }

    let t = Instant::now();
    // The first open waits for the project scan and, with the semantic feature, the .NET bridge start.
    // Wait up to OPEN_LIMIT_MS for the acknowledgement. Over 5 s is a NIGGLE, never acknowledged is the FAIL.
    match c
        .open_file_within(FILE, &original, std::time::Duration::from_millis(OPEN_LIMIT_MS))
        .await
    {
        Some(d) if d.as_millis() > 5_000 => r.line(
            "NIGGLE",
            "lsp.open.time",
            format!("al-lsp acknowledged the first didOpen after {} ms (over 5 s, initialize took {} ms before it)", d.as_millis(), init_ms),
        ),
        Some(_) => {}
        None => {
            r.line(
                "FAIL",
                "lsp.open",
                format!("al-lsp never acknowledged didOpen within {OPEN_LIMIT_MS} ms"),
            );
            return;
        }
    }
    if std::env::args().any(|a| a == "--prime-only") {
        // The dependency source index is ready before the first open is acknowledged. Stop here so
        // the summaries it wrote under the user data dir can be mounted into the editor container.
        r.line(
            "PASS",
            "lsp.prime",
            format!("first open acknowledged after {} ms", ms(t)),
        );
        return;
    }
    let uri = c.file_uri(FILE);
    let opened = c.drain_diagnostics();
    let base = opened.get(&uri).cloned().unwrap_or_default();
    r.save("diagnostics-open", &json!(base));
    let base_errors = base.iter().filter(|d| d["severity"] == json!(1)).count();
    if base_errors == 0 {
        r.line(
            "PASS",
            "lsp.open",
            format!("opened, 0 errors, {} diagnostics, {} ms", base.len(), ms(t)),
        );
    } else {
        r.line(
            "FAIL",
            "lsp.open",
            format!(
                "clean file reports {base_errors} errors: {}",
                base[0]["message"]
            ),
        );
    }

    // hover on the AddNumbers call
    let (l, col) = pos_of(&original, "AddNumbers(1", 0, 3);
    let t = Instant::now();
    let hover = c.hover(FILE, l, col).await;
    let dur = ms(t);
    r.save("hover", &hover.clone().unwrap_or(Value::Null));
    match hover {
        Some(h) if h.to_string().contains("AddNumbers") => {
            r.line("PASS", "lsp.hover", format!("signature shown, {dur} ms"))
        }
        Some(h) => r.line(
            "FAIL",
            "lsp.hover",
            format!("no AddNumbers in hover: {h} ({dur} ms)"),
        ),
        None => r.line(
            "FAIL",
            "lsp.hover",
            format!("null hover on a procedure call ({dur} ms)"),
        ),
    }

    // go to definition
    let t = Instant::now();
    let def = c.definition(FILE, l, col).await;
    let dur = ms(t);
    r.save("definition", &def.clone().unwrap_or(Value::Null));
    let (decl_line, _) = pos_of(&original, "procedure AddNumbers", 0, 0);
    let target_line = def.as_ref().and_then(|d| {
        let loc = if d.is_array() { &d[0] } else { d };
        let uri_ok = loc["uri"]
            .as_str()
            .is_some_and(|u| u.ends_with("JourneyEditor.Codeunit.al"));
        let line = loc["range"]["start"]["line"]
            .as_u64()
            .or_else(|| loc["targetRange"]["start"]["line"].as_u64());
        uri_ok.then_some(line).flatten()
    });
    match target_line {
        Some(line) if line == u64::from(decl_line) => r.line(
            "PASS",
            "lsp.definition",
            format!("-> line {}, {dur} ms", line + 1),
        ),
        other => r.line(
            "FAIL",
            "lsp.definition",
            format!("expected line {} got {other:?} ({dur} ms)", decl_line + 1),
        ),
    }

    // completion and snippets, in a variant with a partial identifier
    let partial = original.replacen(
        "        exit(Total);",
        "        Tot\n        exit(Total);",
        1,
    );
    c.change_file(FILE, &partial).await;
    let _ = c.drain_diagnostics();
    let (l, col) = pos_of(&partial, "        Tot\n", 0, 11);
    let t = Instant::now();
    let items = c.completion(FILE, l, col).await;
    let dur = ms(t);
    r.save("completion", &json!(items));
    let names = labels(&items);
    if names.iter().any(|n| n.eq_ignore_ascii_case("Total")) {
        r.line(
            "PASS",
            "lsp.completion",
            format!("{} items incl. Total, {dur} ms", items.len()),
        );
    } else {
        r.line(
            "FAIL",
            "lsp.completion",
            format!(
                "Total not offered among {} items ({dur} ms), first: {:?}",
                items.len(),
                names.iter().take(5).collect::<Vec<_>>()
            ),
        );
    }
    let snippets_here = items
        .iter()
        .filter(|i| i["insertTextFormat"] == json!(2))
        .count();
    let (l2, c2) = pos_of(&partial, "    procedure Messy", 0, 4);
    let t = Instant::now();
    let items2 = c.completion(FILE, l2, c2).await;
    let dur2 = ms(t);
    r.save("completion-object-level", &json!(items2));
    let snippets_obj: Vec<String> = items2
        .iter()
        .filter(|i| is_snippet(i))
        .filter_map(|i| i["label"].as_str().map(str::to_string))
        .collect();
    if snippets_here + snippets_obj.len() > 0 {
        r.line(
            "PASS",
            "lsp.snippets",
            format!(
                "{snippets_here} snippet items in a body, {} at object level (e.g. {:?}), {dur2} ms",
                snippets_obj.len(),
                snippets_obj.iter().take(3).collect::<Vec<_>>()
            ),
        );
    } else {
        r.line(
            "FAIL",
            "lsp.snippets",
            format!(
                "no completion item is a snippet (insertTextFormat 2 or kind 15) ({} items in body, {} at object level)",
                items.len(),
                items2.len()
            ),
        );
    }

    // semantic error first: an undeclared identifier needs the semantic bridge
    let undeclared = original.replacen(
        "        exit(Total);",
        "        Undeclared := 1;\n        exit(Total);",
        1,
    );
    c.change_file(FILE, &undeclared).await;
    let sem = c.drain_diagnostics().remove(&uri).unwrap_or_default();
    r.save("diagnostics-undeclared", &json!(sem));
    if sem.iter().any(|d| {
        d["message"]
            .as_str()
            .is_some_and(|m| m.contains("Undeclared"))
    }) {
        r.line(
            "PASS",
            "lsp.diagnostics-semantic",
            format!("{} diagnostics name Undeclared", sem.len()),
        );
    } else {
        r.line(
            "FAIL",
            "lsp.diagnostics-semantic",
            format!(
                "undeclared identifier gives {} diagnostics and none names it (needs the semantic bridge, which is off when AlBridge.dll is not beside al-lsp)",
                sem.len()
            ),
        );
    }

    // deliberate syntax error (native parser), code action, fix
    let broken = original.replacen(
        "        exit(Total);",
        "        Total := 5\n        exit(Total);",
        1,
    );
    let t = Instant::now();
    c.change_file(FILE, &broken).await;
    let dur = ms(t);
    let diags = c.drain_diagnostics().remove(&uri).unwrap_or_default();
    r.save("diagnostics-error", &json!(diags));
    let err = diags
        .iter()
        .find(|d| d["severity"] == json!(1) || d["severity"].is_null());
    let (err_line, _) = pos_of(&broken, "Total := 5", 0, 0);
    match err {
        Some(d) => {
            let got = d["range"]["start"]["line"].as_u64().unwrap_or(0);
            let near = got.abs_diff(u64::from(err_line)) <= 1;
            r.line(
                if near { "PASS" } else { "FAIL" },
                "lsp.diagnostics",
                format!(
                    "error on line {} (expected {} or next): {} [{}], {dur} ms",
                    got + 1,
                    err_line + 1,
                    d["message"].as_str().unwrap_or(""),
                    d["code"]
                ),
            );
        }
        None => r.line(
            "FAIL",
            "lsp.diagnostics",
            format!(
                "no error published for a missing semicolon ({} diagnostics, {dur} ms)",
                diags.len()
            ),
        ),
    }

    let t = Instant::now();
    let ca = c
        .raw_request(
            "textDocument/codeAction",
            json!({
                "textDocument": { "uri": uri },
                "range": {
                    "start": { "line": err_line, "character": 8 },
                    "end": { "line": err_line, "character": 18 }
                },
                "context": { "diagnostics": diags }
            }),
        )
        .await;
    let dur = ms(t);
    match ca {
        Ok(v) => {
            r.save("codeAction", &v);
            let actions: Vec<&Value> = v.as_array().into_iter().flatten().collect();
            let titles: Vec<&str> = actions.iter().filter_map(|a| a["title"].as_str()).collect();
            let quickfix = actions.iter().any(|a| {
                a["kind"]
                    .as_str()
                    .is_some_and(|k| k.starts_with("quickfix"))
                    || a["diagnostics"].as_array().is_some_and(|d| !d.is_empty())
            });
            let first: Vec<&&str> = titles.iter().take(4).collect();
            if titles.is_empty() {
                r.line(
                    "FAIL",
                    "lsp.codeAction",
                    format!("no code action at the error ({dur} ms)"),
                );
            } else if quickfix {
                r.line(
                    "PASS",
                    "lsp.codeAction",
                    format!(
                        "{} actions incl. a quickfix for the error: {first:?}, {dur} ms",
                        titles.len()
                    ),
                );
            } else {
                r.line(
                    "FAIL",
                    "lsp.codeAction",
                    format!(
                        "{} generic actions, none is a quickfix for the error: {first:?}, {dur} ms",
                        titles.len()
                    ),
                );
            }
        }
        Err(e) => r.line(
            "FAIL",
            "lsp.codeAction",
            format!("request error: {e} ({dur} ms)"),
        ),
    }

    let t = Instant::now();
    c.change_file(FILE, &original).await;
    let dur = ms(t);
    let mut after = c.drain_diagnostics().remove(&uri);
    let count_errors = |d: &Option<Vec<Value>>| {
        d.as_ref()
            .map(|d| d.iter().filter(|x| x["severity"] == json!(1)).count())
    };
    let first_errors = count_errors(&after);
    // The first publish after the edit can still carry the old error while the semantic pass of the
    // previous text finishes. Wait up to 10 s for a later publish and judge the last state.
    let mut settle_ms = 0u128;
    if first_errors.is_some_and(|n| n > 0) {
        let settle = Instant::now();
        while settle.elapsed() < std::time::Duration::from_secs(10) {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            if let Some(later) = c.drain_diagnostics().remove(&uri) {
                after = Some(later);
            }
            if count_errors(&after) == Some(0) {
                break;
            }
        }
        settle_ms = ms(settle);
    }
    match (after.is_some(), count_errors(&after)) {
        (true, Some(0)) if settle_ms == 0 => r.line(
            "PASS",
            "lsp.diagnostics-clear",
            format!("error cleared after the fix, {dur} ms"),
        ),
        (true, Some(0)) => {
            r.line(
                "PASS",
                "lsp.diagnostics-clear",
                format!("error cleared after the fix, {} ms", dur + settle_ms),
            );
            r.line(
                "NIGGLE",
                "lsp.diagnostics-clear.stale",
                format!(
                    "the first publish after the fix still had {} error(s), the clean publish came {settle_ms} ms later",
                    first_errors.unwrap_or(0)
                ),
            );
        }
        (true, Some(n)) => r.line(
            "FAIL",
            "lsp.diagnostics-clear",
            format!(
                "{n} errors remain after the fix, still there after waiting {settle_ms} ms more"
            ),
        ),
        _ => r.line(
            "FAIL",
            "lsp.diagnostics-clear",
            "no publishDiagnostics after the fix".to_string(),
        ),
    }

    // rename
    let (l, col) = pos_of(&original, "procedure AddNumbers", 0, 10);
    let t = Instant::now();
    let rename = c.rename(FILE, l, col, "SumNumbers").await;
    let dur = ms(t);
    r.save("rename", &rename.clone().unwrap_or(Value::Null));
    match rename {
        Some(edit) => {
            let (n, files, texts) = edit_count(&edit);
            let all_new = texts.iter().all(|x| x == "SumNumbers");
            if n >= 2 && all_new {
                r.line(
                    "PASS",
                    "lsp.rename",
                    format!("{n} edits in {files} file(s) (declaration + call), {dur} ms"),
                );
            } else {
                r.line(
                    "FAIL",
                    "lsp.rename",
                    format!("expected >=2 edits to SumNumbers, got {n} {texts:?} ({dur} ms)"),
                );
            }
        }
        None => r.line(
            "FAIL",
            "lsp.rename",
            format!("null rename result ({dur} ms)"),
        ),
    }

    // formatting
    let t = Instant::now();
    let edits = c.format(FILE).await;
    let dur = ms(t);
    r.save("formatting", &json!(edits));
    if edits.is_empty() {
        r.line(
            "FAIL",
            "lsp.formatting",
            format!("no edits for a badly indented file ({dur} ms)"),
        );
    } else {
        r.line(
            "PASS",
            "lsp.formatting",
            format!("{} edits, {dur} ms", edits.len()),
        );
    }

    // document symbols
    let t = Instant::now();
    let syms = c.document_symbols(FILE).await;
    let dur = ms(t);
    r.save("documentSymbol", &json!(syms));
    let mut names = Vec::new();
    flatten_symbols(&syms, &mut names);
    let ok = ["AddNumbers", "UseIt", "Messy"]
        .iter()
        .all(|n| names.iter().any(|x| x.contains(n)));
    r.line(
        if ok { "PASS" } else { "FAIL" },
        "lsp.documentSymbol",
        format!(
            "{} symbols {:?}, {dur} ms",
            names.len(),
            names.iter().take(6).collect::<Vec<_>>()
        ),
    );

    // workspace diagnostics
    if has_pull && caps["diagnosticProvider"]["workspaceDiagnostics"] == json!(true) {
        let t = Instant::now();
        let ws = c
            .raw_request("workspace/diagnostic", json!({ "previousResultIds": [] }))
            .await;
        let dur = ms(t);
        match ws {
            Ok(v) => {
                r.save("workspaceDiagnostic", &v);
                let n = v["items"].as_array().map_or(0, Vec::len);
                r.line(
                    "PASS",
                    "lsp.project-diagnostics",
                    format!("{n} document reports, {dur} ms"),
                );
            }
            Err(e) => r.line("FAIL", "lsp.project-diagnostics", format!("{e} ({dur} ms)")),
        }
    } else {
        let t = Instant::now();
        let ws = c.workspace_symbol("Journey").await;
        let dur = ms(t);
        r.save("workspaceSymbol", &json!(ws));
        r.line(
            "SKIP",
            "lsp.project-diagnostics",
            format!(
                "workspace/diagnostic not advertised, pushed diagnostics only (workspace/symbol Journey -> {} hits, {dur} ms)",
                ws.len()
            ),
        );
    }

    c.shutdown().await;
}

fn main() {
    let project = PathBuf::from(arg("--project"));
    let out = PathBuf::from(arg("--out"));
    let _ = std::fs::create_dir_all(&out);
    if std::env::var_os("RUST_LOG").is_none() {
        std::env::set_var("RUST_LOG", "warn");
    }
    // A panic inside the harness client (it asserts on timeouts) becomes one FAIL line.
    let out2 = out.clone();
    let worker = std::thread::spawn(move || {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime")
            .block_on(run(project, out));
    });
    if let Err(e) = worker.join() {
        let msg = e
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default();
        let line = format!("FAIL lsp.harness client panicked: {msg}");
        println!("{line}");
        let lines = std::fs::read_to_string(out2.join("lines.txt")).unwrap_or_default();
        let _ = std::fs::write(out2.join("lines.txt"), format!("{lines}{line}\n"));
    }
}
