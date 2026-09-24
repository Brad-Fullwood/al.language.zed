//! Translatable text from AL source: captions, tooltips, labels, with
//! `alc`'s translation-unit IDs.

use super::*;

/// Extract all translatable text units from the workspace AL files.
///
/// Scans for `Caption`, `ToolTip`, and `Label` properties in all workspace .al files.
/// Returns units in the order they appear (deterministic output).
pub fn extract_translation_units(workspace: &Workspace) -> Vec<TranslationUnit> {
    let mut units = Vec::new();

    let mut paths: Vec<_> = workspace
        .file_index
        .files
        .iter()
        .map(|e| e.key().clone())
        .collect();
    paths.sort();

    for path in &paths {
        let text = match workspace.file_index.files.get(path) {
            Some(t) => t.value().clone(),
            None => continue,
        };
        extract_from_file(path, &text, &mut units);
    }

    // A `.g.xlf` with duplicate `trans-unit id=` values silently collapses to
    // one entry in `parse_xliff`'s map, so refresh/untranslated/suggest would
    // operate on corrupted data. Keep the first occurrence and warn.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    units.retain(|unit| {
        if seen.insert(unit.id.clone()) {
            return true;
        }
        tracing::warn!(
            id = %unit.id,
            source = %unit.source,
            "xliff: dropping duplicate translation-unit id"
        );
        false
    });

    units
}

/// Extract translation units from a single AL file.
///
/// The scan is structural: brace nesting decides which member (table field,
/// page control, page action) a `Caption`/`ToolTip` belongs to. The previous
/// line-oriented scan tracked only a numeric `field_id` that page-layout
/// members never set and that was never reset when a field block ended, so
/// every page control produced the *same* trans-unit id and object-level
/// properties were attributed to the last field seen.
pub(super) fn extract_from_file(path: &Path, text: &str, units: &mut Vec<TranslationUnit>) {
    let _ = path;
    let mut object: Option<ObjectContext> = None;
    // One frame per open brace.
    let mut stack: Vec<Frame> = Vec::new();
    let mut pending: Option<MemberBlock> = None;
    let mut in_block_comment = false;

    for line in text.lines() {
        let code = strip_for_structure(line, &mut in_block_comment);
        let mut segment_start = 0usize;
        for (index, ch) in code.char_indices() {
            if ch != '{' && ch != '}' {
                continue;
            }
            scan_segment(
                &line[segment_start..index],
                &code[segment_start..index],
                &mut object,
                &stack,
                &mut pending,
                units,
            );
            match ch {
                '{' => {
                    // `Action` is what alc emits for a group inside `actions`,
                    // so the flag has to reach every descendant, not just the
                    // `actions` marker block itself.
                    let in_actions = stack.last().is_some_and(|frame| frame.in_actions)
                        || matches!(pending, Some(MemberBlock { actions_marker, .. }) if actions_marker);
                    let member = pending.take().filter(|member| !member.actions_marker);
                    stack.push(Frame { member, in_actions });
                }
                _ => {
                    stack.pop();
                    if stack.is_empty() {
                        // The object closed: a file may hold several.
                        object = None;
                    }
                }
            }
            segment_start = index + ch.len_utf8();
        }
        scan_segment(
            &line[segment_start..],
            &code[segment_start..],
            &mut object,
            &stack,
            &mut pending,
            units,
        );
    }
}

/// The object a translation unit belongs to.
pub(super) struct ObjectContext {
    header: ObjectHeader,
    name_hash: i64,
}

/// One open brace: the member it declares, if any, and whether it is under an
/// `actions` section.
pub(super) struct Frame {
    member: Option<MemberBlock>,
    in_actions: bool,
}

/// Process one brace-free run of a line: an object header, a member header,
/// and every `;`-terminated statement in it.
///
/// Working in segments rather than whole lines is what makes a one-line member
/// (`field(1; "No."; Code[20]) { Caption = 'No.'; }`) resolve against the field
/// it declares: the brace between the two has already been applied to `stack`
/// by the time the caption is read.
pub(super) fn scan_segment(
    raw: &str,
    code: &str,
    object: &mut Option<ObjectContext>,
    stack: &[Frame],
    pending: &mut Option<MemberBlock>,
    units: &mut Vec<TranslationUnit>,
) {
    if stack.is_empty() {
        if let Some(header) = parse_object_header_line(code.trim()) {
            *object = Some(ObjectContext {
                name_hash: name_hash(&header.name),
                header,
            });
        }
        return;
    }
    let Some(object) = object.as_ref() else {
        return;
    };

    if let Some(member) = parse_member_block(code.trim()) {
        *pending = Some(member);
    }

    let anchor = stack.iter().rev().find_map(|frame| {
        frame
            .member
            .as_ref()
            .map(|member| (member, frame.in_actions))
    });

    for (statement, stripped) in statements(raw, code) {
        if property_is_locked(stripped) {
            continue;
        }
        emit_statement_units(statement, object, anchor, units);
    }
}

/// Split a segment into `;`-terminated statements, pairing each with the
/// same span of the structure-stripped text. Both strings have the same byte
/// length, so one index serves both.
pub(super) fn statements<'a>(raw: &'a str, code: &'a str) -> Vec<(&'a str, &'a str)> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (index, ch) in code.char_indices() {
        if ch == ';' {
            out.push((raw[start..index].trim(), code[start..index].trim()));
            start = index + ch.len_utf8();
        }
    }
    out.push((raw[start..].trim(), code[start..].trim()));
    out.into_iter().filter(|(raw, _)| !raw.is_empty()).collect()
}

pub(super) fn emit_statement_units(
    statement: &str,
    object: &ObjectContext,
    anchor: Option<(&MemberBlock, bool)>,
    units: &mut Vec<TranslationUnit>,
) {
    let obj_type = &object.header.kind_display;
    let obj_name = &object.header.name;
    let object_hash = object.name_hash;

    for property in ["Caption", "ToolTip"] {
        let Some(value) = parse_property_value(statement, property) else {
            continue;
        };
        let (id, note) = match anchor {
            Some((member, in_actions)) => {
                let kind = member.id_kind(&object.header, in_actions);
                (
                    format!(
                        "{obj_type} {object_hash} - {kind} {} - Property {}",
                        name_hash(&member.name),
                        name_hash(property)
                    ),
                    format!(
                        "{obj_type} {obj_name} - {kind} {} - Property {property}",
                        member.name
                    ),
                )
            }
            None => (
                format!(
                    "{obj_type} {object_hash} - Property {}",
                    name_hash(property)
                ),
                format!("{obj_type} {obj_name} - Property {property}"),
            ),
        };
        let mut unit =
            make_translation_unit(id, obj_type, object.header.id, obj_name, value, Some(note));
        unit.developer_note = statement_comment(statement);
        units.push(unit);
    }

    // `MyLabel: Label 'text';` — alc keys labels by the NamedType name.
    if let Some((label_name, label_text)) = parse_label_declaration(statement) {
        let id = format!(
            "{obj_type} {object_hash} - NamedType {}",
            name_hash(&label_name)
        );
        let mut unit = make_translation_unit(
            id,
            obj_type,
            object.header.id,
            obj_name,
            label_text,
            Some(format!("{obj_type} {obj_name} - NamedType {label_name}")),
        );
        unit.developer_note = statement_comment(statement);
        units.push(unit);
    }
}

/// The `Comment = '...'` that follows the translatable literal of a
/// property or label statement.
pub(super) fn statement_comment(statement: &str) -> Option<String> {
    // Skip the first literal: the text itself may contain "Comment".
    let first = statement.find('\'')?;
    let mut end = None;
    let bytes = statement.as_bytes();
    let mut index = first + 1;
    while index < bytes.len() {
        if bytes[index] == b'\'' {
            if bytes.get(index + 1) == Some(&b'\'') {
                index += 2;
                continue;
            }
            end = Some(index);
            break;
        }
        index += 1;
    }
    let rest = &statement[end? + 1..];
    let lower = rest.to_ascii_lowercase();
    let mut from = 0;
    while let Some(found) = lower[from..].find("comment") {
        let at = from + found;
        let after = rest[at + "comment".len()..].trim_start();
        if let Some(value) = after.strip_prefix('=') {
            let preceded_by_word = rest[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
            if !preceded_by_word {
                return extract_single_quoted(value).filter(|text| !text.is_empty());
            }
        }
        from = at + "comment".len();
    }
    None
}

/// A named member block (`field(…)`, `action(…)`, `group(…)`, …) whose
/// properties are translated relative to it.
#[derive(Debug, Clone)]
pub(super) struct MemberBlock {
    /// Lower-cased declaration keyword.
    keyword: String,
    /// Member name used in the translation id.
    name: String,
    /// The unnamed `actions` section marker, which anchors nothing itself but
    /// makes every block inside it an action.
    actions_marker: bool,
}

impl MemberBlock {
    /// alc's id component for this member: `Field` for a table field,
    /// `Action` for anything under `actions`, `Control` otherwise.
    fn id_kind(&self, header: &ObjectHeader, in_actions: bool) -> &'static str {
        if in_actions || self.keyword == "action" || self.keyword == "actionref" {
            "Action"
        } else if header.is_table_like && self.keyword == "field" {
            "Field"
        } else {
            "Control"
        }
    }
}

/// Declaration keywords that open a *named* member block.
pub(super) const MEMBER_BLOCK_KEYWORDS: &[&str] = &[
    "field",
    "action",
    "actionref",
    "group",
    "part",
    "systempart",
    "usercontrol",
    "label",
    "repeater",
    "cuegroup",
    "fixed",
    "grid",
    "dataitem",
    "column",
];

/// Parse a `keyword(args)` member header, or the bare `actions` section.
pub(super) fn parse_member_block(trimmed: &str) -> Option<MemberBlock> {
    let lower_head = trimmed
        .split(|c: char| c == '(' || c.is_whitespace() || c == '{')
        .next()
        .unwrap_or("")
        .to_lowercase();
    if lower_head == "actions" {
        return Some(MemberBlock {
            keyword: "actions".to_string(),
            name: String::new(),
            actions_marker: true,
        });
    }
    let open = trimmed.find('(')?;
    let keyword = trimmed[..open].trim().to_lowercase();
    if !MEMBER_BLOCK_KEYWORDS.contains(&keyword.as_str()) {
        return None;
    }
    let close = trimmed.rfind(')')?;
    if close < open {
        return None;
    }
    let args: Vec<&str> = trimmed[open + 1..close].split(';').collect();
    // `field(1; Name; Text[50])` (table) vs `field(Name; Rec.Name)` (page):
    // the name is the second segment only when the first is a numeric id.
    let first = args.first().map(|a| a.trim()).unwrap_or("");
    let name_part = if first.parse::<u32>().is_ok() && args.len() >= 2 {
        args[1].trim()
    } else {
        first
    };
    let name = name_part.trim_matches('"').trim_matches('\'').trim();
    if name.is_empty() {
        return None;
    }
    Some(MemberBlock {
        keyword,
        name: name.to_string(),
        actions_marker: false,
    })
}

/// Blank out string literals and comment bodies so a brace or a `;` inside
/// either does not corrupt the structure scan.
///
/// The result has the same byte length as `line`, so an index into one is an
/// index into the other. AL has block comments and the object-header scan
/// already handled them; without the same handling here, a line such as
/// `/* the old layout used a { here */` pushed a frame that was never popped
/// and every id built after it anchored one level too deep.
pub(super) fn strip_for_structure(line: &str, in_block_comment: &mut bool) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.char_indices().peekable();
    let mut in_single = false;
    let mut in_double = false;
    let mut in_line_comment = false;
    while let Some((_, ch)) = chars.next() {
        let blank = |out: &mut String, ch: char| {
            for _ in 0..ch.len_utf8() {
                out.push(' ');
            }
        };
        if *in_block_comment {
            if ch == '*' && chars.peek().is_some_and(|(_, n)| *n == '/') {
                chars.next();
                *in_block_comment = false;
                out.push_str("  ");
            } else {
                blank(&mut out, ch);
            }
            continue;
        }
        if in_line_comment {
            blank(&mut out, ch);
            continue;
        }
        if !in_single && !in_double && ch == '/' {
            match chars.peek().map(|(_, n)| *n) {
                Some('/') => {
                    chars.next();
                    in_line_comment = true;
                    out.push_str("  ");
                    continue;
                }
                Some('*') => {
                    chars.next();
                    *in_block_comment = true;
                    out.push_str("  ");
                    continue;
                }
                _ => {}
            }
        }
        match ch {
            '\'' if !in_double => {
                in_single = !in_single;
                out.push(ch);
            }
            '"' if !in_single => {
                in_double = !in_double;
                out.push(ch);
            }
            _ if in_single => blank(&mut out, ch),
            _ => out.push(ch),
        }
    }
    out
}

/// Whether a `Caption`/`ToolTip`/`Label` statement carries `Locked = true`.
///
/// Microsoft's AL excludes locked strings from the generated translation file;
/// emitting them made non-translatable text look translatable.
///
/// Takes the statement with every literal blanked, so a `Comment` that happens
/// to use the word ("Shown when the period is locked") does not lock the
/// caption and drop it from the `.g.xlf` for good.
pub(super) fn property_is_locked(stripped_statement: &str) -> bool {
    let lower = stripped_statement.to_lowercase();
    let mut search = 0usize;
    while let Some(offset) = lower[search..].find("locked") {
        let position = search + offset;
        search = position + "locked".len();
        let preceded_by_word = lower[..position]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        if preceded_by_word {
            continue;
        }
        let after = lower[search..].trim_start();
        let locked = match after.strip_prefix('=') {
            // `Locked = true`; the value may be followed by `;` or `,`.
            Some(value) => {
                let word: String = value
                    .trim_start()
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric())
                    .collect();
                word == "true"
            }
            // `Locked` on its own is shorthand for `Locked = true`.
            None => after.is_empty() || after.starts_with(';') || after.starts_with(','),
        };
        if locked {
            return true;
        }
    }
    false
}

pub(super) fn make_translation_unit(
    id: String,
    obj_type: &str,
    obj_id: u32,
    obj_name: &str,
    source: String,
    note: Option<String>,
) -> TranslationUnit {
    TranslationUnit {
        id,
        object_type: obj_type.to_string(),
        object_id: obj_id,
        object_name: obj_name.to_string(),
        source,
        target: None,
        state: TranslationState::New,
        note,
        developer_note: None,
    }
}

/// FNV-1 hash over `s`'s UTF-16LE bytes, biased by `i32::MAX` — Microsoft's
/// `Hash.GetFNVHashCode(string)` as used by `GetLanguageSymbolId`.
///
/// This is the same algorithm `crates/al-emit/src/method_id.rs` implements and
/// verifies against `alc`; it is copied rather than shared so `al-analysis`
/// does not have to depend on the emitter.
pub(super) fn name_hash(s: &str) -> i64 {
    const FNV_OFFSET_BIAS: i32 = -2128831035;
    const FNV_PRIME: i32 = 16777619;
    let mut hash = FNV_OFFSET_BIAS;
    for unit in s.encode_utf16() {
        for byte in unit.to_le_bytes() {
            hash = (hash ^ byte as i32).wrapping_mul(FNV_PRIME);
        }
    }
    hash as i64 + 2_147_483_647
}

/// The first AL object declaration in a file.
#[derive(Debug, Clone)]
pub(super) struct ObjectHeader {
    /// Canonical object-kind spelling used in translation ids (`TableExtension`).
    pub(super) kind_display: String,
    pub(super) id: u32,
    pub(super) name: String,
    is_table_like: bool,
}

/// Parse one already-comment-free line as an object declaration.
///
/// `extract_from_file` calls this at every brace depth of zero, because AL
/// allows several objects in one file and the index supports it. Attributing
/// the whole file to the first declaration gave the second table's captions
/// ids built from the first table's name hash, and two tables sharing a field
/// name ("Document No." is near-universal) produced a byte-identical id, so
/// the second was dropped as a duplicate and its translation vanished.
pub(super) fn parse_object_header_line(line: &str) -> Option<ObjectHeader> {
    let line = line.trim();
    let lower = line.to_lowercase();

    // Sort object type keywords by length descending so longer keywords
    // (extensions) win over their shorter base-type prefixes.
    let mut sorted_types: Vec<&str> = al_syntax::language_data::object_types()
        .iter()
        .map(|ot| ot.keyword.as_str())
        .collect();
    sorted_types.sort_by_key(|k| Reverse(k.len()));

    for ot in &sorted_types {
        let Some(after) = lower.strip_prefix(ot) else {
            continue;
        };
        // Require a word boundary after the keyword.
        if !after.starts_with(|c: char| c.is_ascii_whitespace() || c.is_ascii_digit()) {
            continue;
        }
        let rest = line[ot.len()..].trim();
        let (id_str, rest2) = split_id_and_name(rest);
        let id: u32 = id_str.parse().unwrap_or(0);
        let name = parse_object_name(rest2.trim());
        if name.is_empty() && id == 0 {
            continue;
        }
        let kind = ot.parse::<al_symbols::ObjectKind>().ok();
        let kind_display = kind
            .map(|k| k.to_string())
            .unwrap_or_else(|| capitalize(ot));
        let is_table_like = matches!(
            kind,
            Some(al_symbols::ObjectKind::Table) | Some(al_symbols::ObjectKind::TableExtension)
        );
        return Some(ObjectHeader {
            kind_display,
            id,
            name,
            is_table_like,
        });
    }
    None
}

/// Extract just the object NAME from the header remainder, stopping at the
/// closing quote (quoted identifiers) or the first whitespace (bare
/// identifiers). Extension headers continue with `extends "Base"` —
/// `trim_matches('"')` on the whole remainder swallowed that clause into the
/// name and mangled every xlf unit id for extension objects.
pub(super) fn parse_object_name(rest: &str) -> String {
    let rest = rest.trim();
    for quote in ['"', '\''] {
        if let Some(inner) = rest.strip_prefix(quote) {
            return inner.split(quote).next().unwrap_or_default().to_string();
        }
    }
    rest.split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Split "50100 \"Name\"" into ("50100", "\"Name\"").
pub(super) fn split_id_and_name(s: &str) -> (&str, &str) {
    let s = s.trim();
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    (&s[..end], &s[end..])
}

pub(super) fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

/// Parse a property like `Caption = 'Some text'` or `Caption = 'text', Comment = 'note'`.
///
/// The name and the `=` are compared after trimming, so `Caption='X'` and
/// `Caption  = 'X'` parse the same way alc accepts them. Matching on a literal
/// `"{property} ="` prefix instead left both spellings out of the generated
/// `.g.xlf` with nothing said about it.
pub(super) fn parse_property_value(statement: &str, property: &str) -> Option<String> {
    let (name, value) = statement.split_once('=')?;
    if !name.trim().eq_ignore_ascii_case(property) {
        return None;
    }
    extract_single_quoted(value)
}

/// Parse a `Label` variable declaration like `MyLabel: Label 'Some text';`,
/// returning `(label_name, text)`.
pub(super) fn parse_label_declaration(line: &str) -> Option<(String, String)> {
    // Pattern: <name>: Label '<text>' [, ...];
    let colon = line.find(':')?;
    let name = line[..colon].unquote_identifier();
    if name.is_empty() {
        return None;
    }
    let after_colon = line[colon + 1..].trim();
    let lower = after_colon.to_lowercase();
    if !lower.starts_with("label ") {
        return None;
    }
    let after_label = after_colon[6..].trim();
    let text = extract_single_quoted(after_label)?;
    Some((name.to_string(), text))
}

/// Body of the first single-quoted AL literal in `s`, with `''` unescaped.
///
/// An *empty* literal (`Caption = '';`) is a legal AL construct that suppresses
/// the default caption, and alc emits an empty-source unit for it — returning
/// `None` silently dropped it. `None` now means "no literal here at all".
pub(super) fn extract_single_quoted(s: &str) -> Option<String> {
    let start = s.find('\'')?;
    let inner = &s[start + 1..];
    let mut result = String::new();
    let mut chars = inner.chars().peekable();
    let mut closed = false;
    while let Some(ch) = chars.next() {
        if ch == '\'' {
            if chars.peek() == Some(&'\'') {
                chars.next();
                result.push('\'');
            } else {
                closed = true;
                break;
            }
        } else {
            result.push(ch);
        }
    }
    if closed {
        Some(result)
    } else {
        // Unterminated literal — treat as no value rather than guessing.
        None
    }
}
