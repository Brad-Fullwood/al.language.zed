//! Generates the AL grammar, scanner, queries, and language metadata from the
//! TextMate grammar distributed with Microsoft's AL extension.

use anyhow::{Context, Result, anyhow};
use regex::Regex;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

mod zed_language;

const REPO_TEST_CONFIG: &str = "tests/test_repos.toml";
const REPO_TEST_WORKDIR: &str = "tests/.repos";
const FIXTURES_INVALID_DIR: &str = "tests/fixtures/invalid";
const FIXTURES_VALID_DIR: &str = "tests/fixtures/valid";

#[derive(Default)]
struct Options {
    repo_tests: bool,
    zed_language_only: bool,
}

#[derive(Debug, PartialEq)]
struct ProjectRoots {
    tree_sitter: PathBuf,
    extension: PathBuf,
}

fn project_roots(manifest_dir: &Path) -> Result<ProjectRoots> {
    let tree_sitter = manifest_dir
        .parent()
        .context("generator manifest dir has no parent")?
        .to_path_buf();
    let extension = tree_sitter
        .parent()
        .context("tree-sitter-al directory has no parent")?
        .to_path_buf();

    Ok(ProjectRoots {
        tree_sitter,
        extension,
    })
}

fn parse_options() -> Result<Options> {
    let mut options = Options::default();
    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            "--test" | "--tests" => options.repo_tests = true,
            "--zed-language-only" => options.zed_language_only = true,
            _ => anyhow::bail!("Unknown argument: {argument}"),
        }
    }
    Ok(options)
}

fn main() -> Result<()> {
    let options = parse_options()?;
    let generator_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let roots = project_roots(&generator_dir)?;
    let tree_sitter_root = roots.tree_sitter;
    let extension_root = roots.extension;

    if options.zed_language_only {
        zed_language::generate(&extension_root, &tree_sitter_root.join("queries"))?;
        return Ok(());
    }

    println!("Finding the installed AL extension");
    let extension_path = find_al_extension()?;
    println!("Extension: {}", extension_path.display());

    let syntax_file = find_syntax_file(&extension_path)?;
    println!("TextMate grammar: {}", syntax_file.display());

    println!("Extracting keywords");
    let keywords = extract_keywords(&syntax_file)?;
    let scope_name = extract_scope_name(&syntax_file)?;
    print_keyword_stats(&keywords);

    println!("Generating scanner sources");
    let external_tokens = build_external_tokens(&keywords);

    let src_dir = tree_sitter_root.join("src");
    fs::create_dir_all(&src_dir)?;

    generate_keywords_c(&keywords, path_str(&src_dir)?)?;
    generate_scanner_c(&keywords, &external_tokens, path_str(&src_dir)?)?;
    println!("Generated src/keywords.c and src/scanner.c");

    println!("Generating grammar.js");
    generate_grammar_js(&keywords, &external_tokens, path_str(&tree_sitter_root)?)?;
    println!("Generated grammar.js");

    let queries_dir = tree_sitter_root.join("queries");
    fs::create_dir_all(&queries_dir)?;

    println!("Generating highlight queries");
    let highlights_path = queries_dir.join("highlights.scm");
    generate_highlights(&keywords, path_str(&highlights_path)?)?;
    println!("Generated {}", highlights_path.display());

    println!("Generating Zed themes");
    generate_themes(&extension_path, &extension_root.join("themes"))?;

    println!("Generating language data");
    let data_dir = tree_sitter_root.join("data");
    write_data_files(&keywords, path_str(&data_dir)?)?;
    println!("Generated {}/", data_dir.display());

    println!("Generating the parser");
    let tree_sitter_root_str = path_str(&tree_sitter_root)?;
    run_tree_sitter_generate(tree_sitter_root_str)?;

    println!("Generating structural queries");
    let node_types_path = tree_sitter_root.join("src/node-types.json");
    generate_structural_queries(path_str(&node_types_path)?, path_str(&queries_dir)?)?;

    println!("Generating Zed language files");
    zed_language::generate(&extension_root, &tree_sitter_root.join("queries"))?;

    println!("Building the parser library");
    let lib_path = run_tree_sitter_build(tree_sitter_root_str)?;
    println!("Parser library: {}", lib_path.display());

    run_fixture_tests(&lib_path, &scope_name, tree_sitter_root_str)?;

    if options.repo_tests {
        let repo_test_config_path = tree_sitter_root.join(REPO_TEST_CONFIG);
        if !repo_test_config_path.is_file() {
            anyhow::bail!(
                "repository test configuration not found: {}",
                repo_test_config_path.display()
            );
        }
        println!("Testing configured repositories");
        run_repo_tests(&lib_path, &scope_name, tree_sitter_root_str)?;
    } else {
        println!("Repository tests skipped; pass --test to run them");
    }

    println!("Generation and validation completed");

    Ok(())
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .with_context(|| format!("path is not valid UTF-8: {}", path.display()))
}

#[derive(Debug, Default)]
struct Keywords {
    control: BTreeSet<String>,
    operator_words: BTreeSet<String>,
    objects: BTreeSet<String>,
    types: BTreeSet<String>,
    metadata: BTreeSet<String>,
    properties: BTreeSet<String>,
    scope_captures: std::collections::HashMap<String, String>,
}

/// Maps a TextMate scope to the closest standard tree-sitter capture.
fn textmate_to_treesitter_capture(scope: &str) -> String {
    let parts: Vec<&str> = scope.split('.').collect();
    if parts.is_empty() {
        return "@keyword".to_string();
    }

    let category = parts[0];

    match category {
        "keyword" => {
            if parts.len() > 1 && (parts[1] == "operator" || parts[1] == "operators") {
                "@operator".to_string()
            } else if parts.len() > 2 && parts[1] == "other" {
                match parts[2] {
                    "builtintypes" | "type" => "@type.builtin".to_string(),
                    "applicationobject" | "storage" => "@keyword".to_string(),
                    _ => "@keyword".to_string(),
                }
            } else {
                "@keyword".to_string()
            }
        }
        "entity" => {
            if parts.len() > 2 {
                match parts[2] {
                    "function" | "method" => "@function".to_string(),
                    "type" | "class" | "struct" | "enum" | "interface" => "@type".to_string(),
                    "tag" => "@tag".to_string(),
                    "section" => "@title".to_string(),
                    "applicationobject" => "@keyword".to_string(),
                    _ => format!("@{}", parts[2]),
                }
            } else if parts.len() > 1 && parts[1] == "name" {
                "@function".to_string()
            } else {
                "@variable".to_string()
            }
        }
        "constant" => {
            if parts.len() > 1 {
                match parts[1] {
                    "numeric" => "@number".to_string(),
                    "character" => "@character".to_string(),
                    "language" => "@constant.builtin".to_string(),
                    _ => "@constant".to_string(),
                }
            } else {
                "@constant".to_string()
            }
        }
        "string" => "@string".to_string(),
        "comment" => "@comment".to_string(),
        "identifier" => "@variable".to_string(),
        "variable" => {
            if parts.len() > 1 {
                match parts[1] {
                    "parameter" => "@variable.parameter".to_string(),
                    "language" => "@variable.builtin".to_string(),
                    _ => "@variable".to_string(),
                }
            } else {
                "@variable".to_string()
            }
        }
        "storage" => {
            if parts.len() > 1 && parts[1] == "type" {
                "@type".to_string()
            } else {
                "@keyword".to_string()
            }
        }
        "support" => {
            if parts.len() > 1 {
                match parts[1] {
                    "function" => "@function.builtin".to_string(),
                    "class" | "type" => "@type.builtin".to_string(),
                    "variable" => "@variable.builtin".to_string(),
                    "constant" => "@constant.builtin".to_string(),
                    _ => "@keyword".to_string(),
                }
            } else {
                "@keyword".to_string()
            }
        }
        "punctuation" => {
            if parts.len() > 1 {
                match parts[1] {
                    "bracket" => "@punctuation.bracket".to_string(),
                    "delimiter" | "separator" | "terminator" => {
                        "@punctuation.delimiter".to_string()
                    }
                    _ => "@punctuation".to_string(),
                }
            } else {
                "@punctuation".to_string()
            }
        }
        "meta" => "@keyword".to_string(),
        "markup" => {
            if parts.len() > 1 {
                match parts[1] {
                    "heading" => "@title".to_string(),
                    "bold" => "@text.strong".to_string(),
                    "italic" => "@text.emphasis".to_string(),
                    "underline" => "@text.underline".to_string(),
                    "raw" | "inline" => "@text.literal".to_string(),
                    "link" => "@text.uri".to_string(),
                    _ => "@text".to_string(),
                }
            } else {
                "@text".to_string()
            }
        }
        "invalid" => "@error".to_string(),
        _ => format!("@{}", category),
    }
}

#[derive(Clone, Debug)]
struct ExternalTokenSpec {
    grammar: String,
    c_enum: String,
}

fn token_spec(grammar: impl Into<String>, c_enum: impl Into<String>) -> ExternalTokenSpec {
    ExternalTokenSpec {
        grammar: grammar.into(),
        c_enum: c_enum.into(),
    }
}

fn kw_token_spec(prefix: &str, kw: &str, c_prefix: &str) -> ExternalTokenSpec {
    token_spec(
        format!("{prefix}_{kw}"),
        format!("{c_prefix}_{}", kw.to_ascii_uppercase()),
    )
}

// This order must match grammar.js externals and the scanner's TokenType enum.
fn build_external_tokens(keywords: &Keywords) -> Vec<ExternalTokenSpec> {
    let mut out = Vec::new();

    for kw in &keywords.control {
        out.push(kw_token_spec("kw", kw, "KW"));
    }

    for kw in &keywords.operator_words {
        out.push(kw_token_spec("op", kw, "OP"));
    }

    out.push(token_spec("keyword", "KEYWORD"));
    out.push(token_spec("control_keyword", "CONTROL_KEYWORD"));
    out.push(token_spec("operator_word", "OPERATOR_WORD"));
    out.push(token_spec("object_keyword", "OBJECT_KEYWORD"));
    out.push(token_spec("type_keyword", "TYPE_KEYWORD"));
    out.push(token_spec("metadata_keyword", "METADATA_KEYWORD"));
    out.push(token_spec("property_keyword", "PROPERTY_KEYWORD"));

    out.push(token_spec("directive", "DIRECTIVE"));
    out.push(token_spec("inactive_code", "INACTIVE_CODE"));

    out
}

fn render_template(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (key, value) in vars {
        let needle = format!("{{{{{}}}}}", key);
        let needle_spaced = format!("{{{{ {} }}}}", key);
        out = out.replace(&needle, value);
        out = out.replace(&needle_spaced, value);
    }
    out
}

fn gen_scanner_token_enum_fragment(tokens: &[ExternalTokenSpec]) -> String {
    let mut out = String::new();
    for t in tokens {
        out.push_str("  ");
        out.push_str(&t.c_enum);
        out.push_str(",\n");
    }
    out
}

fn gen_grammar_externals_fragment(tokens: &[ExternalTokenSpec]) -> String {
    let mut out = String::new();
    for t in tokens {
        out.push_str("    $.");
        out.push_str(&t.grammar);
        out.push_str(",\n");
    }
    out
}

fn gen_scanner_valid_symbols_fastpath_fragment(tokens: &[ExternalTokenSpec]) -> String {
    let mut out = String::new();
    out.push_str("  if (");
    for (i, t) in tokens.iter().enumerate() {
        if i > 0 {
            out.push_str(" &&\n      ");
        }
        out.push_str("!valid_symbols[");
        out.push_str(&t.c_enum);
        out.push(']');
    }
    out.push_str(") {\n    return false;\n  }\n");
    out
}

fn gen_scanner_directive_handling_fragment() -> String {
    r#"  if (scanner_scan_directive(scanner, lexer, valid_symbols)) {
    return true;
  }
  if (scanner_scan_inactive_code(scanner, lexer, valid_symbols)) {
    return true;
  }
"#
    .to_string()
}

fn gen_scanner_keyword_dispatch_fragment() -> String {
    // Dispatch order is important for overlap cases; keep most-specific first.
    r#"  TokenType specific;
  if (al_lookup_control_kw_token(word, &specific) && valid_symbols[specific]) {
    lexer->result_symbol = specific;
    return true;
  }

  if (al_lookup_operator_word_token(word, &specific) && valid_symbols[specific]) {
    lexer->result_symbol = specific;
    return true;
  }

  if (valid_symbols[CONTROL_KEYWORD] && is_al_control_keyword(word)) {
    lexer->result_symbol = CONTROL_KEYWORD;
    return true;
  }
  if (valid_symbols[OPERATOR_WORD] && is_al_operator_word_keyword(word)) {
    lexer->result_symbol = OPERATOR_WORD;
    return true;
  }
  if (valid_symbols[OBJECT_KEYWORD] && is_al_object_keyword(word)) {
    lexer->result_symbol = OBJECT_KEYWORD;
    return true;
  }
  if (valid_symbols[TYPE_KEYWORD] && is_al_type_keyword(word)) {
    lexer->result_symbol = TYPE_KEYWORD;
    return true;
  }
  if (valid_symbols[METADATA_KEYWORD] && is_al_metadata_keyword(word)) {
    lexer->result_symbol = METADATA_KEYWORD;
    return true;
  }
  if (valid_symbols[PROPERTY_KEYWORD] && is_al_property_keyword(word)) {
    lexer->result_symbol = PROPERTY_KEYWORD;
    return true;
  }
  if (valid_symbols[KEYWORD] && is_al_keyword(word)) {
    lexer->result_symbol = KEYWORD;
    return true;
  }
"#
    .to_string()
}

fn find_al_extension() -> Result<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .context("Cannot find home directory")?;

    let search_paths = vec![
        PathBuf::from(&home).join(".cursor/extensions"),
        PathBuf::from(&home).join(".vscode/extensions"),
    ];

    for search_path in search_paths {
        if !search_path.exists() {
            continue;
        }

        {
            let entries = fs::read_dir(&search_path)
                .with_context(|| format!("Failed to read {}", search_path.display()))?;
            let mut al_extensions: Vec<_> = entries
                .collect::<std::io::Result<Vec<_>>>()?
                .into_iter()
                .map(|e| e.path())
                .filter_map(|path| al_extension_version_key(&path).map(|version| (version, path)))
                .collect();

            al_extensions.sort_by(|a, b| a.0.cmp(&b.0));
            if let Some((_, latest)) = al_extensions.last() {
                return Ok(latest.to_path_buf());
            }
        }
    }

    anyhow::bail!("AL extension not found")
}

fn al_extension_version_key(path: &Path) -> Option<Vec<u64>> {
    let name = path.file_name()?.to_str()?;
    let prefix = "ms-dynamics-smb.al-";
    let version = name.strip_prefix(prefix)?;
    let parts = version
        .split('.')
        .map(str::parse)
        .collect::<std::result::Result<Vec<u64>, _>>()
        .ok()?;
    (!parts.is_empty()).then_some(parts)
}

fn find_syntax_file(extension_path: &Path) -> Result<PathBuf> {
    let direct = extension_path.join("syntaxes/alsyntax.tmlanguage");
    if direct.exists() {
        return Ok(direct);
    }

    let syntaxes_dir = extension_path.join("syntaxes");
    if !syntaxes_dir.is_dir() {
        anyhow::bail!(
            "No syntaxes directory in extension: {}",
            extension_path.display()
        );
    }

    let mut candidates = Vec::new();
    for entry in fs::read_dir(&syntaxes_dir).context("Failed to read syntaxes directory")? {
        let path = entry?.path();
        if path.extension().and_then(|s| s.to_str()) == Some("tmlanguage") {
            candidates.push(path);
        }
    }

    candidates.sort();
    candidates
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("No .tmlanguage files found in {}", syntaxes_dir.display()))
}

fn extract_keywords(syntax_file: &Path) -> Result<Keywords> {
    let xml = read_encoding_aware(syntax_file)?;
    let mut out = Keywords::default();

    let kw_list_re = Regex::new(r#"(?i)\(\?i:\((.*?)\)\)"#)?;
    let name_re = Regex::new(r#"(?s)<key>name</key>\s*<string>([^<]+)</string>"#)?;

    let single_kw_re =
        Regex::new(r#"(?i)<key>match</key>\s*<string>\\b\(\?i:([a-zA-Z0-9_]+)\)\\b</string>"#)?;

    for mat in kw_list_re.find_iter(&xml) {
        let kw_list_str = &xml[mat.start()..mat.end()];

        let start_search = mat.start().saturating_sub(1000);
        let end_search = (mat.end() + 1000).min(xml.len());
        let search_window = &xml[start_search..end_search];

        let mut scope_name = None;
        let mut best_dist = usize::MAX;
        for name_caps in name_re.captures_iter(search_window) {
            let name_mat = name_caps.get(0).unwrap();
            let dist = if name_mat.start() + start_search < mat.start() {
                mat.start() - (name_mat.end() + start_search)
            } else {
                (name_mat.start() + start_search) - mat.end()
            };
            if dist < best_dist {
                best_dist = dist;
                scope_name = name_caps.get(1).map(|value| value.as_str());
            }
        }
        let scope_name = scope_name.ok_or_else(|| {
            anyhow!(
                "No TextMate scope found near keyword expression at byte {}",
                mat.start()
            )
        })?;

        if let Some(kw_caps) = kw_list_re.captures(kw_list_str) {
            for raw in kw_caps[1].split('|') {
                let kw = raw.trim().to_lowercase();
                if !is_identifier_like(&kw) {
                    continue;
                }

                let capture = textmate_to_treesitter_capture(scope_name);
                out.scope_captures
                    .insert(scope_name.to_string(), capture.to_string());

                if scope_name.contains("keyword.control") {
                    out.control.insert(kw);
                } else if scope_name.contains("keyword.operators.al") {
                    out.operator_words.insert(kw);
                } else if scope_name.contains("applicationobject") {
                    out.objects.insert(kw.clone());
                    out.control.insert(kw);
                } else if scope_name.contains("builtintypes") {
                    out.types.insert(kw.clone());
                    out.control.insert(kw);
                } else if scope_name.contains("metadata") {
                    out.metadata.insert(kw);
                } else if scope_name.contains("property")
                    || scope_name.contains("variable.other")
                    || scope_name.contains("support.variable")
                {
                    out.properties.insert(kw);
                }
            }
        }
    }

    for caps in single_kw_re.captures_iter(&xml) {
        let kw = caps[1].trim().to_lowercase();
        if is_identifier_like(&kw) {
            out.control.insert(kw);
        }
    }

    for name_caps in name_re.captures_iter(&xml) {
        let scope_name = name_caps.get(1).unwrap().as_str();
        if !scope_name.starts_with("punctuation.whitespace")
            && !scope_name.starts_with("source.")
            && !out.scope_captures.contains_key(scope_name)
        {
            let capture = textmate_to_treesitter_capture(scope_name);
            out.scope_captures.insert(scope_name.to_string(), capture);
        }
    }

    println!("TextMate capture mappings:");
    for (scope, capture) in &out.scope_captures {
        println!("  {scope} -> {capture}");
    }

    Ok(out)
}

fn read_encoding_aware(path: &Path) -> Result<String> {
    let bytes = fs::read(path)?;
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        let utf16: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16(&utf16).context("Failed to decode UTF-16LE")
    } else if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        let utf16: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16(&utf16).context("Failed to decode UTF-16BE")
    } else {
        String::from_utf8(bytes).context("Failed to decode UTF-8")
    }
}

fn extract_scope_name(syntax_file: &Path) -> Result<String> {
    let xml = read_encoding_aware(syntax_file)?;
    let re = Regex::new(r#"<key>scopeName</key>\s*<string>([^<]+)</string>"#)?;
    let caps = re
        .captures(&xml)
        .ok_or_else(|| anyhow!("scopeName not found in {}", syntax_file.display()))?;
    Ok(caps[1].trim().to_string())
}

fn is_identifier_like(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn print_keyword_stats(keywords: &Keywords) {
    let total = keywords_all(keywords).len();
    println!("   Control:   {}", keywords.control.len());
    println!("   Operators: {}", keywords.operator_words.len());
    println!("   Objects:   {}", keywords.objects.len());
    println!("   Types:     {}", keywords.types.len());
    println!("   Metadata:  {}", keywords.metadata.len());
    println!("   Property:  {}", keywords.properties.len());
    println!("   Total:     {}", total);
}

fn keywords_all(k: &Keywords) -> BTreeSet<String> {
    let mut all = BTreeSet::new();
    all.extend(k.control.iter().cloned());
    all.extend(k.operator_words.iter().cloned());
    all.extend(k.objects.iter().cloned());
    all.extend(k.types.iter().cloned());
    all.extend(k.metadata.iter().cloned());
    all.extend(k.properties.iter().cloned());
    all
}

fn write_data_files(keywords: &Keywords, data_dir: &str) -> Result<()> {
    fs::create_dir_all(data_dir)?;

    write_keywords_json(keywords, data_dir)?;
    println!("   keywords.json");

    write_object_types_json(keywords, data_dir)?;
    println!("   object_types.json");

    write_page_controls_json(data_dir)?;
    println!("   page_controls.json");

    write_token_classification_json(keywords, data_dir)?;
    println!("   token_classification.json");

    Ok(())
}

fn write_keywords_json(keywords: &Keywords, data_dir: &str) -> Result<()> {
    fn to_entries(set: &BTreeSet<String>, prefix: &str) -> Vec<serde_json::Value> {
        set.iter()
            .map(|kw| {
                serde_json::json!({
                    "keyword": kw,
                    "node_kind": format!("{}_{}", prefix, kw)
                })
            })
            .collect()
    }

    let obj = serde_json::json!({
        "control":   to_entries(&keywords.control, "kw"),
        "operator":  to_entries(&keywords.operator_words, "op"),
        "object":    to_entries(&keywords.objects, "kw"),
        "type":      to_entries(&keywords.types, "kw"),
        "metadata":  to_entries(&keywords.metadata, "kw"),
        "property":  to_entries(&keywords.properties, "kw"),
    });

    let path = format!("{}/keywords.json", data_dir);
    fs::write(path, serde_json::to_string_pretty(&obj)?)?;
    Ok(())
}

fn write_object_types_json(keywords: &Keywords, data_dir: &str) -> Result<()> {
    let extensions_map: &[(&str, &[&str])] = &[
        ("table", &["tableextension"]),
        ("page", &["pageextension", "pagecustomization"]),
        ("report", &["reportextension"]),
        ("enum", &["enumextension"]),
        ("permissionset", &["permissionsetextension"]),
        ("profile", &["profileextension"]),
    ];

    let lsp_kind_map: &[(&str, &str)] = &[
        ("codeunit", "Class"),
        ("controladdin", "Module"),
        ("dotnet", "Class"),
        ("entitlement", "Class"),
        ("enum", "Enum"),
        ("enumextension", "Class"),
        ("interface", "Interface"),
        ("page", "Class"),
        ("pagecustomization", "Class"),
        ("pageextension", "Class"),
        ("permissionset", "Class"),
        ("permissionsetextension", "Class"),
        ("profile", "File"),
        ("profileextension", "Class"),
        ("query", "Class"),
        ("report", "Class"),
        ("reportextension", "Class"),
        ("table", "Class"),
        ("tableextension", "Class"),
        ("value", "EnumMember"),
        ("xmlport", "Class"),
    ];
    let display_name_map: &[(&str, &str)] = &[
        ("codeunit", "Codeunit"),
        ("controladdin", "ControlAddIn"),
        ("dotnet", "DotNet"),
        ("entitlement", "Entitlement"),
        ("enum", "Enum"),
        ("enumextension", "EnumExtension"),
        ("interface", "Interface"),
        ("page", "Page"),
        ("pagecustomization", "PageCustomization"),
        ("pageextension", "PageExtension"),
        ("permissionset", "PermissionSet"),
        ("permissionsetextension", "PermissionSetExtension"),
        ("profile", "Profile"),
        ("profileextension", "ProfileExtension"),
        ("query", "Query"),
        ("report", "Report"),
        ("reportextension", "ReportExtension"),
        ("table", "Table"),
        ("tableextension", "TableExtension"),
        ("value", "Value"),
        ("xmlport", "XmlPort"),
    ];
    let permission_map: &[(&str, &str, &str)] = &[
        ("codeunit", "codeunit", "X"),
        ("page", "page", "X"),
        ("query", "query", "X"),
        ("report", "report", "X"),
        ("table", "tabledata", "RIMD"),
        ("xmlport", "xmlport", "X"),
    ];

    let ext_lookup: std::collections::HashMap<&str, Vec<String>> = extensions_map
        .iter()
        .map(|(k, v)| (*k, v.iter().map(|s| s.to_string()).collect()))
        .collect();

    let lsp_lookup: std::collections::HashMap<&str, &str> =
        lsp_kind_map.iter().map(|(k, v)| (*k, *v)).collect();
    let display_name_lookup: std::collections::HashMap<&str, &str> =
        display_name_map.iter().map(|(k, v)| (*k, *v)).collect();
    let permission_lookup: std::collections::HashMap<&str, (&str, &str)> = permission_map
        .iter()
        .map(|(keyword, object_type, value)| (*keyword, (*object_type, *value)))
        .collect();

    let entries: Result<Vec<serde_json::Value>> = keywords
        .objects
        .iter()
        .map(|kw| {
            let extensions: Vec<String> = ext_lookup.get(kw.as_str()).cloned().unwrap_or_default();
            let lsp_symbol_kind = lsp_lookup
                .get(kw.as_str())
                .copied()
                .with_context(|| format!("No LSP symbol kind configured for object type {kw}"))?;
            let display_name = display_name_lookup
                .get(kw.as_str())
                .copied()
                .with_context(|| format!("No display name configured for object type {kw}"))?;
            let permission = permission_lookup.get(kw.as_str()).copied();
            Ok(serde_json::json!({
                "keyword":         kw,
                "display_name":    display_name,
                "node_kind":       format!("kw_{}", kw),
                "extensions":      extensions,
                "lsp_symbol_kind": lsp_symbol_kind,
                "permission_type": permission.map(|(object_type, _)| object_type),
                "permission_value": permission.map(|(_, value)| value)
            }))
        })
        .collect();

    let obj = serde_json::json!({ "object_types": entries? });
    let path = format!("{}/object_types.json", data_dir);
    fs::write(path, format!("{}\n", serde_json::to_string_pretty(&obj)?))?;
    Ok(())
}

/// Writes page and report constructs needed independently of TextMate scopes.
fn write_page_controls_json(data_dir: &str) -> Result<()> {
    let controls: &[(&str, &str)] = &[
        ("area", "Struct"),
        ("group", "Struct"),
        ("repeater", "Struct"),
        ("field", "Field"),
        ("part", "Class"),
        ("action", "Event"),
        ("separator", "Event"),
        ("cuegroup", "Struct"),
        ("grid", "Struct"),
        ("fixed", "Struct"),
        ("usercontrol", "Class"),
        ("label", "Constant"),
        ("dataitem", "Struct"),
        ("column", "Field"),
        ("filter", "Field"),
        ("addfirst", "Namespace"),
        ("addlast", "Namespace"),
        ("addafter", "Namespace"),
        ("addbefore", "Namespace"),
        ("modify", "Namespace"),
        ("moveafter", "Namespace"),
        ("movebefore", "Namespace"),
        ("actionref", "Event"),
    ];

    let entries: Vec<_> = controls
        .iter()
        .map(|(keyword, lsp_symbol_kind)| {
            serde_json::json!({
                "keyword": keyword,
                "node_kind": format!("kw_{}", keyword),
                "lsp_symbol_kind": lsp_symbol_kind
            })
        })
        .collect();

    let obj = serde_json::json!({ "page_controls": entries });
    let path = format!("{}/page_controls.json", data_dir);
    fs::write(path, format!("{}\n", serde_json::to_string_pretty(&obj)?))?;
    Ok(())
}

fn write_token_classification_json(keywords: &Keywords, data_dir: &str) -> Result<()> {
    let extension_suffixes = ["extension", "customization"];

    let keyword_object: Vec<String> = keywords
        .objects
        .iter()
        .filter(|kw| !extension_suffixes.iter().any(|sfx| kw.ends_with(sfx)))
        .map(|kw| format!("kw_{}", kw))
        .collect();

    let keyword_object_extension: Vec<String> = keywords
        .objects
        .iter()
        .filter(|kw| extension_suffixes.iter().any(|sfx| kw.ends_with(sfx)))
        .map(|kw| format!("kw_{}", kw))
        .collect();

    let object_set: std::collections::HashSet<&str> =
        keywords.objects.iter().map(|s| s.as_str()).collect();
    let type_set: std::collections::HashSet<&str> =
        keywords.types.iter().map(|s| s.as_str()).collect();

    let keyword_control: Vec<String> = keywords
        .control
        .iter()
        .filter(|kw| !object_set.contains(kw.as_str()) && !type_set.contains(kw.as_str()))
        .map(|kw| format!("kw_{}", kw))
        .collect();

    let builtin_type: Vec<String> = keywords
        .types
        .iter()
        .map(|kw| format!("kw_{}", kw))
        .collect();

    let builtin_function: Vec<String> = Vec::new();

    let obj = serde_json::json!({
        "keyword_control":           keyword_control,
        "keyword_object":            keyword_object,
        "keyword_object_extension":  keyword_object_extension,
        "builtin_type":              builtin_type,
        "builtin_function":          builtin_function
    });

    let path = format!("{}/token_classification.json", data_dir);
    fs::write(path, serde_json::to_string_pretty(&obj)?)?;
    Ok(())
}

fn generate_keywords_c(keywords: &Keywords, out_dir: &str) -> Result<()> {
    let all = keywords_all(keywords);

    let mut out = String::new();
    out.push_str("// Keyword lookup tables for AL external scanner\n");
    out.push_str("// AUTO-GENERATED - DO NOT EDIT\n");
    out.push_str("// Generated by: cargo run\n\n");
    out.push_str("#include <stdbool.h>\n");
    out.push_str("#include <stddef.h>\n");
    out.push_str("#include <string.h>\n\n");

    out.push_str(
        "static bool al_kw_binsearch(const char *word, const char *const *arr, size_t count) {\n",
    );
    out.push_str("  size_t lo = 0;\n");
    out.push_str("  size_t hi = count;\n");
    out.push_str("  while (lo < hi) {\n");
    out.push_str("    size_t mid = lo + (hi - lo) / 2;\n");
    out.push_str("    int cmp = strcmp(word, arr[mid]);\n");
    out.push_str("    if (cmp == 0) return true;\n");
    out.push_str("    if (cmp < 0) hi = mid; else lo = mid + 1;\n");
    out.push_str("  }\n");
    out.push_str("  return false;\n");
    out.push_str("}\n\n");

    write_kw_array(&mut out, "AL_KEYWORDS_ALL", &all);
    write_kw_array(&mut out, "AL_KEYWORDS_CONTROL", &keywords.control);
    write_kw_array(
        &mut out,
        "AL_KEYWORDS_OPERATOR_WORDS",
        &keywords.operator_words,
    );
    write_kw_array(&mut out, "AL_KEYWORDS_OBJECTS", &keywords.objects);
    write_kw_array(&mut out, "AL_KEYWORDS_TYPES", &keywords.types);
    write_kw_array(&mut out, "AL_KEYWORDS_METADATA", &keywords.metadata);
    write_kw_array(&mut out, "AL_KEYWORDS_PROPERTIES", &keywords.properties);

    out.push_str("static bool is_al_keyword(const char *word) {\n");
    out.push_str("  return al_kw_binsearch(word, AL_KEYWORDS_ALL, sizeof(AL_KEYWORDS_ALL) / sizeof(AL_KEYWORDS_ALL[0]));\n");
    out.push_str("}\n\n");

    out.push_str("static bool is_al_control_keyword(const char *word) {\n");
    out.push_str("  return al_kw_binsearch(word, AL_KEYWORDS_CONTROL, sizeof(AL_KEYWORDS_CONTROL) / sizeof(AL_KEYWORDS_CONTROL[0]));\n");
    out.push_str("}\n\n");

    out.push_str("static bool is_al_object_keyword(const char *word) {\n");
    out.push_str("  return al_kw_binsearch(word, AL_KEYWORDS_OBJECTS, sizeof(AL_KEYWORDS_OBJECTS) / sizeof(AL_KEYWORDS_OBJECTS[0]));\n");
    out.push_str("}\n\n");

    out.push_str("static bool is_al_type_keyword(const char *word) {\n");
    out.push_str("  return al_kw_binsearch(word, AL_KEYWORDS_TYPES, sizeof(AL_KEYWORDS_TYPES) / sizeof(AL_KEYWORDS_TYPES[0]));\n");
    out.push_str("}\n\n");

    out.push_str("static bool is_al_metadata_keyword(const char *word) {\n");
    out.push_str("  return al_kw_binsearch(word, AL_KEYWORDS_METADATA, sizeof(AL_KEYWORDS_METADATA) / sizeof(AL_KEYWORDS_METADATA[0]));\n");
    out.push_str("}\n\n");

    out.push_str("static bool is_al_property_keyword(const char *word) {\n");
    out.push_str("  return al_kw_binsearch(word, AL_KEYWORDS_PROPERTIES, sizeof(AL_KEYWORDS_PROPERTIES) / sizeof(AL_KEYWORDS_PROPERTIES[0]));\n");
    out.push_str("}\n");

    out.push_str("\nstatic bool is_al_operator_word_keyword(const char *word) {\n");
    out.push_str("  return al_kw_binsearch(word, AL_KEYWORDS_OPERATOR_WORDS, sizeof(AL_KEYWORDS_OPERATOR_WORDS) / sizeof(AL_KEYWORDS_OPERATOR_WORDS[0]));\n");
    out.push_str("}\n");

    out.push_str("\n\ntypedef struct { const char *word; TokenType tok; } AlTokenMapEntry;\n");
    out.push_str("static bool al_kw_token_binsearch(const char *word, const AlTokenMapEntry *arr, size_t count, TokenType *out_tok) {\n");
    out.push_str("  size_t lo = 0;\n");
    out.push_str("  size_t hi = count;\n");
    out.push_str("  while (lo < hi) {\n");
    out.push_str("    size_t mid = lo + (hi - lo) / 2;\n");
    out.push_str("    int cmp = strcmp(word, arr[mid].word);\n");
    out.push_str("    if (cmp == 0) { *out_tok = arr[mid].tok; return true; }\n");
    out.push_str("    if (cmp < 0) hi = mid; else lo = mid + 1;\n");
    out.push_str("  }\n");
    out.push_str("  return false;\n");
    out.push_str("}\n\n");

    out.push_str("static const AlTokenMapEntry AL_CONTROL_KW_TOKENS[] = {\n");
    for kw in &keywords.control {
        out.push_str(&format!(
            "  {{\"{}\", KW_{}}},\n",
            c_escape(kw),
            kw.to_ascii_uppercase()
        ));
    }
    out.push_str("};\n\n");

    out.push_str(
        "static bool al_lookup_control_kw_token(const char *word, TokenType *out_tok) {\n",
    );
    out.push_str("  return al_kw_token_binsearch(word, AL_CONTROL_KW_TOKENS, sizeof(AL_CONTROL_KW_TOKENS) / sizeof(AL_CONTROL_KW_TOKENS[0]), out_tok);\n");
    out.push_str("}\n");

    out.push_str("\nstatic const AlTokenMapEntry AL_OPERATOR_WORD_TOKENS[] = {\n");
    for kw in &keywords.operator_words {
        out.push_str(&format!(
            "  {{\"{}\", OP_{}}},\n",
            c_escape(kw),
            kw.to_ascii_uppercase()
        ));
    }
    out.push_str("};\n\n");

    out.push_str(
        "static bool al_lookup_operator_word_token(const char *word, TokenType *out_tok) {\n",
    );
    out.push_str("  return al_kw_token_binsearch(word, AL_OPERATOR_WORD_TOKENS, sizeof(AL_OPERATOR_WORD_TOKENS) / sizeof(AL_OPERATOR_WORD_TOKENS[0]), out_tok);\n");
    out.push_str("}\n");

    let path = format!("{}/keywords.c", out_dir);
    fs::write(path, out)?;
    Ok(())
}

fn write_kw_array(out: &mut String, name: &str, words: &BTreeSet<String>) {
    out.push_str(&format!("static const char *const {}[] = {{\n", name));
    for w in words {
        out.push_str(&format!("  \"{}\",\n", c_escape(w)));
    }
    out.push_str("};\n\n");
}

fn c_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\"', "\\\"")
}

fn generate_scanner_c(
    _keywords: &Keywords,
    external_tokens: &[ExternalTokenSpec],
    out_dir: &str,
) -> Result<()> {
    let template_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tools/al-gen/templates");
    let template = fs::read_to_string(format!("{}/scanner.c.template", template_dir))?;

    let token_enum = gen_scanner_token_enum_fragment(external_tokens);
    let fastpath = gen_scanner_valid_symbols_fastpath_fragment(external_tokens);
    let directive_handling = gen_scanner_directive_handling_fragment();
    let keyword_dispatch = gen_scanner_keyword_dispatch_fragment();

    let rendered = render_template(
        &template,
        &[
            ("TOKEN_ENUM", &token_enum),
            ("VALID_SYMBOLS_FASTPATH", &fastpath),
            ("DIRECTIVE_HANDLING", &directive_handling),
            ("KEYWORD_DISPATCH", &keyword_dispatch),
        ],
    );

    let path = format!("{}/scanner.c", out_dir);
    fs::write(path, rendered)?;
    Ok(())
}

fn generate_grammar_js(
    keywords: &Keywords,
    external_tokens: &[ExternalTokenSpec],
    root: &str,
) -> Result<()> {
    let template_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tools/al-gen/templates");
    let template = fs::read_to_string(format!("{}/grammar.js.template", template_dir))?;
    let externals = gen_grammar_externals_fragment(external_tokens);
    let objects = gen_choice_fragment(&keywords.objects, &[])?;
    let types_excluding_option = gen_choice_fragment(&keywords.types, &["option"])?;

    let mut placeholders = vec![
        ("EXTERNALS_LIST".to_string(), externals),
        ("OBJECT_DECLARE_KIND".to_string(), objects),
        ("TYPE_REFERENCE_KIND".to_string(), types_excluding_option),
    ];

    let kw_placeholder_re = Regex::new(r"\{\{KW:([a-zA-Z_]+)\}\}")?;
    for caps in kw_placeholder_re.captures_iter(&template) {
        let kw_name = &caps[1].to_lowercase();
        let placeholder_key = format!("KW:{}", &caps[1]);

        let token = if keywords.control.contains(kw_name)
            || keywords.operator_words.contains(kw_name)
            || keywords.objects.contains(kw_name)
            || keywords.types.contains(kw_name)
            || keywords.metadata.contains(kw_name)
            || keywords.properties.contains(kw_name)
        {
            format!("$.kw_{}", kw_name)
        } else {
            anyhow::bail!("Grammar template references unknown keyword: {kw_name}");
        };

        placeholders.push((placeholder_key, token));
    }

    let render_pairs: Vec<(&str, &str)> = placeholders
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let rendered = render_template(&template, &render_pairs);
    let out_path = Path::new(root).join("grammar.js");

    let header = "// AUTO-GENERATED FILE - DO NOT EDIT MANUALLY\n\
                  // Generated from generator/tools/al-gen/templates/grammar.js.template by al-gen.\n\n";

    fs::write(out_path, format!("{}{}", header, rendered))?;
    Ok(())
}

fn gen_choice_fragment(elements: &BTreeSet<String>, exclude: &[&str]) -> Result<String> {
    let filtered: Vec<_> = elements
        .iter()
        .filter(|e| !exclude.contains(&e.as_str()))
        .collect();

    if filtered.is_empty() {
        anyhow::bail!("Cannot generate a choice from an empty keyword category");
    }
    if filtered.len() == 1 {
        return Ok(format!("$.kw_{}", filtered[0]));
    }
    let mut out = "choice(\n".to_string();
    for kw in filtered {
        out.push_str(&format!("      $.kw_{},\n", kw));
    }
    out.push_str("    )");
    Ok(out)
}

fn capture_for_scope<'a>(
    captures: &'a std::collections::HashMap<String, String>,
    scope: &str,
    default: &'a str,
) -> &'a str {
    captures.get(scope).map(String::as_str).unwrap_or(default)
}

fn generate_highlights(keywords: &Keywords, out_path: &str) -> Result<()> {
    let template_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tools/al-gen/templates");
    let template = fs::read_to_string(format!("{}/highlights.scm.template", template_dir))?;

    let mut specific_tokens = String::new();

    // TextMate groups control flow, declarations, and modifiers together;
    // tree-sitter captures distinguish them for themes.
    use std::collections::HashSet;

    let control_flow: HashSet<&str> = [
        "if",
        "then",
        "else",
        "begin",
        "end",
        "for",
        "foreach",
        "to",
        "downto",
        "do",
        "while",
        "repeat",
        "until",
        "case",
        "of",
        "with",
        "in",
        "exit",
        "break",
        "continue",
        "asserterror",
    ]
    .into_iter()
    .collect();

    let function_def: HashSet<&str> = ["procedure", "trigger", "event", "function"]
        .into_iter()
        .collect();

    let modifiers: HashSet<&str> = [
        "var",
        "local",
        "protected",
        "internal",
        "temporary",
        "runonclient",
        "withevents",
        "suppressdispose",
        "indataset",
    ]
    .into_iter()
    .collect();

    for kw in &keywords.control {
        if control_flow.contains(kw.as_str()) {
            specific_tokens.push_str(&format!("(kw_{}) @keyword.control\n", kw));
        }
    }

    specific_tokens.push('\n');
    for kw in &keywords.control {
        if function_def.contains(kw.as_str()) {
            specific_tokens.push_str(&format!("(kw_{}) @keyword.function\n", kw));
        }
    }

    specific_tokens.push('\n');
    for kw in &keywords.control {
        if modifiers.contains(kw.as_str()) {
            specific_tokens.push_str(&format!("(kw_{}) @keyword.modifier\n", kw));
        }
    }

    specific_tokens.push('\n');
    // Last-match-wins would otherwise make the earlier keyword capture dead.
    let types_set: HashSet<&String> = keywords.types.iter().collect();
    for kw in &keywords.control {
        if !control_flow.contains(kw.as_str())
            && !function_def.contains(kw.as_str())
            && !modifiers.contains(kw.as_str())
            && !types_set.contains(kw)
        {
            specific_tokens.push_str(&format!("(kw_{}) @keyword\n", kw));
        }
    }

    specific_tokens.push('\n');
    for kw in &keywords.operator_words {
        specific_tokens.push_str(&format!("(op_{}) @keyword.operator\n", kw));
    }

    fn find_capture<'a>(
        caps: &'a std::collections::HashMap<String, String>,
        pred: impl Fn(&str) -> bool,
        default: &'a str,
    ) -> &'a str {
        caps.iter()
            .find(|(scope, _)| pred(scope))
            .map(|(_, cap)| cap.as_str())
            .unwrap_or(default)
    }

    let type_capture = find_capture(
        &keywords.scope_captures,
        |s| s.contains("builtintypes"),
        "@type.builtin",
    );

    // Query matches are last-wins, so type captures follow keyword captures.
    specific_tokens.push_str("\n; Type keywords (override control keyword captures)\n");
    for kw in &keywords.types {
        specific_tokens.push_str(&format!("(kw_{}) {}\n", kw, type_capture));
    }

    let object_capture = find_capture(
        &keywords.scope_captures,
        |s| s.contains("applicationobject"),
        "@keyword",
    );
    let metadata_capture = find_capture(
        &keywords.scope_captures,
        |s| s.contains("metadata"),
        "@keyword",
    );
    let property_capture = capture_for_scope(
        &keywords.scope_captures,
        "keyword.operators.property.al",
        "@operator",
    );
    let operator_capture = find_capture(
        &keywords.scope_captures,
        |s| s.contains("keyword.operator"),
        "@operator",
    );

    let quoted_identifier_capture = find_capture(
        &keywords.scope_captures,
        |s| s.contains("identifier.quoted"),
        "@variable",
    );

    let comment_capture = find_capture(
        &keywords.scope_captures,
        |s| s.starts_with("comment."),
        "@comment",
    );
    let string_capture = find_capture(
        &keywords.scope_captures,
        |s| s.starts_with("string."),
        "@string",
    );
    let number_capture = find_capture(
        &keywords.scope_captures,
        |s| s.contains("constant.numeric"),
        "@number",
    );
    let variable_capture = find_capture(
        &keywords.scope_captures,
        |s| s.starts_with("variable."),
        "@variable",
    );

    let constant_capture = find_capture(
        &keywords.scope_captures,
        |s| s.contains("constant.language"),
        "@constant.builtin",
    );

    let keyword_capture = find_capture(
        &keywords.scope_captures,
        |s| s == "keyword.control.al" || (s.starts_with("keyword.") && !s.contains("operator")),
        "@keyword",
    );
    let punctuation_capture = find_capture(
        &keywords.scope_captures,
        |s| s == "punctuation.al",
        "@punctuation.delimiter",
    );
    let function_capture = find_capture(
        &keywords.scope_captures,
        |s| s.contains("entity.name.function"),
        "@function.definition",
    );

    let rendered = render_template(
        &template,
        &[
            ("CONTROL_KW_TOKEN_HIGHLIGHTS", &specific_tokens),
            ("OBJECT_CAPTURE", object_capture),
            ("TYPE_CAPTURE", type_capture),
            ("METADATA_CAPTURE", metadata_capture),
            ("PROPERTY_CAPTURE", property_capture),
            ("QUOTED_IDENTIFIER_CAPTURE", quoted_identifier_capture),
            ("COMMENT_CAPTURE", comment_capture),
            ("STRING_CAPTURE", string_capture),
            ("NUMBER_CAPTURE", number_capture),
            ("VARIABLE_CAPTURE", variable_capture),
            ("CONSTANT_CAPTURE", constant_capture),
            ("OPERATOR_CAPTURE", operator_capture),
            ("KEYWORD_CAPTURE", keyword_capture),
            ("PUNCTUATION_CAPTURE", punctuation_capture),
            ("FUNCTION_CAPTURE", function_capture),
        ],
    );
    fs::write(out_path, rendered)?;
    Ok(())
}

// Structural query generation from node-types.json
/// Node type entry from tree-sitter's generated node-types.json.
#[derive(Debug, Deserialize)]
struct GrammarNodeType {
    #[serde(rename = "type")]
    type_name: String,
    named: bool,
    #[serde(default)]
    fields: BTreeMap<String, GrammarFieldInfo>,
    #[serde(default)]
    children: Option<GrammarChildrenInfo>,
}

#[derive(Debug, Deserialize)]
struct GrammarFieldInfo {
    #[serde(default)]
    types: Vec<GrammarTypeRef>,
}

#[derive(Debug, Deserialize)]
struct GrammarChildrenInfo {
    #[serde(default)]
    types: Vec<GrammarTypeRef>,
}

#[derive(Debug, Deserialize)]
struct GrammarTypeRef {
    #[serde(rename = "type")]
    type_name: String,
    named: bool,
}

fn generate_structural_queries(node_types_path: &str, queries_dir: &str) -> Result<()> {
    let content = fs::read_to_string(node_types_path)
        .with_context(|| format!("Failed to read {}", node_types_path))?;
    let nodes: Vec<GrammarNodeType> =
        serde_json::from_str(&content).context("Failed to parse node-types.json")?;

    let node_map: BTreeMap<&str, &GrammarNodeType> = nodes
        .iter()
        .filter(|n| n.named)
        .map(|n| (n.type_name.as_str(), n))
        .collect();

    generate_folds_scm(&nodes, &format!("{}/folds.scm", queries_dir))?;
    println!("Generated {}/folds.scm", queries_dir);

    generate_locals_scm(&nodes, &node_map, &format!("{}/locals.scm", queries_dir))?;
    println!("Generated {}/locals.scm", queries_dir);

    generate_textobjects_scm(
        &nodes,
        &node_map,
        &format!("{}/textobjects.scm", queries_dir),
    )?;
    println!("Generated {}/textobjects.scm", queries_dir);

    Ok(())
}

fn generate_folds_scm(nodes: &[GrammarNodeType], out_path: &str) -> Result<()> {
    let exclude: BTreeSet<&str> = [
        "empty_if_statement",
        "break_statement",
        "continue_statement",
        "exit_statement",
        "expression_statement",
        "enum_value_declaration",
        "event_procedure_declaration",
        "key_declaration",
        "label_declaration",
        "namespace_or_using_declaration",
        "object_variable_declaration",
        "regular_variable_declaration",
        "variable_declaration",
        "empty_var_section",
        "parenthesized_block",
        "bracketed_block",
    ]
    .into_iter()
    .collect();

    let foldable_suffixes = ["_statement", "_declaration", "_block", "_section"];

    let mut foldable: Vec<&str> = Vec::new();

    for node in nodes {
        if !node.named {
            continue;
        }
        let name = node.type_name.as_str();
        if exclude.contains(name) {
            continue;
        }

        let matches_suffix = foldable_suffixes.iter().any(|s| name.ends_with(s));
        let is_extra = name == "case_branch" || name == "argument_list";

        if matches_suffix || is_extra {
            foldable.push(name);
        }
    }
    foldable.sort();

    let mut out = String::from(
        "; Code folding regions for AL\n\
         ; AUTO-GENERATED from node-types.json — do not edit manually\n\n[\n",
    );
    for name in &foldable {
        out.push_str(&format!("  ({})\n", name));
    }
    out.push_str("] @fold\n");

    fs::write(out_path, out)?;
    Ok(())
}

fn generate_locals_scm(
    nodes: &[GrammarNodeType],
    node_map: &BTreeMap<&str, &GrammarNodeType>,
    out_path: &str,
) -> Result<()> {
    let mut out = String::new();
    out.push_str(
        "; Local scope and variable resolution for AL\n\
         ; AUTO-GENERATED from node-types.json — do not edit manually\n\n\
         ; SCOPES\n\n",
    );

    let scope_types: BTreeSet<&str> = [
        "source_file",
        "object_declaration",
        "procedure_declaration",
        "trigger_declaration",
        "event_declaration",
        "begin_end_block",
    ]
    .into_iter()
    .collect();

    let statement_exclude: BTreeSet<&str> = [
        "asserterror_statement",
        "break_statement",
        "continue_statement",
        "empty_if_statement",
        "exit_statement",
        "expression_statement",
    ]
    .into_iter()
    .collect();

    let mut scopes: Vec<&str> = Vec::new();
    for node in nodes {
        if !node.named {
            continue;
        }
        let name = node.type_name.as_str();
        if scope_types.contains(name)
            || (name.ends_with("_statement") && !statement_exclude.contains(name))
        {
            scopes.push(name);
        }
    }
    scopes.sort();

    for name in &scopes {
        out.push_str(&format!("({}) @local.scope\n\n", name));
    }

    out.push_str("; DEFINITIONS\n\n");

    let def_rules: &[(&str, &str)] = &[
        ("object_declaration", "type"),
        ("procedure_declaration", "method"),
        ("trigger_declaration", "method"),
        ("event_declaration", "method"),
        ("regular_variable_declaration", "var"),
        ("object_variable_declaration", "field"),
        ("parameter", "parameter"),
    ];

    for &(node_type, def_kind) in def_rules {
        let Some(node) = node_map.get(node_type) else {
            continue;
        };
        let Some(name_field) = node.fields.get("name") else {
            continue;
        };

        let field_type_names: Vec<&str> = name_field
            .types
            .iter()
            .filter(|t| t.named)
            .map(|t| t.type_name.as_str())
            .collect();

        let capture = format!("@local.definition.{}", def_kind);

        if field_type_names.contains(&"name_or_keyword") {
            if let Some(nok) = node_map.get("name_or_keyword") {
                let nok_has_name = nok
                    .children
                    .as_ref()
                    .map(|c| c.types.iter().any(|t| t.type_name == "name"))
                    .unwrap_or(false);
                if nok_has_name && let Some(name_node) = node_map.get("name") {
                    let has_ident = name_node
                        .children
                        .as_ref()
                        .map(|c| c.types.iter().any(|t| t.type_name == "identifier"))
                        .unwrap_or(false);
                    let has_quoted = name_node
                        .children
                        .as_ref()
                        .map(|c| c.types.iter().any(|t| t.type_name == "quoted_identifier"))
                        .unwrap_or(false);
                    if has_ident {
                        out.push_str(&format!(
                            "({}\n  name: (name_or_keyword (name (identifier) {})))\n\n",
                            node_type, capture
                        ));
                    }
                    if has_quoted {
                        out.push_str(&format!(
                            "({}\n  name: (name_or_keyword (name (quoted_identifier) {})))\n\n",
                            node_type, capture
                        ));
                    }
                }
            }
        } else if field_type_names.contains(&"name") {
            if let Some(name_node) = node_map.get("name") {
                let has_ident = name_node
                    .children
                    .as_ref()
                    .map(|c| c.types.iter().any(|t| t.type_name == "identifier"))
                    .unwrap_or(false);
                let has_quoted = name_node
                    .children
                    .as_ref()
                    .map(|c| c.types.iter().any(|t| t.type_name == "quoted_identifier"))
                    .unwrap_or(false);
                if has_ident {
                    out.push_str(&format!(
                        "({}\n  name: (name (identifier) {}))\n\n",
                        node_type, capture
                    ));
                }
                if has_quoted {
                    out.push_str(&format!(
                        "({}\n  name: (name (quoted_identifier) {}))\n\n",
                        node_type, capture
                    ));
                }
            }
        } else {
            out.push_str(&format!("({}\n  name: (_) {})\n\n", node_type, capture));
        }
    }

    out.push_str("; REFERENCES\n\n");

    for node in nodes {
        if !node.named {
            continue;
        }
        if node.type_name == "identifier" || node.type_name == "quoted_identifier" {
            out.push_str(&format!("({}) @local.reference\n\n", node.type_name));
        }
    }

    fs::write(out_path, out)?;
    Ok(())
}

fn generate_textobjects_scm(
    nodes: &[GrammarNodeType],
    node_map: &BTreeMap<&str, &GrammarNodeType>,
    out_path: &str,
) -> Result<()> {
    let mut out = String::from(
        "; Text objects for AL (vim-mode: select function, class, comment)\n\
         ; AUTO-GENERATED from node-types.json — do not edit manually\n\n",
    );

    let function_types = [
        "procedure_declaration",
        "trigger_declaration",
        "event_declaration",
    ];

    out.push_str("; Functions — procedures, triggers, events\n");
    for &func_type in &function_types {
        let Some(node) = node_map.get(func_type) else {
            continue;
        };
        out.push_str(&format!("({}) @function.around\n\n", func_type));

        let has_body_block = node
            .children
            .as_ref()
            .map(|c| c.types.iter().any(|t| t.type_name == "begin_end_block"))
            .unwrap_or(false);
        if has_body_block {
            out.push_str(&format!(
                "({}\n  (begin_end_block) @function.inside)\n\n",
                func_type
            ));
        }
    }

    out.push_str("; Classes — AL objects (codeunit, table, page, report, etc.)\n");
    for node in nodes {
        if !node.named {
            continue;
        }
        if let Some(body_field) = node.fields.get("body") {
            let has_object_body = body_field
                .types
                .iter()
                .any(|t| t.type_name == "object_body");
            if has_object_body {
                out.push_str(&format!("({}) @class.around\n\n", node.type_name));
                out.push_str(&format!(
                    "({}\n  body: (object_body) @class.inside)\n\n",
                    node.type_name
                ));
            }
        }
    }

    out.push_str("; Comments\n");
    for node in nodes {
        if node.named && node.type_name == "comment" {
            out.push_str("(comment) @comment.around\n");
        }
    }

    fs::write(out_path, out)?;
    Ok(())
}

fn run_tree_sitter_generate(root: &str) -> Result<()> {
    let root_abs = fs::canonicalize(root).context("Failed to canonicalize root")?;
    println!("   Generate root absolute: {}", root_abs.display());

    let output = Command::new("tree-sitter")
        .current_dir(&root_abs)
        .arg("generate")
        .output()
        .context("Failed to run tree-sitter")?;

    if !output.status.success() {
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
        anyhow::bail!("tree-sitter generate failed");
    }

    Ok(())
}

fn run_tree_sitter_build(root: &str) -> Result<PathBuf> {
    let root_abs = fs::canonicalize(root).context("Failed to canonicalize root")?;
    println!("   Build root absolute: {}", root_abs.display());

    let out_path = root_abs.join("target/tree-sitter-al.so");

    // Ensure target dir exists relative to root
    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let status = Command::new("tree-sitter")
        .current_dir(&root_abs)
        .arg("build")
        .arg("-o")
        .arg(&out_path)
        .status()
        .context("Failed to run tree-sitter build")?;

    if !status.success() {
        anyhow::bail!("tree-sitter build failed");
    }

    Ok(out_path)
}

#[derive(Debug, Deserialize)]
struct RepoList {
    #[serde(default)]
    repo: Vec<RepoConfig>,
}

#[derive(Debug, Deserialize)]
struct RepoConfig {
    name: String,
    url: String,
    #[serde(default = "default_branch")]
    branch: String,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    description: Option<String>,
}

fn default_branch() -> String {
    "main".to_string()
}

fn run_repo_tests(parser_lib: &Path, scope_name: &str, root: &str) -> Result<()> {
    let config_path = Path::new(root).join(REPO_TEST_CONFIG);
    let cfg_text = fs::read_to_string(&config_path)
        .with_context(|| format!("Failed to read {}", config_path.display()))?;
    let cfg: RepoList = toml::from_str(&cfg_text)
        .with_context(|| format!("Failed to parse {}", config_path.display()))?;

    let enabled: Vec<_> = cfg.repo.into_iter().filter(|r| r.enabled).collect();
    if enabled.is_empty() {
        anyhow::bail!("No enabled repositories in {}", config_path.display());
    }

    let work_dir = Path::new(root).join(REPO_TEST_WORKDIR);
    fs::create_dir_all(&work_dir)?;

    let target_dir = Path::new(root).join("target");
    fs::create_dir_all(&target_dir)?;

    let mut total_files = 0usize;
    let mut total_ok = 0usize;

    for repo in enabled {
        println!("\nRepository: {}", repo.name);
        if let Some(desc) = &repo.description {
            println!("   {}", desc);
        }
        println!("   URL: {}", repo.url);

        let repo_dir = work_dir.join(&repo.name);
        clone_or_update_repo(&repo, &repo_dir)?;

        let repo_dir_abs = fs::canonicalize(&repo_dir)
            .with_context(|| format!("Failed to canonicalize {}", repo_dir.display()))?;
        let files = collect_al_files(&repo_dir_abs)?;
        total_files += files.len();

        if files.is_empty() {
            anyhow::bail!("No AL files found in {}", repo_dir_abs.display());
        }

        let paths_file = target_dir.join(format!("paths-{}.txt", repo.name));
        write_paths_file(&paths_file, &files)?;

        let paths_file_abs = fs::canonicalize(&paths_file)
            .with_context(|| format!("Failed to canonicalize {}", paths_file.display()))?;

        let (ok, failed, samples) =
            parse_paths_with_tree_sitter(parser_lib, scope_name, &paths_file_abs, root)?;
        total_ok += ok;

        let pct = (ok as f64) * 100.0 / (files.len() as f64);
        println!("   Total files:  {}", files.len());
        println!("   Parsed OK:    {} ({:.2}%)", ok, pct);
        println!("   Parse errors: {}", failed);
        if !samples.is_empty() {
            println!("   Failed examples:");
            for s in &samples {
                println!("   - {}", s);
            }
            print_failed_parse_details(parser_lib, scope_name, &samples)?;
        }
    }

    if total_files > 0 {
        let pct = (total_ok as f64) * 100.0 / (total_files as f64);
        println!("\nOverall repository results");
        println!("Total AL files tested:     {}", total_files);
        println!("Successfully parsed:       {} ({:.2}%)", total_ok, pct);
        println!("Parse errors:              {}", total_files - total_ok);
    }

    Ok(())
}

fn run_fixture_tests(parser_lib: &Path, scope_name: &str, root: &str) -> Result<()> {
    let invalid_dir = Path::new(root).join(FIXTURES_INVALID_DIR);
    let valid_dir = Path::new(root).join(FIXTURES_VALID_DIR);
    let target_dir = Path::new(root).join("target");
    fs::create_dir_all(&target_dir)?;

    let invalid_dir_abs = fs::canonicalize(&invalid_dir).with_context(|| {
        format!(
            "Missing invalid fixture directory: {}",
            invalid_dir.display()
        )
    })?;
    let invalid = collect_al_files(&invalid_dir_abs)?;
    if invalid.is_empty() {
        anyhow::bail!("No invalid fixtures in {}", invalid_dir.display());
    }
    println!("\nTesting invalid fixtures");
    let paths_file = target_dir.join("paths-fixtures-invalid.txt");

    write_paths_file(&paths_file, &invalid)?;
    let paths_file_abs = fs::canonicalize(&paths_file)?;

    let summaries =
        parse_paths_with_tree_sitter_detailed(parser_lib, scope_name, &paths_file_abs, root)?;

    let mut unexpected_ok = Vec::new();
    for summary in summaries {
        if summary.successful {
            unexpected_ok.push(summary.file);
        }
    }
    if !unexpected_ok.is_empty() {
        anyhow::bail!(
            "Invalid fixtures unexpectedly parsed successfully:\n{}",
            unexpected_ok
                .into_iter()
                .take(20)
                .map(|path| format!("- {path}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    println!("Invalid fixtures rejected: {}", invalid.len());

    let valid_dir_abs = fs::canonicalize(&valid_dir)
        .with_context(|| format!("Missing valid fixture directory: {}", valid_dir.display()))?;
    let valid = collect_al_files(&valid_dir_abs)?;
    if valid.is_empty() {
        anyhow::bail!("No valid fixtures in {}", valid_dir.display());
    }
    println!("Testing valid fixtures");
    let paths_file = target_dir.join("paths-fixtures-valid.txt");

    write_paths_file(&paths_file, &valid)?;
    let paths_file_abs = fs::canonicalize(&paths_file)?;

    let summaries =
        parse_paths_with_tree_sitter_detailed(parser_lib, scope_name, &paths_file_abs, root)?;

    let mut unexpected_failed = Vec::new();
    for summary in summaries {
        if !summary.successful {
            unexpected_failed.push(summary.file);
        }
    }
    if !unexpected_failed.is_empty() {
        anyhow::bail!(
            "Valid fixtures unexpectedly failed to parse:\n{}",
            unexpected_failed
                .into_iter()
                .take(20)
                .map(|path| format!("- {path}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    println!("Valid fixtures parsed: {}", valid.len());

    Ok(())
}

fn print_failed_parse_details(
    parser_lib: &Path,
    scope_name: &str,
    samples: &[String],
) -> Result<()> {
    let limit = 3usize.min(samples.len());
    if limit == 0 {
        return Ok(());
    }
    println!("   Failure details (first {}):", limit);
    for path in samples.iter().take(limit) {
        let output = Command::new("tree-sitter")
            .arg("parse")
            .arg("--cst")
            .arg("--no-ranges")
            .arg("--lib-path")
            .arg(parser_lib)
            .arg("--lang-name")
            .arg("al")
            .arg("--scope")
            .arg(scope_name)
            .arg(path)
            .output()
            .with_context(|| format!("Failed to run tree-sitter parse on sample: {path}"))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let filtered = filter_tree_sitter_config_warning(&stdout);
        let snippet = truncate_lines(&filtered, 60);
        println!("   --- {}", path);
        for line in snippet.lines() {
            println!("   {}", line);
        }
    }
    Ok(())
}

fn filter_tree_sitter_config_warning(s: &str) -> String {
    // When using `tree-sitter parse --lib-path`, some versions still print a global
    // "no parser directories configured" warning. Strip it from our diagnostic snippets.
    s.lines()
        .filter(|line| {
            let l = line.trim();
            !(l.starts_with("Warning: You have not configured any parser directories!")
                || l.starts_with("Please run `tree-sitter init-config`")
                || l.starts_with("configuration file to indicate where we should look for")
                || l.starts_with("language grammars."))
        })
        .map(|l| format!("{l}\n"))
        .collect()
}

fn truncate_lines(s: &str, max_lines: usize) -> String {
    let mut out = String::new();
    for (i, line) in s.lines().enumerate() {
        if i >= max_lines {
            out.push_str("... (truncated)\n");
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn clone_or_update_repo(repo: &RepoConfig, dest: &Path) -> Result<()> {
    if dest.exists() {
        println!("   Updating checkout");

        let status = Command::new("git")
            .arg("-C")
            .arg(dest)
            .arg("fetch")
            .arg("--all")
            .status()
            .context("git fetch failed")?;
        if !status.success() {
            anyhow::bail!("git fetch failed for {}", repo.name);
        }

        let status = Command::new("git")
            .arg("-C")
            .arg(dest)
            .arg("checkout")
            .arg(&repo.branch)
            .status()
            .context("git checkout failed")?;
        if !status.success() {
            anyhow::bail!("git checkout failed for {}", repo.name);
        }

        let status = Command::new("git")
            .arg("-C")
            .arg(dest)
            .arg("pull")
            .arg("--ff-only")
            .status()
            .context("git pull failed")?;
        if !status.success() {
            anyhow::bail!("git pull failed for {}", repo.name);
        }

        Ok(())
    } else {
        println!("   Cloning repository");
        let status = Command::new("git")
            .arg("clone")
            .arg("--depth")
            .arg("1")
            .arg("--branch")
            .arg(&repo.branch)
            .arg(&repo.url)
            .arg(dest)
            .status()
            .context("git clone failed")?;
        if !status.success() {
            anyhow::bail!("git clone failed for {}", repo.name);
        }
        Ok(())
    }
}

fn collect_al_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry.with_context(|| format!("Failed to walk {}", root.display()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("al") || extension.eq_ignore_ascii_case("dal")
            })
        {
            out.push(path.to_path_buf());
        }
    }
    out.sort();
    Ok(out)
}

fn write_paths_file(paths_file: &Path, files: &[PathBuf]) -> Result<()> {
    let mut buf = String::new();
    for p in files {
        let display_path = p
            .to_str()
            .with_context(|| format!("Non-UTF-8 path: {}", p.display()))?;
        if display_path.contains('\n') || display_path.contains('\r') {
            anyhow::bail!(
                "Path cannot be represented in a tree-sitter paths file: {}",
                p.display()
            );
        }
        buf.push_str(display_path);
        buf.push('\n');
    }
    fs::write(paths_file, buf)?;
    Ok(())
}

fn parse_paths_with_tree_sitter(
    parser_lib: &Path,
    scope_name: &str,
    paths_file: &Path,
    root: &str,
) -> Result<(usize, usize, Vec<String>)> {
    let summaries =
        parse_paths_with_tree_sitter_detailed(parser_lib, scope_name, paths_file, root)?;
    let mut ok = 0usize;
    let mut failed = 0usize;
    let mut samples = Vec::new();
    for s in summaries {
        if s.successful {
            ok += 1;
        } else {
            failed += 1;
            if samples.len() < 10 {
                samples.push(s.file);
            }
        }
    }
    Ok((ok, failed, samples))
}

#[derive(Debug, Clone)]
struct FileParseSummary {
    file: String,
    successful: bool,
}

fn parse_paths_with_tree_sitter_detailed(
    parser_lib: &Path,
    scope_name: &str,
    paths_file: &Path,
    root: &str,
) -> Result<Vec<FileParseSummary>> {
    let output = Command::new("tree-sitter")
        .current_dir(root)
        .arg("parse")
        .arg("--quiet")
        .arg("--json-summary")
        .arg("--lib-path")
        .arg(parser_lib)
        .arg("--lang-name")
        .arg("al")
        .arg("--scope")
        .arg(scope_name)
        .arg("--paths")
        .arg(paths_file)
        .output()
        .context("Failed to run tree-sitter parse")?;

    let stdout_full = String::from_utf8_lossy(&output.stdout);
    let stdout_json = extract_json_object_from_output(&stdout_full)
        .context("tree-sitter produced no JSON summary")?;

    let summary: serde_json::Value = match serde_json::from_str(stdout_json) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{}", String::from_utf8_lossy(&output.stderr));
            return Err(anyhow!(
                "Failed to parse tree-sitter --json-summary output: {e}"
            ));
        }
    };

    if let Some(summaries) = summary.get("parse_summaries").and_then(|v| v.as_array()) {
        let mut out = Vec::new();
        for s in summaries {
            let file = s
                .get("file")
                .or_else(|| s.get("path"))
                .and_then(|v| v.as_str())
                .context("parse summary has no file path")?
                .to_string();
            let successful = s
                .get("successful")
                .and_then(|v| v.as_bool())
                .context("parse summary has no successful flag")?;
            out.push(FileParseSummary { file, successful });
        }
        return Ok(out);
    }

    if let Some(files) = summary.get("files").and_then(|v| v.as_array()) {
        let mut out = Vec::new();
        for f in files {
            let file = f
                .get("path")
                .or_else(|| f.get("file"))
                .and_then(|v| v.as_str())
                .context("file summary has no path")?
                .to_string();
            let successful = match f.get("success").and_then(|v| v.as_bool()) {
                Some(successful) => successful,
                None => f
                    .get("error_count")
                    .and_then(|v| v.as_u64())
                    .map(|count| count == 0)
                    .context("file summary has neither success nor error_count")?,
            };
            out.push(FileParseSummary { file, successful });
        }
        return Ok(out);
    }

    anyhow::bail!("Unrecognized tree-sitter --json-summary output format");
}

fn extract_json_object_from_output(s: &str) -> Option<&str> {
    // Some CLI versions print per-file output before the JSON object.
    let mut offset = 0usize;
    for line in s.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('{') {
            let brace_idx_in_line = line.len() - trimmed.len();
            return Some(&s[offset + brace_idx_in_line..]);
        }
        offset += line.len() + 1; // + '\n'
    }
    None
}

// ---------------------------------------------------------------------------
// Theme generation: VS Code BC themes -> Zed theme format
// ---------------------------------------------------------------------------

/// Strip JavaScript-style comments and trailing commas from JSONC.
/// The BC theme files use VS Code's JSONC format with `// ...` comments
/// and trailing commas, which serde_json cannot parse.
fn strip_jsonc(input: &str) -> Result<String> {
    let mut out = String::with_capacity(input.len());
    let mut in_string = false;
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if in_string {
            out.push(ch);
            // Handle escape sequences inside strings
            if ch == '\\' {
                if let Some(&next) = chars.peek() {
                    out.push(next);
                    chars.next();
                }
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        if ch == '"' {
            in_string = true;
            out.push(ch);
            continue;
        }

        if ch == '/' {
            if chars.peek() == Some(&'/') {
                // Single-line comment: skip until end of line
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                        break;
                    }
                }
                continue;
            }
            if chars.peek() == Some(&'*') {
                // Block comment: skip until */
                chars.next(); // consume '*'
                let mut prev = ' ';
                for c in chars.by_ref() {
                    if prev == '*' && c == '/' {
                        break;
                    }
                    if c == '\n' {
                        out.push('\n');
                    }
                    prev = c;
                }
                continue;
            }
        }

        out.push(ch);
    }

    let trailing_comma_re = Regex::new(r",(\s*[}\]])")?;
    Ok(trailing_comma_re.replace_all(&out, "$1").to_string())
}

/// Convert a single VS Code BC theme file to a Zed theme variant.
fn convert_vscode_theme_to_zed(vscode_theme: &serde_json::Value) -> Result<serde_json::Value> {
    let name = vscode_theme
        .get("name")
        .and_then(|v| v.as_str())
        .context("Theme has no string name")?;

    let theme_type = vscode_theme
        .get("type")
        .and_then(|v| v.as_str())
        .context("Theme has no string type")?;

    let appearance = match theme_type {
        "dark" => "dark",
        "light" => "light",
        other => anyhow::bail!("Unsupported theme type: {other}"),
    };

    let colors = vscode_theme
        .get("colors")
        .and_then(|v| v.as_object())
        .context("Theme has no colors object")?;

    let token_colors = vscode_theme
        .get("tokenColors")
        .and_then(|v| v.as_array())
        .context("Theme has no tokenColors array")?;

    let mut style = serde_json::Map::new();
    map_ui_colors(colors, &mut style, appearance)?;

    let syntax = map_token_colors(token_colors);
    style.insert("syntax".to_string(), serde_json::Value::Object(syntax));

    Ok(serde_json::json!({
        "name": name,
        "appearance": appearance,
        "style": serde_json::Value::Object(style)
    }))
}

/// Map VS Code `colors` object to Zed `style` UI color properties.
fn map_ui_colors(
    colors: &serde_json::Map<String, serde_json::Value>,
    style: &mut serde_json::Map<String, serde_json::Value>,
    appearance: &str,
) -> Result<()> {
    let mappings: &[(&str, &[&str])] = &[
        (
            "editor.background",
            &["background", "editor.background", "toolbar.background"],
        ),
        ("editor.foreground", &["editor.foreground", "text"]),
        ("activityBar.background", &["tab_bar.background"]),
        ("sideBar.background", &["panel.background"]),
        ("sideBarSectionHeader.background", &["surface.background"]),
        ("statusBar.background", &["status_bar.background"]),
        (
            "editorSuggestWidget.background",
            &["elevated_surface.background"],
        ),
        ("editorIndentGuide.background1", &["editor.wrap_guide"]),
        (
            "editorIndentGuide.activeBackground1",
            &["editor.active_wrap_guide"],
        ),
        (
            "editor.selectionBackground",
            &["editor.highlight.occurrence"],
        ),
        (
            "editor.selectionHighlightBackground",
            &[
                "editor.document_highlight.read_background",
                "editor.document_highlight.write_background",
            ],
        ),
        (
            "list.activeSelectionBackground",
            &["element.selected", "ghost_element.selected"],
        ),
        (
            "list.hoverBackground",
            &["element.hover", "ghost_element.hover"],
        ),
        ("input.placeholderForeground", &["text.placeholder"]),
        ("sideBarSectionHeader.border", &["border"]),
        ("list.focusAndSelectionOutline", &["border.focused"]),
        ("button.background", &["element.active"]),
        ("tab.activeBackground", &["tab.active_background"]),
        ("tab.inactiveBackground", &["tab.inactive_background"]),
    ];

    for (vscode_key, zed_keys) in mappings {
        if let Some(value) = colors.get(*vscode_key) {
            for zed_key in *zed_keys {
                style.insert(zed_key.to_string(), value.clone());
            }
        }
    }

    if let Some(v) = colors.get("editorLineNumber.foreground") {
        style.insert("editor.line_number".to_string(), v.clone());
    } else if let Some(fg) = colors.get("editor.foreground").and_then(|v| v.as_str()) {
        style.insert(
            "editor.line_number".to_string(),
            serde_json::Value::String(dim_color(fg, 0.5)?),
        );
    }

    if let Some(v) = colors.get("editor.foreground") {
        style.insert("editor.active_line_number".to_string(), v.clone());
    }

    if let Some(bg) = colors.get("editor.background").and_then(|v| v.as_str()) {
        let active_line_bg = if appearance == "dark" {
            lighten_color(bg, 0.05)?
        } else {
            darken_color(bg, 0.03)?
        };
        style.insert(
            "editor.active_line.background".to_string(),
            serde_json::Value::String(active_line_bg),
        );
    }

    if let Some(v) = colors.get("editor.background") {
        style.insert("editor.gutter.background".to_string(), v.clone());
    }

    if let Some(bg) = colors.get("editor.background").and_then(|v| v.as_str()) {
        let title_bg = if appearance == "dark" {
            darken_color(bg, 0.05)?
        } else {
            darken_color(bg, 0.03)?
        };
        style.insert(
            "title_bar.background".to_string(),
            serde_json::Value::String(title_bg),
        );
    }

    if !style.contains_key("tab_bar.background")
        && let Some(bg) = colors.get("editor.background").and_then(|v| v.as_str())
    {
        let tab_bar_bg = if appearance == "dark" {
            darken_color(bg, 0.03)?
        } else {
            darken_color(bg, 0.02)?
        };
        style.insert(
            "tab_bar.background".to_string(),
            serde_json::Value::String(tab_bar_bg),
        );
    }

    if let Some(v) = colors.get("editor.background") {
        style.insert("terminal.background".to_string(), v.clone());
    }

    let ansi_mappings: &[(&str, &str)] = &[
        ("terminal.ansiBlack", "terminal.ansi.black"),
        ("terminal.ansiRed", "terminal.ansi.red"),
        ("terminal.ansiGreen", "terminal.ansi.green"),
        ("terminal.ansiYellow", "terminal.ansi.yellow"),
        ("terminal.ansiBlue", "terminal.ansi.blue"),
        ("terminal.ansiMagenta", "terminal.ansi.magenta"),
        ("terminal.ansiCyan", "terminal.ansi.cyan"),
        ("terminal.ansiWhite", "terminal.ansi.white"),
        ("terminal.ansiBrightBlack", "terminal.ansi.bright_black"),
        ("terminal.ansiBrightRed", "terminal.ansi.bright_red"),
        ("terminal.ansiBrightGreen", "terminal.ansi.bright_green"),
        ("terminal.ansiBrightYellow", "terminal.ansi.bright_yellow"),
        ("terminal.ansiBrightBlue", "terminal.ansi.bright_blue"),
        ("terminal.ansiBrightMagenta", "terminal.ansi.bright_magenta"),
        ("terminal.ansiBrightCyan", "terminal.ansi.bright_cyan"),
        ("terminal.ansiBrightWhite", "terminal.ansi.bright_white"),
    ];

    for (vscode_key, zed_key) in ansi_mappings {
        if let Some(v) = colors.get(*vscode_key) {
            style.insert(zed_key.to_string(), v.clone());
        }
    }

    if let Some(v) = colors.get("editorError.foreground") {
        style.insert("error".to_string(), v.clone());
    }
    if let Some(v) = colors.get("editorWarning.foreground") {
        style.insert("warning".to_string(), v.clone());
    }

    if let Some(bg) = colors.get("editor.background").and_then(|v| v.as_str()) {
        style.insert(
            "scrollbar.track.background".to_string(),
            serde_json::Value::String(bg.to_string()),
        );
        let thumb = if appearance == "dark" {
            lighten_color(bg, 0.15)?
        } else {
            darken_color(bg, 0.12)?
        };
        style.insert(
            "scrollbar.thumb.background".to_string(),
            serde_json::Value::String(format!("{}80", thumb)),
        );
    }

    if let Some(border) = style.get("border").cloned() {
        style.insert("border.variant".to_string(), border);
    }

    if let Some(brand_color) = colors
        .get("button.background")
        .or_else(|| colors.get("statusBar.background"))
        .and_then(|v| v.as_str())
    {
        let (red, green, blue) = parse_hex_color(brand_color)
            .with_context(|| format!("Invalid player color: {brand_color}"))?;
        let brand_color = format!("#{red:02X}{green:02X}{blue:02X}");
        style.insert(
            "players".to_string(),
            serde_json::json!([{
                "cursor": format!("{brand_color}ff"),
                "background": format!("{brand_color}ff"),
                "selection": format!("{brand_color}40")
            }]),
        );
    }

    Ok(())
}

/// Map VS Code `tokenColors` array to Zed `syntax` object.
fn map_token_colors(
    token_colors: &[serde_json::Value],
) -> serde_json::Map<String, serde_json::Value> {
    // Build a scope -> (color, font_style) map from VS Code tokenColors.
    // VS Code uses most-specific-scope-wins, so we process in order and let
    // more specific scopes override less specific ones.
    let mut scope_map: BTreeMap<String, (Option<String>, Option<String>)> = BTreeMap::new();

    for entry in token_colors {
        let settings = match entry.get("settings").and_then(|v| v.as_object()) {
            Some(s) => s,
            None => continue,
        };

        let foreground = settings
            .get("foreground")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let font_style = settings
            .get("fontStyle")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Scope can be a string or an array of strings
        let scopes: Vec<String> = match entry.get("scope") {
            Some(serde_json::Value::String(s)) => vec![s.clone()],
            Some(serde_json::Value::Array(arr)) => arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
            _ => continue,
        };

        for scope in scopes {
            scope_map.insert(scope, (foreground.clone(), font_style.clone()));
        }
    }

    // Map from VS Code scope names to Zed syntax token names
    let scope_to_zed: &[(&[&str], &str)] = &[
        (&["comment"], "comment"),
        (&["comment.documentation"], "comment.doc"),
        (&["keyword"], "keyword"),
        (&["string"], "string"),
        (&["constant.numeric"], "number"),
        (&["constant.language"], "boolean"),
        (&["constant.regexp"], "string.regex"),
        (&["entity.name.function"], "function"),
        (
            &[
                "entity.name.type",
                "entity.name.class",
                "entity.name.enum",
                "entity.name.interface",
                "entity.name.namespace",
            ],
            "type",
        ),
        (&["variable"], "variable"),
        (&["entity.other.attribute-name"], "attribute"),
        (&["entity.name.tag"], "tag"),
        (&["support.type.property-name.json"], "property"),
        (&["markup.bold"], "emphasis.strong"),
        (&["markup.italic", "emphasis"], "emphasis"),
        (&["markup.heading"], "title"),
        (&["markup.underline"], "link_text"),
        (&["invalid"], "error"),
        (&["meta.embedded"], "embedded"),
    ];

    let mut syntax = serde_json::Map::new();

    for (vscode_scopes, zed_token) in scope_to_zed {
        // Find the first matching scope that has a foreground color
        for vscode_scope in *vscode_scopes {
            if let Some((foreground, font_style)) = scope_map.get(*vscode_scope) {
                let mut token_style = serde_json::Map::new();

                if let Some(color) = foreground {
                    token_style.insert(
                        "color".to_string(),
                        serde_json::Value::String(color.clone()),
                    );
                }

                if let Some(style) = font_style {
                    token_style.insert(
                        "font_style".to_string(),
                        serde_json::Value::String(style.clone()),
                    );
                } else {
                    token_style.insert("font_style".to_string(), serde_json::Value::Null);
                }

                token_style.insert("font_weight".to_string(), serde_json::Value::Null);

                if !token_style.is_empty() {
                    syntax.insert(
                        zed_token.to_string(),
                        serde_json::Value::Object(token_style),
                    );
                    break; // Use the first match
                }
            }
        }
    }

    if !syntax.contains_key("operator")
        && let Some((Some(color), _)) = scope_map.get("keyword")
    {
        syntax.insert(
            "operator".to_string(),
            serde_json::json!({
                "color": color,
                "font_style": null,
                "font_weight": null
            }),
        );
    }

    if !syntax.contains_key("punctuation")
        && let Some((Some(color), _)) = scope_map.get("meta.embedded")
    {
        syntax.insert(
            "punctuation".to_string(),
            serde_json::json!({
                "color": color,
                "font_style": null,
                "font_weight": null
            }),
        );
    }

    // Add constant token (use constant.language color)
    if let Some((Some(color), _)) = scope_map.get("constant.language") {
        syntax.insert(
            "constant".to_string(),
            serde_json::json!({
                "color": color,
                "font_style": null,
                "font_weight": null
            }),
        );
    }

    syntax
}

/// Generate Zed themes from VS Code BC theme files.
fn generate_themes(extension_path: &Path, output_dir: &Path) -> Result<()> {
    let themes_dir = extension_path.join("themes");
    if !themes_dir.is_dir() {
        anyhow::bail!("Theme directory not found: {}", themes_dir.display());
    }

    let mut zed_themes: Vec<serde_json::Value> = Vec::new();
    for file_name in ["BC_dark.json", "BC_light.json"] {
        let theme_path = themes_dir.join(file_name);
        println!("   Converting {file_name}");
        let raw = fs::read_to_string(&theme_path)
            .with_context(|| format!("Failed to read {}", theme_path.display()))?;
        let cleaned = strip_jsonc(&raw)?;
        let vscode_theme: serde_json::Value = serde_json::from_str(&cleaned)
            .with_context(|| format!("Failed to parse {}", theme_path.display()))?;
        zed_themes.push(convert_vscode_theme_to_zed(&vscode_theme)?);
    }

    let zed_theme_file = serde_json::json!({
        "name": "Business Central",
        "author": "Microsoft (converted)",
        "themes": zed_themes
    });

    fs::create_dir_all(output_dir).with_context(|| {
        format!(
            "Failed to create themes directory: {}",
            output_dir.display()
        )
    })?;

    let output_path = output_dir.join("bc-themes.json");
    let formatted = serde_json::to_string_pretty(&zed_theme_file)?;
    fs::write(&output_path, &formatted)
        .with_context(|| format!("Failed to write {}", output_path.display()))?;

    println!(
        "   Generated {} theme variant(s) -> {}",
        zed_themes.len(),
        output_path.display()
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Color manipulation helpers
// ---------------------------------------------------------------------------

/// Parse a hex color string (#RGB, #RRGGBB, or #RRGGBBAA) into (r, g, b) components.
fn parse_hex_color(hex: &str) -> Option<(u8, u8, u8)> {
    let hex = hex.trim_start_matches('#');
    match hex.len() {
        3 => {
            let r = u8::from_str_radix(&hex[0..1].repeat(2), 16).ok()?;
            let g = u8::from_str_radix(&hex[1..2].repeat(2), 16).ok()?;
            let b = u8::from_str_radix(&hex[2..3].repeat(2), 16).ok()?;
            Some((r, g, b))
        }
        6 | 8 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some((r, g, b))
        }
        _ => None,
    }
}

/// Lighten a hex color by a factor (0.0 = no change, 1.0 = white).
fn lighten_color(hex: &str, factor: f64) -> Result<String> {
    let (r, g, b) = parse_hex_color(hex).with_context(|| format!("Invalid color: {hex}"))?;
    let r = (r as f64 + (255.0 - r as f64) * factor).round() as u8;
    let g = (g as f64 + (255.0 - g as f64) * factor).round() as u8;
    let b = (b as f64 + (255.0 - b as f64) * factor).round() as u8;
    Ok(format!("#{:02X}{:02X}{:02X}", r, g, b))
}

/// Darken a hex color by a factor (0.0 = no change, 1.0 = black).
fn darken_color(hex: &str, factor: f64) -> Result<String> {
    let (r, g, b) = parse_hex_color(hex).with_context(|| format!("Invalid color: {hex}"))?;
    let r = (r as f64 * (1.0 - factor)).round() as u8;
    let g = (g as f64 * (1.0 - factor)).round() as u8;
    let b = (b as f64 * (1.0 - factor)).round() as u8;
    Ok(format!("#{:02X}{:02X}{:02X}", r, g, b))
}

/// Dim a hex color by blending towards gray with a given opacity factor.
fn dim_color(hex: &str, factor: f64) -> Result<String> {
    let (r, g, b) = parse_hex_color(hex).with_context(|| format!("Invalid color: {hex}"))?;
    let r = (r as f64 * factor).round() as u8;
    let g = (g as f64 * factor).round() as u8;
    let b = (b as f64 * factor).round() as u8;
    Ok(format!("#{:02X}{:02X}{:02X}", r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_roots_are_derived_from_the_manifest_directory() {
        let roots =
            project_roots(Path::new("checkout/extension/tree-sitter-al/generator")).unwrap();

        assert_eq!(
            roots,
            ProjectRoots {
                tree_sitter: PathBuf::from("checkout/extension/tree-sitter-al"),
                extension: PathBuf::from("checkout/extension"),
            }
        );
    }

    #[test]
    fn property_capture_uses_the_operator_scope() {
        let captures = std::collections::HashMap::from([
            (
                "keyword.other.property.al".to_string(),
                "@keyword".to_string(),
            ),
            (
                "keyword.operators.property.al".to_string(),
                "@operator".to_string(),
            ),
        ]);

        assert_eq!(
            capture_for_scope(&captures, "keyword.operators.property.al", "@operator"),
            "@operator"
        );
    }
}
