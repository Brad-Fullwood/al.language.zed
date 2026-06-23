//! Per-file language operations: lint, format, hover, definition/references/signature/completions, document symbols/folding/tokens, and rename.

use std::process::ExitCode;

use crate::cli::commands::*;

pub fn cmd_lint(file: Option<&str>, all: bool, analyzers: Option<&str>, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let mut params = serde_json::json!({ "all": all });
    if let Some(a) = analyzers {
        let analyzer_list: Vec<&str> = a.split(',').map(|s| s.trim()).collect();
        params["analyzers"] = serde_json::json!(analyzer_list);
    }
    if let Some(f) = file {
        // file_to_uri() prints "file not found" on failure; surface a clear
        // client-side error instead of forwarding an unresolvable raw path that
        // would only trigger a second, confusing error from the daemon.
        let Some(uri) = file_to_uri(f) else {
            return report_error(&format!("Cannot resolve path: {f}"), json);
        };
        params["uri"] = serde_json::json!(uri);
    }
    match client.request("lint", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
                return ExitCode::SUCCESS;
            }
            let found_diagnostics = if all {
                let mut total = 0usize;
                if let Some(files) = result.as_array() {
                    for file_result in files {
                        let fname = file_result
                            .get("file")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        if let Some(diags) =
                            file_result.get("diagnostics").and_then(|v| v.as_array())
                        {
                            for d in diags {
                                print_lint_diag(Some(fname), d);
                            }
                            total += diags.len();
                        }
                    }
                    eprintln!("\n{} diagnostics across {} files", total, files.len());
                }
                total > 0
            } else {
                let diagnostics = result.as_array().cloned().unwrap_or_default();
                if diagnostics.is_empty() {
                    eprintln!("No issues found");
                } else {
                    for d in &diagnostics {
                        print_lint_diag(file, d);
                    }
                    eprintln!("\n{} diagnostics", diagnostics.len());
                }
                !diagnostics.is_empty()
            };
            if found_diagnostics {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => report_error(&e, json),
    }
}

pub fn cmd_format(file: Option<&str>, check: bool, stdin: bool, all: bool, json: bool) -> ExitCode {
    if stdin {
        return cmd_format_stdin(check, json);
    }
    if all {
        return cmd_format_all(check, json);
    }

    let Some(file) = file else {
        return report_error("No file specified. Use --stdin or --all.", json);
    };

    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let Some(uri) = file_to_uri(file) else {
        return report_error(&format!("Cannot resolve path: {file}"), json);
    };
    let params = serde_json::json!({ "check": check, "uri": uri });
    match client.request("format", Some(params)) {
        Ok(result) => {
            let changed = result
                .get("changed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if json {
                print_json(&result);
            } else if check {
                if changed {
                    eprintln!("{file}: would reformat");
                } else {
                    eprintln!("{file}: already formatted");
                }
            } else if changed {
                eprintln!("{file}: formatted");
            } else {
                eprintln!("{file}: already formatted");
            }
            if check && changed {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_format_stdin(check: bool, json: bool) -> ExitCode {
    use std::io::Read;
    let mut content = String::new();
    if std::io::stdin().read_to_string(&mut content).is_err() {
        return report_error("Failed to read from stdin", json);
    }
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let params = serde_json::json!({ "content": content, "check": check });
    match client.request("format", Some(params)) {
        Ok(result) => {
            if check {
                let changed = result
                    .get("changed")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if json {
                    print_json(&serde_json::json!({ "changed": changed }));
                }
                if changed {
                    ExitCode::FAILURE
                } else {
                    ExitCode::SUCCESS
                }
            } else {
                let formatted = result
                    .get("formatted")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if json {
                    print_json(&serde_json::json!({ "formatted": formatted }));
                } else {
                    print!("{formatted}");
                }
                ExitCode::SUCCESS
            }
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_format_all(check: bool, json: bool) -> ExitCode {
    let root = project_root(None);
    let al_files = collect_al_files(&root);
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let mut changed_count = 0;
    let mut error_count = 0;
    let mut total = 0;
    for path in &al_files {
        total += 1;
        if let Some(uri) = url::Url::from_file_path(path).ok().map(|u| u.to_string()) {
            let params = serde_json::json!({ "uri": uri, "check": check });
            match client.request("format", Some(params)) {
                Ok(result) => {
                    let changed = result
                        .get("changed")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if changed {
                        changed_count += 1;
                        if !json {
                            let display = path.strip_prefix(&root).unwrap_or(path);
                            if check {
                                eprintln!("  would reformat: {}", display.display());
                            } else {
                                eprintln!("  formatted: {}", display.display());
                            }
                        }
                    }
                }
                Err(e) => {
                    error_count += 1;
                    eprintln!("Error formatting {}: {e}", path.display());
                }
            }
        }
    }
    if json {
        print_json(&serde_json::json!({
            "total": total,
            "changed": changed_count,
            "errors": error_count,
            "check": check
        }));
    } else {
        eprintln!(
            "\n{total} files, {changed_count} {}{}",
            if check { "would change" } else { "formatted" },
            if error_count > 0 {
                format!(", {error_count} error(s)")
            } else {
                String::new()
            }
        );
    }
    if (check && changed_count > 0) || error_count > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

pub fn cmd_hover(file: &str, line: u32, col: u32, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let Some(uri) = file_to_uri(file) else {
        return report_error(&format!("Cannot resolve path: {file}"), json);
    };
    let params = serde_json::json!({
        "uri": uri,
        "line": line.saturating_sub(1),
        "character": col.saturating_sub(1),
    });
    match client.request("hover", Some(params)) {
        Ok(result) => {
            if result.is_null() {
                if json {
                    print_json(&serde_json::json!(null));
                } else {
                    eprintln!("No symbol at {file}:{line}:{col}");
                }
            } else if json {
                print_json(&result);
            } else {
                let contents = result
                    .get("contents")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let display = contents
                    .replace("```al\n", "")
                    .replace("```\n", "")
                    .replace("```", "")
                    .replace("*(", "(")
                    .replace(")*", ")");
                println!("{}", display.trim());
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

pub fn cmd_position_query(method: &str, file: &str, line: u32, col: u32, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let Some(uri) = file_to_uri(file) else {
        return report_error(&format!("Cannot resolve path: {file}"), json);
    };
    let params = serde_json::json!({
        "uri": uri,
        "line": line.saturating_sub(1),
        "character": col.saturating_sub(1),
    });
    match client.request(method, Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else if result.is_null() {
                eprintln!("No results at {file}:{line}:{col}");
            } else {
                print_position_query_human(method, &result);
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn print_position_query_human(method: &str, result: &serde_json::Value) {
    /// Extract `file:line:col` from an LSP Location object.
    fn location_str(loc: &serde_json::Value) -> Option<String> {
        let uri = loc.get("uri").and_then(|v| v.as_str())?;
        let path = url::Url::parse(uri)
            .ok()
            .and_then(|u| u.to_file_path().ok())
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| uri.to_string());
        let line = loc
            .get("range")
            .and_then(|r| r.get("start"))
            .and_then(|s| s.get("line"))
            .and_then(|v| v.as_u64())
            .map(|l| l + 1) // convert 0-based to 1-based
            .unwrap_or(0);
        let col = loc
            .get("range")
            .and_then(|r| r.get("start"))
            .and_then(|s| s.get("character"))
            .and_then(|v| v.as_u64())
            .map(|c| c + 1)
            .unwrap_or(0);
        Some(format!("{path}:{line}:{col}"))
    }

    match method {
        "definition" | "typeDefinition" | "declaration" | "implementation" => {
            if let Some(locations) = result.as_array() {
                for loc in locations {
                    if let Some(s) = location_str(loc) {
                        println!("{s}");
                    }
                }
            } else if let Some(s) = location_str(result) {
                println!("{s}");
            } else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(result).unwrap_or_default()
                );
            }
        }
        "references" => {
            if let Some(locations) = result.as_array() {
                for loc in locations {
                    if let Some(s) = location_str(loc) {
                        println!("{s}");
                    }
                }
                eprintln!("\n{} reference(s)", locations.len());
            } else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(result).unwrap_or_default()
                );
            }
        }
        _ => {
            println!("Result:");
            println!(
                "{}",
                serde_json::to_string_pretty(result).unwrap_or_default()
            );
        }
    }
}

pub fn cmd_symbols(file: &str, json: bool) -> ExitCode {
    cmd_file_query("documentSymbols", file, json)
}

pub fn cmd_folding(file: &str, json: bool) -> ExitCode {
    cmd_file_query("foldingRanges", file, json)
}

pub fn cmd_tokens(file: &str, json: bool) -> ExitCode {
    cmd_file_query("semanticTokens", file, json)
}

fn cmd_file_query(method: &str, file: &str, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let Some(uri) = file_to_uri(file) else {
        return report_error(&format!("Cannot resolve path: {file}"), json);
    };
    let params = serde_json::json!({ "uri": uri });
    match client.request(method, Some(params)) {
        Ok(result) => {
            if json || !result.is_null() {
                print_json(&result);
            } else {
                eprintln!("No results for {file}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

pub fn cmd_rename(
    file: &str,
    line: u32,
    col: u32,
    new_name: &str,
    dry_run: bool,
    json: bool,
) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let Some(uri) = file_to_uri(file) else {
        return report_error(&format!("Cannot resolve path: {file}"), json);
    };
    let params = serde_json::json!({
        "uri": uri,
        "line": line.saturating_sub(1),
        "character": col.saturating_sub(1),
        "newName": new_name,
    });
    match client.request("rename", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
                return ExitCode::SUCCESS;
            }
            if result.is_null() {
                eprintln!("Cannot rename symbol at {file}:{line}:{col}");
                return ExitCode::FAILURE;
            }
            if let Some(changes) = result.get("changes").and_then(|v| v.as_object()) {
                let mut total_edits = 0;
                for edits in changes.values() {
                    if let Some(arr) = edits.as_array() {
                        total_edits += arr.len();
                    }
                }
                if dry_run {
                    for (uri, edits) in changes {
                        if let Some(arr) = edits.as_array() {
                            println!("{uri}: {} edit(s)", arr.len());
                        }
                    }
                    eprintln!("\n{total_edits} total edits (dry run, not applied)");
                } else {
                    match apply_workspace_edit(changes) {
                        Ok(files_changed) => {
                            eprintln!(
                                "{total_edits} edit(s) applied across {} file(s)",
                                files_changed
                            );
                        }
                        Err(e) => {
                            eprintln!("Error applying edits: {e}");
                            return ExitCode::FAILURE;
                        }
                    }
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

/// Apply a WorkspaceEdit `changes` map to disk.
///
/// Each key is a `file://` URI and each value is an array of LSP TextEdit
/// objects (`{ range: { start, end }, newText }`).  Edits are applied in
/// reverse position order so that later offsets are not invalidated by earlier
/// mutations.  Returns the number of files that were written.
fn apply_workspace_edit(
    changes: &serde_json::Map<String, serde_json::Value>,
) -> Result<usize, String> {
    let mut files_changed = 0;

    for (uri, edits_val) in changes {
        let edits = match edits_val.as_array() {
            Some(a) if !a.is_empty() => a,
            _ => continue,
        };

        let path = url::Url::parse(uri)
            .ok()
            .and_then(|u| u.to_file_path().ok())
            .ok_or_else(|| format!("Cannot resolve URI to file path: {uri}"))?;

        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;

        let lines: Vec<&str> = content.split('\n').collect();

        /// Convert a 0-based LSP (line, character) — where character is a
        /// UTF-16 code unit index — to a byte offset in `content`.
        fn lsp_pos_to_byte_offset(
            lines: &[&str],
            line: u64,
            character: u64,
        ) -> Result<usize, String> {
            if line as usize >= lines.len() {
                // Position is past EOF — clamp to end of content.
                return Ok(lines
                    .iter()
                    .map(|l| l.len() + 1)
                    .sum::<usize>()
                    .saturating_sub(1));
            }
            let line_start: usize = lines[..line as usize]
                .iter()
                .map(|l| l.len() + 1) // +1 for the '\n' we split on
                .sum();
            let line_str = lines[line as usize];
            let mut utf16_count = 0u64;
            for (byte_pos, ch) in line_str.char_indices() {
                if utf16_count >= character {
                    return Ok(line_start + byte_pos);
                }
                utf16_count += ch.len_utf16() as u64;
            }
            // character is at or beyond the end of the line.
            Ok(line_start + line_str.len())
        }

        // Parse and sort edits in reverse start-position order so that
        // applying them from the end does not shift the offsets of earlier edits.
        let mut parsed_edits: Vec<(u64, u64, u64, u64, &str)> = edits
            .iter()
            .map(|e| {
                let range = e.get("range").ok_or("missing range")?;
                let start = range.get("start").ok_or("missing start")?;
                let end = range.get("end").ok_or("missing end")?;
                let sl = start
                    .get("line")
                    .and_then(|v| v.as_u64())
                    .ok_or("missing start.line")?;
                let sc = start
                    .get("character")
                    .and_then(|v| v.as_u64())
                    .ok_or("missing start.character")?;
                let el = end
                    .get("line")
                    .and_then(|v| v.as_u64())
                    .ok_or("missing end.line")?;
                let ec = end
                    .get("character")
                    .and_then(|v| v.as_u64())
                    .ok_or("missing end.character")?;
                let new_text = e
                    .get("newText")
                    .and_then(|v| v.as_str())
                    .ok_or("missing newText")?;
                Ok((sl, sc, el, ec, new_text))
            })
            .collect::<Result<_, &str>>()
            .map_err(|e| format!("Invalid edit in {uri}: {e}"))?;

        parsed_edits.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));

        // Pre-compute all byte ranges from the *original* lines before any
        // mutation so that later edits don't shift the offsets used by earlier
        // ones.  The edits are already sorted descending, so applying them in
        // order produces correct results.
        let mut byte_ranges: Vec<(usize, usize, &str)> = Vec::with_capacity(parsed_edits.len());
        for (sl, sc, el, ec, new_text) in &parsed_edits {
            let start_byte = lsp_pos_to_byte_offset(&lines, *sl, *sc)?;
            let end_byte = lsp_pos_to_byte_offset(&lines, *el, *ec)?;
            if start_byte > content.len() || end_byte > content.len() || start_byte > end_byte {
                return Err(format!(
                    "Edit range out of bounds in {uri}: {sl}:{sc}-{el}:{ec}"
                ));
            }
            byte_ranges.push((start_byte, end_byte, new_text));
        }

        let mut new_content = content.clone();
        for (start_byte, end_byte, new_text) in byte_ranges {
            new_content.replace_range(start_byte..end_byte, new_text);
        }

        if new_content != content {
            std::fs::write(&path, &new_content)
                .map_err(|e| format!("Cannot write {}: {e}", path.display()))?;
            files_changed += 1;
        }
    }

    Ok(files_changed)
}
