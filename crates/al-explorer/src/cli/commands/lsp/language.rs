//! Per-file language operations: lint, format, hover, definition/references/signature/completions, document symbols/folding/tokens, and rename.

use std::process::ExitCode;

use crate::cli::commands::*;

pub fn cmd_lint(file: Option<&str>, all: bool, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let mut params = serde_json::json!({ "all": all });
    if let Some(f) = file {
        let uri = match file_to_uri(f) {
            Ok(uri) => uri,
            Err(error) => return report_error(&error, json),
        };
        params["uri"] = serde_json::json!(uri);
    }
    match request_checked(&mut client, "lint", Some(params)) {
        Ok(result) => {
            let diagnostic_count = match lint_diagnostic_count(&result, all) {
                Ok(count) => count,
                Err(error) => return report_error(&error, json),
            };
            if json {
                print_json(&result);
                return if diagnostic_count > 0 {
                    ExitCode::FAILURE
                } else {
                    ExitCode::SUCCESS
                };
            }
            if all {
                if let Some(files) = list_rows(&result).as_array() {
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
                        }
                    }
                    eprintln!(
                        "\n{} diagnostics across {} files",
                        diagnostic_count,
                        files.len()
                    );
                }
            } else {
                let diagnostics = list_rows(&result).as_array().cloned().unwrap_or_default();
                if diagnostics.is_empty() {
                    eprintln!("No issues found");
                } else {
                    for d in &diagnostics {
                        print_lint_diag(file, d);
                    }
                    eprintln!("\n{} diagnostics", diagnostics.len());
                }
            }
            if diagnostic_count > 0 {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => report_error(&e, json),
    }
}

fn lint_diagnostic_count(result: &serde_json::Value, all: bool) -> Result<usize, String> {
    let files_or_diagnostics = result
        .as_array()
        .ok_or_else(|| "validated lint response was not an array".to_string())?;
    if !all {
        return Ok(files_or_diagnostics.len());
    }
    files_or_diagnostics
        .iter()
        .enumerate()
        .try_fold(0usize, |total, (index, file)| {
            let diagnostics = file
                .get("diagnostics")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| {
                    format!("validated lint response file {index} has no diagnostics array")
                })?;
            Ok(total + diagnostics.len())
        })
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
    let uri = match file_to_uri(file) {
        Ok(uri) => uri,
        Err(error) => return report_error(&error, json),
    };
    let params = serde_json::json!({ "check": check, "uri": uri });
    match request_checked(&mut client, "format", Some(params)) {
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
    match request_checked(&mut client, "format", Some(params)) {
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
    let root = match project_root(None) {
        Ok(root) => root,
        Err(error) => return report_error(&error, json),
    };
    let al_files = match collect_al_files(&root) {
        Ok(files) => files,
        Err(error) => return report_error(&error, json),
    };
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let mut changed_count = 0;
    let mut error_count = 0;
    let mut total = 0;
    for path in &al_files {
        total += 1;
        let uri = match url::Url::from_file_path(path) {
            Ok(uri) => uri.to_string(),
            Err(()) => {
                error_count += 1;
                eprintln!(
                    "Error formatting {}: path cannot be represented as a file URI",
                    path.display()
                );
                continue;
            }
        };
        let params = serde_json::json!({ "uri": uri, "check": check });
        match request_checked(&mut client, "format", Some(params)) {
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
    let uri = match file_to_uri(file) {
        Ok(uri) => uri,
        Err(error) => return report_error(&error, json),
    };
    let params = serde_json::json!({
        "uri": uri,
        "line": line.saturating_sub(1),
        "character": col.saturating_sub(1),
    });
    match request_checked(&mut client, "hover", Some(params)) {
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
    let uri = match file_to_uri(file) {
        Ok(uri) => uri,
        Err(error) => return report_error(&error, json),
    };
    let params = serde_json::json!({
        "uri": uri,
        "line": line.saturating_sub(1),
        "character": col.saturating_sub(1),
    });
    match request_checked(&mut client, method, Some(params)) {
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
            if let Some(locations) = list_rows(result).as_array() {
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
            if let Some(locations) = list_rows(result).as_array() {
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
        "completions" => {
            let items = list_rows(result)
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            for item in items {
                let label = item["label"].as_str().unwrap_or("?");
                match item["detail"].as_str().filter(|detail| !detail.is_empty()) {
                    Some(detail) => println!("{label}  {detail}"),
                    None => println!("{label}"),
                }
            }
            eprintln!("\n{} completion(s)", items.len());
        }
        "signatureHelp" => print_signature_help(result),
        _ => {
            println!("Result:");
            println!(
                "{}",
                serde_json::to_string_pretty(result).unwrap_or_default()
            );
        }
    }
    print_page_footer(result);
}

/// Each signature on its own line, the active one marked and its active
/// parameter in brackets: `> SetTier(var Cust: Record Customer; [Tier: Code[10]])`.
fn print_signature_help(result: &serde_json::Value) {
    let active_signature = result["activeSignature"].as_u64().unwrap_or(0);
    let signatures = result["signatures"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    for (index, signature) in signatures.iter().enumerate() {
        let mut label = signature["label"].as_str().unwrap_or("?").to_string();
        let active_parameter = signature["activeParameter"]
            .as_u64()
            .or_else(|| result["activeParameter"].as_u64());
        let parameter = active_parameter
            .and_then(|active| signature["parameters"].get(active as usize))
            .and_then(|parameter| parameter["label"].as_str());
        if let Some(parameter) = parameter
            && let Some(start) = label.find(parameter)
        {
            label.replace_range(start..start + parameter.len(), &format!("[{parameter}]"));
        }
        let marker = if index as u64 == active_signature {
            ">"
        } else {
            " "
        };
        println!("{marker} {label}");
    }
}

pub fn cmd_symbols(file: &str, json: bool) -> ExitCode {
    if json {
        return cmd_file_query("documentSymbols", file, json);
    }
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let uri = match file_to_uri(file) {
        Ok(uri) => uri,
        Err(error) => return report_error(&error, json),
    };
    match request_checked(
        &mut client,
        "documentSymbols",
        Some(serde_json::json!({ "uri": uri })),
    ) {
        Ok(result) => {
            let outline = render_outline(list_rows(&result));
            if outline.is_empty() {
                eprintln!("No symbols in {file}");
            } else {
                print!("{outline}");
                print_page_footer(&result);
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

/// Document symbols as an indented outline, one symbol per line with its
/// 1-based line: `Method SetTier(var Cust: Record Customer)  :3`.
fn render_outline(symbols: &serde_json::Value) -> String {
    fn walk(symbols: &serde_json::Value, depth: usize, out: &mut String) {
        for symbol in symbols.as_array().into_iter().flatten() {
            let kind = symbol["kind"].as_str().unwrap_or("Symbol");
            let name = symbol["name"].as_str().unwrap_or("?");
            let detail = symbol["detail"].as_str().unwrap_or("");
            let line = symbol["range"]["start"]["line"]
                .as_u64()
                .map_or(0, |l| l + 1);
            let separator = if detail.starts_with('(') || detail.is_empty() {
                ""
            } else {
                " "
            };
            out.push_str(&format!(
                "{:indent$}{kind} {name}{separator}{detail}  :{line}\n",
                "",
                indent = depth * 2
            ));
            walk(&symbol["children"], depth + 1, out);
        }
    }
    let mut out = String::new();
    walk(symbols, 0, &mut out);
    out
}

pub fn cmd_folding(file: &str, json: bool) -> ExitCode {
    if json {
        return cmd_file_query("foldingRanges", file, json);
    }
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let uri = match file_to_uri(file) {
        Ok(uri) => uri,
        Err(error) => return report_error(&error, json),
    };
    match request_checked(
        &mut client,
        "foldingRanges",
        Some(serde_json::json!({ "uri": uri })),
    ) {
        Ok(result) => {
            let ranges = list_rows(&result)
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            if ranges.is_empty() {
                eprintln!("No folding ranges in {file}");
            } else {
                // 1-based, as the command's input and every other text output.
                for range in ranges {
                    let start = range["startLine"].as_u64().map_or(0, |line| line + 1);
                    let end = range["endLine"].as_u64().map_or(0, |line| line + 1);
                    let kind = range["kind"].as_str().unwrap_or("region");
                    println!("{start}-{end}  {kind}");
                }
                print_page_footer(&result);
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

pub fn cmd_tokens(file: &str, json: bool) -> ExitCode {
    cmd_file_query("semanticTokens", file, json)
}

fn cmd_file_query(method: &str, file: &str, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let uri = match file_to_uri(file) {
        Ok(uri) => uri,
        Err(error) => return report_error(&error, json),
    };
    let params = serde_json::json!({ "uri": uri });
    match request_checked(&mut client, method, Some(params)) {
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
    let uri = match file_to_uri(file) {
        Ok(uri) => uri,
        Err(error) => return report_error(&error, json),
    };
    let params = serde_json::json!({
        "uri": uri,
        "line": line.saturating_sub(1),
        "character": col.saturating_sub(1),
        "newName": new_name,
    });
    match request_checked(&mut client, "rename", Some(params)) {
        Ok(result) => {
            let exit_code = rename_exit_code(&result);
            if json {
                print_json(&result);
            }
            if exit_code == ExitCode::FAILURE {
                if !json {
                    eprintln!("Cannot rename symbol at {file}:{line}:{col}");
                }
                return exit_code;
            }
            if json {
                return exit_code;
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

fn rename_exit_code(result: &serde_json::Value) -> ExitCode {
    if result.is_null() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Apply a WorkspaceEdit `changes` map to disk.
///
/// Each key is a `file://` URI and each value is an array of LSP TextEdit
/// objects (`{ range: { start, end }, newText }`).  Edits are applied in
/// reverse position order so that later offsets are not invalidated by earlier
/// mutations.  Returns the number of files that were written.
///
/// The whole edit is all-or-nothing. Every file's new content is computed
/// first, then written; a rename touching five files whose third is read-only
/// used to leave the first two rewritten, exit 1, and hand the user a
/// workspace carrying both the old and the new name.
fn apply_workspace_edit(
    changes: &serde_json::Map<String, serde_json::Value>,
) -> Result<usize, String> {
    let mut staged: Vec<(std::path::PathBuf, String)> = Vec::new();

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
            staged.push((path, new_content));
        }
    }

    write_all_or_nothing(staged)
}

/// Write every staged `(path, content)` pair, or none of them.
///
/// Each file is written to a temp file in its own directory and persisted over
/// the original, so a crash mid-write cannot truncate a source file. If any
/// write fails, the files already persisted are restored from the contents read
/// before the first write.
fn write_all_or_nothing(staged: Vec<(std::path::PathBuf, String)>) -> Result<usize, String> {
    let mut rollback: Vec<(std::path::PathBuf, String)> = Vec::with_capacity(staged.len());
    for (path, new_content) in &staged {
        let previous = std::fs::read_to_string(path)
            .map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
        match persist_atomically(path, new_content) {
            Ok(()) => rollback.push((path.clone(), previous)),
            Err(error) => {
                let mut report = error;
                for (done, content) in rollback.iter().rev() {
                    if let Err(error) = persist_atomically(done, content) {
                        report.push_str(&format!(
                            "\nand {} could not be rolled back: {error}",
                            done.display()
                        ));
                    }
                }
                return Err(report);
            }
        }
    }
    Ok(staged.len())
}

/// Replace `path`'s contents through a temp file in the same directory.
fn persist_atomically(path: &std::path::Path, content: &str) -> Result<(), String> {
    use std::io::Write;

    let directory = path.parent().unwrap_or(std::path::Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(directory)
        .map_err(|error| format!("Cannot stage {}: {error}", path.display()))?;
    temp.write_all(content.as_bytes())
        .and_then(|()| temp.as_file().sync_all())
        .map_err(|error| format!("Cannot write {}: {error}", path.display()))?;
    // `NamedTempFile` is 0600; a source file must keep the mode it had.
    if let Ok(metadata) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(temp.path(), metadata.permissions());
    }
    temp.persist(path)
        .map_err(|error| format!("Cannot replace {}: {error}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod exit_status_tests {
    use super::*;

    /// A `changes` map renaming `old` to `new` on line 0 of each file.
    fn rename_edit(paths: &[&std::path::Path]) -> serde_json::Map<String, serde_json::Value> {
        paths
            .iter()
            .map(|path| {
                (
                    url::Url::from_file_path(path).unwrap().to_string(),
                    serde_json::json!([{
                        "range": {
                            "start": { "line": 0, "character": 0 },
                            "end": { "line": 0, "character": 3 },
                        },
                        "newText": "new",
                    }]),
                )
            })
            .collect()
    }

    #[test]
    #[cfg(unix)]
    fn a_failed_file_write_leaves_the_whole_workspace_unchanged() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        // "A.al" sorts before "locked/B.al", and `changes` is a BTreeMap, so
        // the writable file is written before the failure is hit.
        let writable = dir.path().join("A.al");
        let locked_dir = dir.path().join("locked");
        std::fs::create_dir(&locked_dir).unwrap();
        let unwritable = locked_dir.join("B.al");
        std::fs::write(&writable, "old text\n").unwrap();
        std::fs::write(&unwritable, "old text\n").unwrap();
        std::fs::set_permissions(&locked_dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        let result = apply_workspace_edit(&rename_edit(&[&writable, &unwritable]));

        let writable_after = std::fs::read_to_string(&writable).unwrap();
        let unwritable_after = std::fs::read_to_string(&unwritable).unwrap();
        std::fs::set_permissions(&locked_dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        let error = result.expect_err("an unwritable directory must fail the edit");
        assert!(error.contains("B.al"), "{error}");
        assert_eq!(
            writable_after, "old text\n",
            "the file written before the failure must be rolled back"
        );
        assert_eq!(unwritable_after, "old text\n");
    }

    #[test]
    fn a_successful_edit_writes_every_file_and_keeps_its_mode() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("A.al");
        let second = dir.path().join("B.al");
        std::fs::write(&first, "old text\n").unwrap();
        std::fs::write(&second, "old text\n").unwrap();
        let before = std::fs::metadata(&first).unwrap().permissions();

        let changed = apply_workspace_edit(&rename_edit(&[&first, &second])).unwrap();

        assert_eq!(changed, 2);
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "new text\n");
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "new text\n");
        assert_eq!(std::fs::metadata(&first).unwrap().permissions(), before);
    }

    #[test]
    fn lint_counts_findings_in_single_and_all_responses() {
        assert_eq!(
            lint_diagnostic_count(&serde_json::json!([{}, {}]), false).unwrap(),
            2
        );
        assert_eq!(
            lint_diagnostic_count(
                &serde_json::json!([
                    {"file": "a.al", "diagnostics": [{}]},
                    {"file": "b.al", "diagnostics": [{}, {}]}
                ]),
                true
            )
            .unwrap(),
            3
        );
        assert!(lint_diagnostic_count(&serde_json::json!({}), false).is_err());
    }

    #[test]
    fn unresolved_rename_is_a_failure_in_every_output_mode() {
        assert_eq!(
            rename_exit_code(&serde_json::Value::Null),
            ExitCode::FAILURE
        );
        assert_eq!(
            rename_exit_code(&serde_json::json!({"changes": {}})),
            ExitCode::SUCCESS
        );
    }
}

#[cfg(test)]
mod outline_tests {
    use super::render_outline;

    #[test]
    fn outline_indents_children_and_shows_one_based_lines() {
        let symbols = serde_json::json!([{
            "name": "Loyalty Mgt",
            "detail": "codeunit 50101",
            "kind": "Class",
            "range": { "start": { "line": 0, "character": 0 } },
            "children": [{
                "name": "SetTier",
                "detail": "(var Cust: Record Customer)",
                "kind": "Method",
                "range": { "start": { "line": 2, "character": 4 } }
            }]
        }]);
        assert_eq!(
            render_outline(&symbols),
            "Class Loyalty Mgt codeunit 50101  :1\n  Method SetTier(var Cust: Record Customer)  :3\n"
        );
    }
}
