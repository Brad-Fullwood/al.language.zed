//! Every value the CLI prints as text goes through `terminal_text` or
//! `text_field`.
//!
//! Object, file and package names come from the project's files, from the
//! `.app` packages under `.alpackages` and from the BC debugger, so a name can
//! hold an escape sequence that clears the screen or renames the terminal
//! window. The two helpers in `src/cli/commands/mod.rs` write each control
//! character as its escape (`\u{1b}`). This test reads the CLI source and fails
//! on a print macro that formats a value the rule below does not accept, so a
//! new renderer cannot skip the helpers.
//!
//! The rule is read from the source text. Each value that `print!`,
//! `println!`, `eprint!`, `eprintln!`, `write!` or `writeln!` formats, and each
//! value a `format!` formats inside a renderer (a function whose name ends in
//! `_text` or `_line`), is one of:
//!
//! - a literal, `String::new()`, `env!("..")`, or a string literal's
//!   `.repeat(..)`, `.to_string()` or `.to_owned()`;
//! - a call to `terminal_text`, `text_field`, `terminal_lines` or a renderer;
//! - a count (`.len()`, `.count()`), a cast to a number type, a read of a JSON
//!   number or bool, or arithmetic on accepted values;
//! - a call to a function of the same file that returns `&'static str`, a
//!   number type, or an `Option` or `Result` of a number type, or a call to a
//!   closure whose `let` is accepted;
//! - an `if` or a `match` whose branches end in accepted values or leave with
//!   `return`, `continue`, `break` or a panic, or a `format!` whose values are
//!   accepted;
//! - JSON from `serde_json::to_string` or `to_string_pretty`, which writes a
//!   control character as `\u001b`;
//! - a name, or a method chain on a name, whose nearest binding in scope is a
//!   `let` or a parameter of a number or `bool` type, the index of an
//!   `.enumerate()` loop, a `for` loop over an array whose items are accepted
//!   at the name's place in the pattern, a `Some(..)` or `Ok(..)` match arm on
//!   a value that is accepted, or a `let` whose value is accepted or calls a
//!   helper or a renderer;
//! - an entry of `ALLOWED`, which names the file, the value and the reason.
//!
//! A renderer is trusted to build its text through the macros this test
//! checks. The rule reads text, so a `let` that calls a helper in one branch and
//! returns a raw string in the other passes, and a read of a JSON number is any
//! expression that calls `as_u64`, `as_i64`, `as_f64` or `as_bool` and not
//! `as_str`.

use std::path::{Path, PathBuf};

use regex::Regex;

/// Values printed as they are on purpose: the file under `src/cli`, the value
/// as written in the macro, and the reason.
const ALLOWED: &[(&str, &str, &str)] = &[
    (
        "commands/lsp/language.rs",
        "formatted",
        "format --stdin is a filter: it writes back the source the caller piped in, \
         formatted, and escaping would change that file",
    ),
    (
        "commands/trust.rs",
        "setting.display_line()",
        "PrivilegedSetting::display_line escapes the key, the value and the source \
         through al_project::trust::one_line",
    ),
];

/// A macro call whose formatted values the rule checks.
struct Site {
    line: usize,
    /// Where the macro starts: bindings are looked up above it.
    at: usize,
    /// Byte ranges of the formatted values in the file.
    values: Vec<(usize, usize)>,
}

/// One CLI source file: the text as written, and the same text with comments
/// and test modules blanked and the inside of every string and char literal
/// replaced by `_`, byte for byte, so offsets match and a quote, brace or
/// comma in a literal is not read as code.
struct File {
    source: String,
    code: String,
}

struct Rule {
    macro_call: Regex,
    function: Regex,
    literal: Regex,
    literal_method: Regex,
    helper_call: Regex,
    helper_anywhere: Regex,
    count: Regex,
    cast: Regex,
    number_read: Regex,
    number_default: Regex,
    match_keyword: Regex,
    leaves: Regex,
    call: Regex,
    json: Regex,
    number_type: Regex,
    name_chain: Regex,
    let_binding: Regex,
    for_binding: Regex,
    closure_params: Regex,
    arm_pattern: Regex,
    constant: Regex,
}

const NUMBER_TYPES: &str = "u8|u16|u32|u64|u128|usize|i8|i16|i32|i64|i128|isize|f32|f64|bool";

impl Rule {
    fn new() -> Self {
        let regex = |pattern: &str| Regex::new(pattern).unwrap();
        Rule {
            macro_call: regex(
                r"(?:^|[^\w:!])(print|println|eprint|eprintln|write|writeln|format)!\s*\(",
            ),
            function: regex(r"\bfn\s+(\w+)"),
            literal: regex(
                r##"^(?:"_*"|r#*"_*"#*|'_+'|-?\d[\w.]*|true|false|String::new\(\)|env!\("_*"\))$"##,
            ),
            literal_method: regex(r#"^"_*"\.(?:repeat|to_string|to_owned)\("#),
            helper_call: regex(
                r"^(?:\w+::)*(?:terminal_text|terminal_lines|text_field|\w+_text|\w+_line)\(",
            ),
            helper_anywhere: regex(
                r"\b(?:terminal_text|terminal_lines|text_field)\b|\b\w+_(?:text|line)\(",
            ),
            count: regex(r"\.(?:len|count)\(\)$"),
            cast: regex(&format!(r"\sas\s+(?:{NUMBER_TYPES})$")),
            number_read: regex(r"\bas_(?:u64|i64|f64|bool)\b"),
            number_default: regex(
                r"\.(?:unwrap_or\(\s*(?:-?\d[\w.]*|true|false)\s*|map_or\(\s*-?\d[\w.]*\s*,[^;]*)\)$",
            ),
            match_keyword: regex(r"\bmatch\s"),
            leaves: regex(
                r"^(?:return\b|continue\b|break\b|panic!|unreachable!|std::process::exit\b)",
            ),
            call: regex(r"^(\w+)\("),
            json: regex(r"^serde_json::to_string(?:_pretty)?\("),
            number_type: regex(&format!(r"^[\s(),]*(?:(?:{NUMBER_TYPES})[\s(),]*)+$")),
            name_chain: regex(r"^([a-z_][a-z0-9_]*)\s*(?:$|[.\[?])"),
            let_binding: regex(r"\blet\s+"),
            for_binding: regex(r"\bfor\s+(.+?)\s+in\s"),
            closure_params: regex(r"\|([^|;\n]*)\|"),
            arm_pattern: regex(r"\(([^()]*)\)\s*(?:if\b[^=]*)?=>"),
            constant: regex(r#"\bconst\s+(\w+)\s*:([^=;]*)=\s*([^;]*);"#),
        }
    }

    /// The print macros in `file`, and `format!` inside a renderer.
    fn sites(&self, file: &File) -> Vec<Site> {
        let mut sites = Vec::new();
        for captures in self.macro_call.captures_iter(&file.code) {
            let name = captures.get(1).unwrap();
            if name.as_str() == "format" && !self.inside_renderer(file, name.start()) {
                continue;
            }
            let open = captures.get(0).unwrap().end();
            let (args, _) = split_args(&file.code, open);
            let values = format_values(file, &args, name.as_str().starts_with("write"));
            let line = file.source[..name.start()].matches('\n').count() + 1;
            sites.push(Site {
                line,
                at: name.start(),
                values,
            });
        }
        sites
    }

    fn inside_renderer(&self, file: &File, at: usize) -> bool {
        self.function
            .captures_iter(&file.code[..at])
            .last()
            .map(|captures| {
                let name = captures.get(1).unwrap().as_str();
                name.ends_with("_text") || name.ends_with("_line")
            })
            .unwrap_or(false)
    }

    fn accepted(&self, file: &File, (start, end): (usize, usize), at: usize, depth: u8) -> bool {
        let (start, end) = trim_value(&file.code, start, end);
        let value = &file.code[start..end];
        if depth > 6 || value.is_empty() {
            return false;
        }
        let written = &file.source[start..end];
        if value != written && is_identifier(written) {
            // A name the format string captures, blanked in `code`.
            return self.binding_accepted(file, written, at, depth);
        }
        if self.literal.is_match(value)
            || self.literal_method.is_match(value)
            || self.count.is_match(value)
            || self.cast.is_match(value)
            || self.json.is_match(value)
            || (self.number_read.is_match(value) && !value.contains("as_str"))
            || self.number_default.is_match(value)
        {
            return true;
        }
        if self.helper_call.is_match(value) {
            let open = start + value.find('(').unwrap() + 1;
            let (_, close) = split_args(&file.code, open);
            let rest = file.code[close..end].trim_start();
            return rest.is_empty() || rest.starts_with('.') || rest.starts_with('?');
        }
        if let Some(inner) = strip_parens(&file.code, start, end) {
            return self.accepted(file, inner, at, depth + 1);
        }
        let parts = split_arithmetic(&file.code, start, end);
        if parts.len() > 1 {
            return parts
                .into_iter()
                .all(|part| self.accepted(file, part, at, depth + 1));
        }
        if let Some(branches) = if_branches(&file.code, start, end) {
            return branches
                .into_iter()
                .all(|branch| self.accepted(file, branch, at, depth + 1));
        }
        if let Some(arms) = match_arms(&file.code, start, end) {
            return arms
                .into_iter()
                .all(|body| self.arm_accepted(file, body, depth + 1));
        }
        if let Some(captures) = self.call.captures(value) {
            let name = &captures[1];
            let returns_safe = Regex::new(&format!(
                r"\bfn\s+{name}\s*\([^)]*\)\s*->\s*(?:&'static\s+str\b|(?:Option|Result)<\s*(?:{NUMBER_TYPES})\b|(?:{NUMBER_TYPES})\b)"
            ))
            .unwrap();
            return returns_safe.is_match(&file.code)
                || self.binding_accepted(file, name, at, depth + 1);
        }
        if let Some(rest) = value.strip_prefix("format!") {
            let open = end - rest.len() + rest.find('(').unwrap() + 1;
            let (args, _) = split_args(&file.code, open);
            return format_values(file, &args, false)
                .into_iter()
                .all(|range| self.accepted(file, range, at, depth + 1));
        }
        if let Some(captures) = self.name_chain.captures(value) {
            return self.binding_accepted(file, captures.get(1).unwrap().as_str(), at, depth);
        }
        false
    }

    /// Whether a match arm's body ends in an accepted value or leaves. The
    /// body's names are looked up where the body starts.
    fn arm_accepted(&self, file: &File, body: (usize, usize), depth: u8) -> bool {
        let (start, end) = trim_value(&file.code, body.0, body.1);
        let text = &file.code[start..end];
        if self.leaves.is_match(text) {
            return true;
        }
        if !text.starts_with('{') {
            return self.accepted(file, (start, end), start, depth);
        }
        let (inner_start, inner_end) = (start + 1, end - 1);
        let leaves = statements(&file.code, inner_start, inner_end)
            .into_iter()
            .any(|(from, to)| self.leaves.is_match(file.code[from..to].trim_start()));
        let last = last_expression(&file.code, inner_start, inner_end);
        leaves || self.accepted(file, last, last.0, depth)
    }

    /// The `match` whose block holds `pos`: the offset of its keyword and the
    /// range of the value it matches on.
    fn enclosing_match(&self, code: &str, pos: usize) -> Option<(usize, (usize, usize))> {
        let keywords: Vec<_> = self.match_keyword.find_iter(&code[..pos]).collect();
        keywords.into_iter().rev().find_map(|keyword| {
            let open = block_open(code, keyword.end())?;
            (open < pos && still_open(code, open, pos))
                .then_some((keyword.start(), (keyword.end(), open)))
        })
    }

    /// Whether the items of the array literal at `range` are accepted, or the
    /// item at `place` of each tuple in it when the loop pattern is a tuple.
    fn array_accepted(
        &self,
        file: &File,
        range: (usize, usize),
        place: Option<usize>,
        depth: u8,
    ) -> bool {
        let (start, end) = trim_value(&file.code, range.0, range.1);
        if !file.code[start..end].starts_with('[') {
            return false;
        }
        let (items, close) = split_args(&file.code, start + 1);
        if close != end {
            return false;
        }
        items.into_iter().all(|item| {
            let item = trim_value(&file.code, item.0, item.1);
            match place {
                None => self.accepted(file, item, item.0, depth + 1),
                Some(place) => {
                    file.code[item.0..item.1].starts_with('(')
                        && split_args(&file.code, item.0 + 1)
                            .0
                            .get(place)
                            .is_some_and(|&part| self.accepted(file, part, part.0, depth + 1))
                }
            }
        })
    }

    /// The value of the nearest `let name = ..` above `at` that is in scope
    /// there.
    fn let_value(&self, file: &File, name: &str, at: usize) -> Option<(usize, usize)> {
        self.let_binding
            .find_iter(&file.code[..at])
            .filter_map(|found| {
                let binding = let_parts(&file.code, found.end())?;
                let simple = binding.pattern.trim_start_matches("mut ").trim() == name;
                (simple && !binding.conditional && in_scope(&file.code, found.start(), at))
                    .then_some(binding.value)
            })
            .last()
    }

    /// Whether the nearest binding of `name` above `at` that is still in
    /// scope there proves the value safe.
    fn binding_accepted(&self, file: &File, name: &str, at: usize, depth: u8) -> bool {
        let code = &file.code[..at];
        let word = Regex::new(&format!(r"\b{name}\b")).unwrap();
        let mut nearest: Option<(usize, bool)> = None;
        let mut consider = |position: usize, accepted: bool| {
            if nearest.is_none_or(|(best, _)| position > best) {
                nearest = Some((position, accepted));
            }
        };

        for found in self.let_binding.find_iter(code) {
            let Some(binding) = let_parts(&file.code, found.end()) else {
                continue;
            };
            if !word.is_match(binding.pattern) || !in_scope(&file.code, found.start(), at) {
                continue;
            }
            if binding.conditional && !block_encloses(&file.code, binding.value.1, at) {
                continue;
            }
            let simple = binding.pattern.trim_start_matches("mut ").trim() == name;
            let value = &file.code[binding.value.0..binding.value.1];
            let accepted = binding.ty.is_some_and(|ty| self.number_type.is_match(ty))
                || self.helper_anywhere.is_match(value)
                || (self.number_read.is_match(value) && !value.contains("as_str"))
                || (simple && self.accepted(file, binding.value, at, depth + 1));
            consider(found.start(), accepted);
        }

        for captures in self.for_binding.captures_iter(code) {
            let pattern = captures.get(1).unwrap();
            if !word.is_match(pattern.as_str()) {
                continue;
            }
            let header_end = captures.get(0).unwrap().end();
            if !block_encloses(&file.code, header_end, at) {
                continue;
            }
            let body = file.code[header_end..].find('{').map(|i| header_end + i);
            let iterable = &file.code[header_end..body.unwrap_or(header_end)];
            let index = pattern
                .as_str()
                .strip_prefix('(')
                .and_then(|rest| rest.split(',').next())
                .is_some_and(|first| first.trim() == name);
            // The loop runs over an array written in place or bound by a `let`.
            let place = pattern.as_str().trim().strip_prefix('(').map(|rest| {
                rest.trim_end_matches(')')
                    .split(',')
                    .position(|part| part.trim().trim_start_matches(['&', ' ']) == name)
            });
            let iterable_range = trim_value(&file.code, header_end, body.unwrap_or(header_end));
            let iterable_text = file.code[iterable_range.0..iterable_range.1]
                .trim_end_matches(".iter()")
                .to_string();
            let array = if iterable_text.starts_with('[') {
                Some(iterable_range)
            } else if is_identifier(&iterable_text) {
                self.let_value(file, &iterable_text, header_end)
            } else {
                None
            };
            let array_accepted = match (array, place) {
                (Some(array), None) => self.array_accepted(file, array, None, depth),
                (Some(array), Some(Some(place))) => {
                    self.array_accepted(file, array, Some(place), depth)
                }
                _ => false,
            };
            let accepted =
                (index && iterable.trim_end().ends_with(".enumerate()")) || array_accepted;
            consider(pattern.start(), accepted);
        }

        for captures in self.closure_params.captures_iter(code) {
            let pattern = captures.get(1).unwrap();
            let after = captures.get(0).unwrap().end();
            if word.is_match(pattern.as_str()) && in_scope(&file.code, after, at) {
                // A closure parameter written with a number type passes.
                let typed_number = pattern.as_str().split(',').any(|param| {
                    param.split_once(':').is_some_and(|(param, ty)| {
                        param.trim() == name && self.number_type.is_match(ty)
                    })
                });
                consider(pattern.start(), typed_number);
            }
        }

        for captures in self.arm_pattern.captures_iter(code) {
            let pattern = captures.get(1).unwrap();
            let after = captures.get(0).unwrap().end();
            if word.is_match(pattern.as_str()) && in_scope(&file.code, after, at) {
                // A name a `Some(..)` or `Ok(..)` arm binds out of a value the
                // rule accepts. An `Err(..)` arm holds something else.
                let variant = file.code[..captures.get(0).unwrap().start()].trim_end();
                let accepted = (variant.ends_with("Some") || variant.ends_with("Ok"))
                    && self
                        .enclosing_match(&file.code, pattern.start())
                        .is_some_and(|(keyword, value)| {
                            self.accepted(file, value, keyword, depth + 1)
                        });
                consider(pattern.start(), accepted);
            }
        }

        if let Some(function) = self.function.find_iter(code).last() {
            let open = function.end() + file.code[function.end()..].find('(').unwrap() + 1;
            let (params, close) = split_args(&file.code, open);
            if close <= at {
                for (start, end) in params {
                    let param = &file.code[start..end];
                    if let Some((pattern, ty)) = param.split_once(':')
                        && pattern.trim().trim_start_matches("mut ").trim() == name
                    {
                        consider(start, self.number_type.is_match(ty));
                    }
                }
            }
        }

        if nearest.is_none() {
            // A `const` of a number type or holding a string literal.
            return self.constant.captures_iter(&file.code).any(|captures| {
                &captures[1] == name
                    && (self.number_type.is_match(&captures[2])
                        || self.literal.is_match(captures[3].trim()))
            });
        }
        nearest.is_some_and(|(_, accepted)| accepted)
    }
}

struct LetParts<'a> {
    pattern: &'a str,
    ty: Option<&'a str>,
    value: (usize, usize),
    /// An `if let`, `while let` or a `let` in a chain: its names live in the
    /// block that follows.
    conditional: bool,
}

/// The pattern, type and value of the `let` whose pattern starts at `from`.
fn let_parts(code: &str, from: usize) -> Option<LetParts<'_>> {
    let bytes = code.as_bytes();
    let mut depth = 0i32;
    let mut i = from;
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'[' | b'{' | b'<' => depth += 1,
            b')' | b']' | b'}' | b'>' => depth -= 1,
            b';' => return None,
            b'=' if depth == 0 => {
                let next = bytes.get(i + 1).copied();
                let previous = bytes[i - 1];
                if next != Some(b'=') && next != Some(b'>') && !b"!<>=".contains(&previous) {
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    let head = &code[from..i];
    let (pattern, ty) = match top_level_colon(head) {
        Some(colon) => (&head[..colon], Some(&head[colon + 1..])),
        None => (head, None),
    };
    let conditional = {
        let before = code[..from].trim_end();
        let before = before.strip_suffix("let").unwrap_or(before).trim_end();
        before.ends_with("if") || before.ends_with("while") || before.ends_with("&&")
    };
    let start = i + 1;
    let mut depth = 0i32;
    let mut end = start;
    while end < bytes.len() {
        match bytes[end] {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            b'{' if depth == 0 && conditional => break,
            b'{' => depth += 1,
            b'}' => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            b';' if depth == 0 => break,
            _ => {}
        }
        end += 1;
    }
    Some(LetParts {
        pattern,
        ty,
        value: (start, end),
        conditional,
    })
}

fn top_level_colon(head: &str) -> Option<usize> {
    let bytes = head.as_bytes();
    let mut depth = 0i32;
    for (i, byte) in bytes.iter().enumerate() {
        match byte {
            b'(' | b'[' | b'{' | b'<' => depth += 1,
            b')' | b']' | b'}' | b'>' => depth -= 1,
            b':' if depth == 0
                && bytes.get(i + 1) != Some(&b':')
                && (i == 0 || bytes[i - 1] != b':') =>
            {
                return Some(i);
            }
            _ => {}
        }
    }
    None
}

/// Whether the block, closure or match arm a binding at `from` lives in is
/// still open at `at`: the bracket depth never drops below where it started.
fn in_scope(code: &str, from: usize, at: usize) -> bool {
    let mut depth = 0i32;
    for byte in code[from..at].bytes() {
        match byte {
            b'{' | b'(' | b'[' => depth += 1,
            b'}' | b')' | b']' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

/// Whether the first block that opens after `from` is still open at `at`.
fn block_encloses(code: &str, from: usize, at: usize) -> bool {
    let Some(open) = code[from..at].find('{').map(|i| from + i) else {
        return false;
    };
    let mut depth = 0i32;
    for byte in code[open..at].bytes() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

/// The arguments of the call whose `(` ends at `open`, as byte ranges, and
/// the offset just past its `)`.
fn split_args(code: &str, open: usize) -> (Vec<(usize, usize)>, usize) {
    let bytes = code.as_bytes();
    let mut args = Vec::new();
    let mut depth = 0i32;
    let mut start = open;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' if depth == 0 => {
                if !code[start..i].trim().is_empty() {
                    args.push((start, i));
                }
                return (args, i + 1);
            }
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                args.push((start, i));
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    (args, i)
}

/// The values a format macro formats: the names its format string captures
/// and every argument after it. `skip_target` drops the writer of `write!`.
fn format_values(file: &File, args: &[(usize, usize)], skip_target: bool) -> Vec<(usize, usize)> {
    let args = if skip_target && !args.is_empty() {
        &args[1..]
    } else {
        args
    };
    let Some(&(format_start, format_end)) = args.first() else {
        return Vec::new();
    };
    let (format_start, format_end) = trim_value(&file.code, format_start, format_end);
    let mut values = Vec::new();
    let format_code = &file.code[format_start..format_end];
    if !(format_code.starts_with('"') || format_code.starts_with('r')) {
        values.push((format_start, format_end));
    }
    let mut named = Vec::new();
    for &(start, end) in &args[1..] {
        let (start, end) = trim_value(&file.code, start, end);
        let text = &file.code[start..end];
        match text.split_once('=') {
            Some((name, rest)) if is_identifier(name.trim()) && !rest.starts_with('=') => {
                named.push(name.trim().to_string());
                values.push((end - rest.len(), end));
            }
            _ => values.push((start, end)),
        }
    }
    let format = &file.source[format_start..format_end];
    let mut chars = format.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        if (ch == '{' || ch == '}') && chars.peek().map(|&(_, next)| next) == Some(ch) {
            chars.next();
            continue;
        }
        if ch != '{' {
            continue;
        }
        let Some(close) = format[i..].find('}') else {
            break;
        };
        let inner = &format[i + 1..i + close];
        let argument = inner.split(':').next().unwrap().trim();
        if is_identifier(argument) && !named.iter().any(|name| name == argument) {
            let start = format_start + i + 1 + inner.find(argument).unwrap();
            values.push((start, start + argument.len()));
        }
    }
    values
}

fn is_identifier(text: &str) -> bool {
    text.chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && text
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

/// The range without surrounding whitespace, `&` and `*`.
fn trim_value(code: &str, mut start: usize, mut end: usize) -> (usize, usize) {
    let bytes = code.as_bytes();
    while start < end && (bytes[start].is_ascii_whitespace() || b"&*".contains(&bytes[start])) {
        start += 1;
    }
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    (start, end)
}

/// The inside of `( .. )` when the parentheses wrap the whole value.
fn strip_parens(code: &str, start: usize, end: usize) -> Option<(usize, usize)> {
    if !code[start..end].starts_with('(') {
        return None;
    }
    let (_, close) = split_args(code, start + 1);
    (close == end).then_some((start + 1, end - 1))
}

/// The operands of a top-level `+`, `-`, `*` or `/`, written with a space on
/// each side as rustfmt writes them.
fn split_arithmetic(code: &str, start: usize, end: usize) -> Vec<(usize, usize)> {
    let bytes = code.as_bytes();
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut part_start = start;
    let mut i = start;
    while i < end {
        match bytes[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'+' | b'-' | b'*' | b'/'
                if depth == 0
                    && i > start
                    && bytes[i - 1] == b' '
                    && bytes.get(i + 1) == Some(&b' ') =>
            {
                parts.push((part_start, i - 1));
                part_start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    parts.push((part_start, end));
    parts
}

/// The first `{` after `from` outside parentheses and brackets, before any
/// `;`.
fn block_open(code: &str, from: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (i, byte) in code.bytes().enumerate().skip(from) {
        match byte {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            b'{' if depth == 0 => return Some(i),
            b';' if depth == 0 => return None,
            _ => {}
        }
    }
    None
}

/// Whether the block whose `{` is at `open` is still open at `at`.
fn still_open(code: &str, open: usize, at: usize) -> bool {
    let mut depth = 0i32;
    for byte in code[open..at].bytes() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

/// The body of each arm of a `match` that is the whole value.
fn match_arms(code: &str, start: usize, end: usize) -> Option<Vec<(usize, usize)>> {
    let rest = code[start..end].strip_prefix("match")?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let open = block_open(code, start + "match".len())?;
    let (_, close) = split_args(code, open + 1);
    if close != end {
        return None;
    }
    let bytes = code.as_bytes();
    let inner_end = close - 1;
    let mut arms = Vec::new();
    let mut i = open + 1;
    loop {
        let mut depth = 0i32;
        let mut arrow = None;
        for j in i..inner_end.saturating_sub(1) {
            match bytes[j] {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                b'=' if depth == 0 && bytes[j + 1] == b'>' => {
                    arrow = Some(j);
                    break;
                }
                _ => {}
            }
        }
        let Some(arrow) = arrow else {
            return Some(arms);
        };
        let body_start = arrow + 2;
        let first = body_start
            + code[body_start..inner_end]
                .find(|ch: char| !ch.is_whitespace())
                .unwrap_or(inner_end - body_start);
        if bytes.get(first) == Some(&b'{') {
            let (_, after) = split_args(code, first + 1);
            arms.push((first, after));
            i = after;
            continue;
        }
        let (parts, _) = split_args(code, body_start);
        let body_end = parts.first().map_or(inner_end, |&(_, part_end)| part_end);
        arms.push((body_start, body_end));
        i = body_end + 1;
    }
}

/// The statements of the block between `start` and `end`, split at each `;`
/// outside brackets.
fn statements(code: &str, start: usize, end: usize) -> Vec<(usize, usize)> {
    let bytes = code.as_bytes();
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut from = start;
    for i in start..end {
        match bytes[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b';' if depth == 0 => {
                parts.push((from, i));
                from = i + 1;
            }
            _ => {}
        }
    }
    parts.push((from, end));
    parts
}

/// The last expression of each branch of an `if .. else ..` that is the
/// whole value.
fn if_branches(code: &str, start: usize, end: usize) -> Option<Vec<(usize, usize)>> {
    let mut rest = start;
    let mut branches = Vec::new();
    loop {
        if !code[rest..end].starts_with("if ") {
            return None;
        }
        let open = rest + code[rest..end].find('{')? + 1;
        let (_, close) = split_args(code, open);
        branches.push(last_expression(code, open, close - 1));
        let after = code[close..end].trim_start();
        let after_start = end - after.len();
        let else_rest = after.strip_prefix("else")?.trim_start();
        let else_start = end - else_rest.len();
        if else_rest.starts_with('{') {
            let (_, close) = split_args(code, else_start + 1);
            if close != end {
                return None;
            }
            branches.push(last_expression(code, else_start + 1, close - 1));
            return Some(branches);
        }
        if after_start >= end {
            return None;
        }
        rest = else_start;
    }
}

fn last_expression(code: &str, start: usize, end: usize) -> (usize, usize) {
    let bytes = code.as_bytes();
    let mut depth = 0i32;
    let mut last = start;
    for i in start..end {
        match bytes[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b';' if depth == 0 => last = i + 1,
            _ => {}
        }
    }
    (last, end)
}

/// `source` with comments blanked and the inside of string and char literals
/// replaced by `_`, byte for byte.
fn code_only(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = bytes.to_vec();
    let identifier = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let mut i = 0;
    while i < bytes.len() {
        let previous_is_identifier = i > 0 && identifier(bytes[i - 1]);
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    out[i] = b' ';
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let end = source[i + 2..]
                    .find("*/")
                    .map_or(bytes.len(), |j| i + 2 + j + 2);
                for byte in &mut out[i..end] {
                    if *byte != b'\n' {
                        *byte = b' ';
                    }
                }
                i = end;
            }
            b'"' => {
                let mut j = i + 1;
                while j < bytes.len() && bytes[j] != b'"' {
                    j += if bytes[j] == b'\\' { 2 } else { 1 };
                }
                out[i + 1..j].fill(b'_');
                i = j + 1;
            }
            b'r' if !previous_is_identifier
                && matches!(bytes.get(i + 1), Some(b'"') | Some(b'#')) =>
            {
                let hashes = bytes[i + 1..].iter().take_while(|&&b| b == b'#').count();
                if bytes.get(i + 1 + hashes) != Some(&b'"') {
                    i += 1;
                    continue;
                }
                let open = i + 1 + hashes + 1;
                let closing = format!("\"{}", "#".repeat(hashes));
                let close = source[open..]
                    .find(&closing)
                    .map_or(bytes.len(), |j| open + j);
                out[open..close].fill(b'_');
                i = close + closing.len();
            }
            b'\'' => {
                let char_end = if bytes.get(i + 1) == Some(&b'\\') {
                    source[i + 2..].find('\'').map(|j| i + 2 + j)
                } else {
                    source[i + 1..]
                        .chars()
                        .next()
                        .map(|ch| i + 1 + ch.len_utf8())
                        .filter(|&j| bytes.get(j) == Some(&b'\''))
                };
                match char_end {
                    Some(close) => {
                        out[i + 1..close].fill(b'_');
                        i = close + 1;
                    }
                    None => i += 1,
                }
            }
            _ => i += 1,
        }
    }
    String::from_utf8(out).unwrap()
}

/// `code` with each `#[cfg(test)]` item blanked.
fn without_test_modules(code: &str) -> String {
    let mut out = code.as_bytes().to_vec();
    for (start, _) in code.match_indices("#[cfg(test)]") {
        let Some(open) = code[start..].find(['{', ';']).map(|i| start + i) else {
            continue;
        };
        let end = if code.as_bytes()[open] == b'{' {
            split_args(code, open + 1).1
        } else {
            open + 1
        };
        for byte in &mut out[start..end] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    }
    String::from_utf8(out).unwrap()
}

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(rust_files(&path));
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
    files.sort();
    files
}

/// Each print site in `source` with the values the rule does not accept, as
/// the line and the values as written.
fn raw_values(rule: &Rule, source: &str) -> Vec<(usize, Vec<String>)> {
    let code = without_test_modules(&code_only(source));
    let file = File {
        source: source.to_string(),
        code,
    };
    let mut sites = Vec::new();
    for site in rule.sites(&file) {
        let raw: Vec<String> = site
            .values
            .iter()
            .filter(|&&range| !rule.accepted(&file, range, site.at, 0))
            .map(|&(start, end)| {
                let (start, end) = trim_value(&file.code, start, end);
                file.source[start..end].to_string()
            })
            .collect();
        if !raw.is_empty() {
            sites.push((site.line, raw));
        }
    }
    sites
}

#[test]
fn every_value_the_cli_prints_as_text_passes_through_the_escaping_helpers() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/cli");
    let rule = Rule::new();
    let mut raw_sites = Vec::new();
    let mut allowed_used = vec![false; ALLOWED.len()];
    for path in rust_files(&root) {
        let name = path
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let source = std::fs::read_to_string(&path).unwrap();
        for (line, values) in raw_values(&rule, &source) {
            let mut raw = Vec::new();
            for value in values {
                let allowed = ALLOWED
                    .iter()
                    .position(|&(file, written, _)| file == name && written == value);
                match allowed {
                    Some(index) => allowed_used[index] = true,
                    None => raw.push(value),
                }
            }
            if !raw.is_empty() {
                raw_sites.push(format!("{name}:{line}: {}", raw.join(" | ")));
            }
        }
    }
    assert!(
        raw_sites.is_empty(),
        "{} print sites format a value that does not pass through terminal_text or \
         text_field (see the rule at the top of this file):\n{}",
        raw_sites.len(),
        raw_sites.join("\n")
    );
    let stale: Vec<_> = ALLOWED
        .iter()
        .zip(&allowed_used)
        .filter(|(_, used)| !**used)
        .map(|((file, value, _), _)| format!("{file}: {value}"))
        .collect();
    assert!(
        stale.is_empty(),
        "ALLOWED entries that match no print site: {stale:#?}"
    );
}

#[test]
fn the_rule_flags_raw_names_and_accepts_escaped_names_and_numbers() {
    let source = r#"
fn render(row: &serde_json::Value, depth: usize) {
    let name = row.get("name").and_then(|v| v.as_str()).unwrap_or("?");
    let shown = text_field(row, "name", "?");
    let line = row.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
    println!("{name} {shown} {line} {depth} {}", "-".repeat(3));
    if depth > 0 {
        let name = terminal_text(name);
        println!("{name}");
    }
    eprintln!("{}: {}", name, rows.len() + 1);
    for (i, item) in rows.iter().enumerate() {
        println!("{i} {item}");
    }
    match row.get("x") {
        Some(found) => println!("{found}"),
        None => {}
    }
    // println!("{name}") in a comment is not code.
    println!("{{name}} is not a capture");
}

fn row_text(row: &serde_json::Value) -> String {
    let raw = row["kind"].as_str().unwrap_or("?");
    let escaped = format!("{}", terminal_text(raw));
    format!("{raw} {escaped}")
}

fn row_json(row: &serde_json::Value) -> String {
    format!("{}", row["kind"].as_str().unwrap_or("?"))
}
"#;
    let raw = raw_values(&Rule::new(), source);
    let expected: Vec<(usize, Vec<String>)> = vec![
        (6, vec!["name".into()]),
        (11, vec!["name".into()]),
        (13, vec!["item".into()]),
        (16, vec!["found".into()]),
        (26, vec!["raw".into()]),
    ];
    assert_eq!(raw, expected);
}

#[test]
fn the_rule_follows_matches_loops_and_closures_to_the_values_they_bind() {
    let source = r#"
fn count(row: &serde_json::Value) -> Option<u64> {
    row.get("count").and_then(|v| v.as_u64())
}

fn render(row: &serde_json::Value) {
    let version = env!("CARGO_PKG_VERSION");
    let errors = match count(row) {
        Some(errors) => errors,
        None => {
            return;
        }
    };
    let numbers = |value: &serde_json::Value| value.as_i64().unwrap_or(0).to_string();
    let labels = |value: &serde_json::Value| value.as_str().unwrap_or("?").to_string();
    println!("{version} {errors} {} {}", numbers(row), labels(row));
    let checks = [("ALTool", row.get("a").is_some()), ("Project", true)];
    for (name, ok) in &checks {
        println!("{name} {ok}");
    }
    let names = [row.get("n").and_then(|v| v.as_str()).unwrap_or("?")];
    for name in names {
        println!("{name}");
    }
    let score = row.get("score").and_then(|v| v.as_f64());
    match score {
        Some(score) => println!("{score:.1}"),
        None => {}
    }
    match serde_json::to_string(row) {
        Ok(json) => println!("{json}"),
        Err(error) => eprintln!("{error}"),
    }
    match row.get("kind").and_then(|v| v.as_str()) {
        Some(kind) => println!("{kind}"),
        None => {}
    }
}
"#;
    let raw = raw_values(&Rule::new(), source);
    let expected: Vec<(usize, Vec<String>)> = vec![
        (16, vec!["labels(row)".into()]),
        (19, vec!["ok".into()]),
        (23, vec!["name".into()]),
        (32, vec!["error".into()]),
        (35, vec!["kind".into()]),
    ];
    assert_eq!(raw, expected);
}
