//! Translatable text from AL source: captions, tooltips, labels, with
//! `alc`'s translation-unit IDs.
//!
//! The rules come from alc 18.1 (`Microsoft.Dynamics.Nav.CodeAnalysis.dll`
//! 18.1.2842859, decompiled with ilspycmd, not from observation of its output):
//!
//! - `LabelWriterVisitor.VisitSymbolWithProperties` decides which units exist.
//! - `LanguageFileUtilities.GetLanguageSymbolId` builds the id.
//! - `ExtensionObjectFoldingUtilities.GetFoldingExtensionObject` picks the
//!   object whose name hash opens an extension object's ids.
//! - `LanguageFileUtilities.ObjectTargetSymbolId` writes `al-object-target`.

use super::*;
use std::collections::HashSet;

/// The app.json `features` that change which units alc puts in the `.g.xlf`.
///
/// `TranslationFile` is not here. It makes alc write the file at all, and
/// `xlf generate` is an explicit request for the file. `AllTranslationItems`,
/// `GenerateLockedTranslations`, `LcgTranslationFile` and
/// `TranslationsWithNamespaces` are not honoured, see
/// `Docs/features/xliff-translation.md`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct XliffOptions {
    /// `GenerateCaptions`: units for objects, actions and option fields that
    /// declare no `Caption` or `OptionCaption` (alc `GenerateTranslationsMode.Captions`).
    pub generate_captions: bool,
}

impl XliffOptions {
    /// Read the options from the app.json `features` array. Feature names are
    /// case-insensitive, as in alc (`CompilerFeaturesExtensions` upper-cases them).
    pub fn from_features<S: AsRef<str>>(features: &[S]) -> Self {
        Self {
            generate_captions: features
                .iter()
                .any(|feature| feature.as_ref().eq_ignore_ascii_case("GenerateCaptions")),
        }
    }
}

/// The properties alc writes a unit for when they hold a literal
/// (`PropertyKindExtensions.IsTranslatableProperty`), spelled as alc names them.
const TRANSLATABLE_PROPERTIES: &[&str] = &[
    "Caption",
    "OptionCaption",
    "InstructionalText",
    "PromotedActionCategories",
    "AdditionalSearchTerms",
    "EntityCaption",
    "EntitySetCaption",
    "ToolTip",
    "AboutText",
    "AboutTitle",
    "RequestFilterHeading",
    "Summary",
    "ProfileDescription",
];

/// Extract all translatable text units from the workspace AL files, for a
/// project with only `TranslationFile` set.
pub fn extract_translation_units(workspace: &Workspace) -> Vec<TranslationUnit> {
    extract_translation_units_with(workspace, XliffOptions::default())
}

/// Extract all translatable text units from the workspace AL files.
///
/// Two passes: the first reads every object header, because the id root of an
/// extension depends on the other extensions of the same target. The second
/// scans each file for the units. Returns units in a deterministic order.
pub fn extract_translation_units_with(
    workspace: &Workspace,
    options: XliffOptions,
) -> Vec<TranslationUnit> {
    let mut paths: Vec<_> = workspace
        .file_index
        .files
        .iter()
        .map(|e| e.key().clone())
        .collect();
    paths.sort();

    let texts: Vec<String> = paths
        .iter()
        .filter_map(|path| workspace.file_index.files.get(path))
        .map(|text| text.value().to_string())
        .collect();
    let index = ProjectIndex::build(texts.iter().map(String::as_str));

    let mut units = Vec::new();
    for text in &texts {
        scan_file(text, &index, options, &mut units);
    }

    // A `.g.xlf` with duplicate `trans-unit id=` values silently collapses to
    // one entry in `parse_xliff`'s map, so refresh/untranslated/suggest would
    // operate on corrupted data. Keep the first occurrence and warn.
    let mut seen: HashSet<String> = HashSet::new();
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

/// Extract translation units from a single AL file, as a project of one file.
#[cfg(test)]
pub(super) fn extract_from_file(path: &Path, text: &str, units: &mut Vec<TranslationUnit>) {
    let _ = path;
    let index = ProjectIndex::build(std::iter::once(text));
    scan_file(text, &index, XliffOptions::default(), units);
}

/// What the headers of the whole project say about extension objects.
#[derive(Debug, Default)]
pub(super) struct ProjectIndex {
    /// Objects the project declares, keyed by (kind, lower-cased name), to the
    /// name as declared.
    declared: HashMap<(String, String), String>,
    /// Extensions keyed by (extension kind, lower-cased target name) to
    /// (object id, name).
    extensions: HashMap<(String, String), Vec<(u32, String)>>,
    /// The target a header names, keyed by (kind, object id). The `extends`
    /// clause may sit on a line after the object keyword.
    targets: HashMap<(String, u32), String>,
}

impl ProjectIndex {
    pub(super) fn build<'a>(texts: impl Iterator<Item = &'a str>) -> Self {
        let mut index = Self::default();
        for text in texts {
            for header in collect_object_headers(text) {
                index.add(header);
            }
        }
        index
    }

    fn add(&mut self, header: ObjectHeader) {
        let kind = header.kind_display.clone();
        if let Some(target) = &header.extends {
            self.targets
                .insert((kind.clone(), header.id), target.clone());
        }
        if is_folding_extension(&kind) {
            if let Some(target) = &header.extends {
                self.extensions
                    .entry((kind, target.to_lowercase()))
                    .or_default()
                    .push((header.id, header.name));
            }
        } else {
            self.declared
                .insert((kind, header.name.to_lowercase()), header.name);
        }
    }

    /// The object context for a header found while scanning.
    ///
    /// `GetFoldingExtensionObject` in alc, for an extension object:
    /// 1. a customization object (`PageCustomization`) is its own root,
    /// 2. when the target is declared in the same app, the target is the root,
    /// 3. otherwise the extension with the lowest object id among all the app's
    ///    extensions of that target is the root.
    fn object_context(&self, header: ObjectHeader) -> ObjectContext {
        let kind = header.kind_display.clone();
        let own_hash = name_hash(&header.name);
        let own_root = (kind.clone(), own_hash);
        if !is_extension_or_customization(&kind) {
            return ObjectContext {
                header,
                root_kind: own_root.0,
                root_hash: own_root.1,
                target: None,
                own_target: None,
            };
        }
        let base = base_kind(&kind);
        let target_text = header
            .extends
            .clone()
            .or_else(|| self.targets.get(&(kind.clone(), header.id)).cloned());
        let declared_target = target_text
            .as_ref()
            .and_then(|text| self.declared.get(&(base.to_string(), text.to_lowercase())));
        // alc hashes the target symbol's name, which is the declared spelling
        // when the project holds the target. For a dependency's object only the
        // spelling in the `extends` clause is known.
        let target_name = declared_target.or(target_text.as_ref());
        let target = match target_name {
            Some(name) => format!("{base} {}", name_hash(name)),
            None => "Unknown Target".to_string(),
        };
        let root = if !is_folding_extension(&kind) {
            own_root
        } else if let Some(declared) = declared_target {
            (base.to_string(), name_hash(declared))
        } else {
            let lowest = target_text.as_ref().and_then(|text| {
                self.extensions
                    .get(&(kind.clone(), text.to_lowercase()))?
                    .iter()
                    .min_by_key(|(id, _)| *id)
            });
            match lowest {
                Some((_, name)) => (kind.clone(), name_hash(name)),
                None => own_root,
            }
        };
        ObjectContext {
            own_target: Some(format!("{kind} {own_hash}")),
            header,
            root_kind: root.0,
            root_hash: root.1,
            target: Some(target),
        }
    }
}

/// `TableExtension`, `PageExtension` and the like.
fn is_folding_extension(kind: &str) -> bool {
    kind.ends_with("Extension")
}

/// `IsExtensionOrCustomizationObject`: extensions and `PageCustomization`.
fn is_extension_or_customization(kind: &str) -> bool {
    is_folding_extension(kind) || kind == "PageCustomization"
}

/// The kind an extension or customization object extends.
fn base_kind(kind: &str) -> &str {
    kind.strip_suffix("Extension")
        .or_else(|| kind.strip_suffix("Customization"))
        .unwrap_or(kind)
}

/// Every object header in a file, with the target of an extension. Works on
/// the brace depth, so a header whose `extends` clause is on the next line is
/// read as a whole.
fn collect_object_headers(text: &str) -> Vec<ObjectHeader> {
    let mut headers = Vec::new();
    let mut depth = 0usize;
    let mut in_block_comment = false;
    let mut pending: Option<(ObjectHeader, String)> = None;
    for line in text.lines() {
        let code = strip_for_structure(line, &mut in_block_comment);
        if depth == 0 {
            let head = code.split('{').next().unwrap_or("").trim().to_string();
            match pending.as_mut() {
                Some((_, collected)) => {
                    collected.push(' ');
                    collected.push_str(&head);
                }
                None => {
                    if let Some(header) = parse_object_header_line(&head) {
                        pending = Some((header, head));
                    }
                }
            }
            if code.contains('{') {
                if let Some((mut header, collected)) = pending.take() {
                    header.extends = parse_target_clause(&collected);
                    headers.push(header);
                }
            }
        }
        for ch in code.chars() {
            match ch {
                '{' => depth += 1,
                '}' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
    }
    headers
}

/// The object named after `extends` or `customizes` in a header.
fn parse_target_clause(header: &str) -> Option<String> {
    let lower = header.to_ascii_lowercase();
    for word in ["extends", "customizes"] {
        let mut from = 0usize;
        while let Some(found) = lower[from..].find(word) {
            let at = from + found;
            let after = &header[at + word.len()..];
            let before_is_word = lower[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '"');
            if !before_is_word && after.starts_with(char::is_whitespace) {
                let name = parse_object_name(after);
                if !name.is_empty() {
                    return Some(name);
                }
            }
            from = at + word.len();
        }
    }
    None
}

/// Scan one file for translation units.
///
/// The scan is structural: brace nesting decides which member (table field,
/// page control, page action) a `Caption`/`ToolTip` belongs to. The previous
/// line-oriented scan tracked only a numeric `field_id` that page-layout
/// members never set and that was never reset when a field block ended, so
/// every page control produced the *same* trans-unit id and object-level
/// properties were attributed to the last field seen.
fn scan_file(
    text: &str,
    index: &ProjectIndex,
    options: XliffOptions,
    units: &mut Vec<TranslationUnit>,
) {
    let mut scanner = Scanner {
        index,
        options,
        object: None,
        stack: Vec::new(),
        pending: None,
        method: None,
        units,
    };
    let mut in_block_comment = false;

    for line in text.lines() {
        let code = strip_for_structure(line, &mut in_block_comment);
        let mut segment_start = 0usize;
        for (position, ch) in code.char_indices() {
            if ch != '{' && ch != '}' {
                continue;
            }
            scanner.segment(
                &line[segment_start..position],
                &code[segment_start..position],
            );
            if ch == '{' {
                scanner.open_block();
            } else {
                scanner.close_block();
            }
            segment_start = position + ch.len_utf8();
        }
        scanner.segment(&line[segment_start..], &code[segment_start..]);
    }
}

/// The object a translation unit belongs to.
pub(super) struct ObjectContext {
    header: ObjectHeader,
    /// The kind opening every id of this object. For an extension folded onto
    /// another object this is that object's kind.
    root_kind: String,
    root_hash: i64,
    /// `al-object-target` for property units: `<TargetKind> <hash>` of the
    /// object an extension extends. `None` outside extension objects.
    target: Option<String>,
    /// `al-object-target` for Label units: the extension object itself.
    own_target: Option<String>,
}

/// One open brace: the member it declares, if any, and where it sits.
pub(super) struct Frame {
    member: Option<MemberBlock>,
    /// Under an `actions` section.
    in_actions: bool,
    /// A report `labels` section, whose entries are `Name = 'text';`.
    in_labels: bool,
    /// A `Caption` or `CaptionML` property is declared directly in this block.
    caption_seen: bool,
    /// An `OptionCaption` or `OptionCaptionML` property is declared here.
    option_caption_seen: bool,
    /// The members of an `OptionMembers` property, joined by commas.
    option_members: Option<String>,
}

struct Scanner<'a> {
    index: &'a ProjectIndex,
    options: XliffOptions,
    object: Option<ObjectContext>,
    /// One frame per open brace.
    stack: Vec<Frame>,
    pending: Option<MemberBlock>,
    /// The procedure or trigger whose `var` section is being read.
    method: Option<String>,
    units: &'a mut Vec<TranslationUnit>,
}

impl Scanner<'_> {
    fn open_block(&mut self) {
        // `Action` is what alc emits for a group inside `actions`,
        // so the flag has to reach every descendant, not just the
        // `actions` marker block itself.
        let in_actions = self.stack.last().is_some_and(|frame| frame.in_actions)
            || self.pending.as_ref().is_some_and(|m| m.actions_marker);
        let in_labels = self.pending.as_ref().is_some_and(|m| m.labels_marker);
        let member = self
            .pending
            .take()
            .filter(|member| !member.actions_marker && !member.labels_marker);
        self.stack.push(Frame {
            member,
            in_actions,
            in_labels,
            caption_seen: false,
            option_caption_seen: false,
            option_members: None,
        });
    }

    fn close_block(&mut self) {
        let Some(frame) = self.stack.pop() else {
            return;
        };
        let is_object = self.stack.is_empty();
        self.finish_frame(&frame, is_object);
        if is_object {
            // The object closed: a file may hold several.
            self.object = None;
            self.method = None;
        }
    }

    /// Units for what a block did not declare. alc writes them from
    /// `LabelWriterVisitor.VisitSymbolWithProperties`:
    /// - a table or table extension field with no `Caption` gets its name as
    ///   the caption, in every mode,
    /// - with `GenerateCaptions` a table, page, report, query or xmlport object,
    ///   an action, and an option field's `OptionCaption` get one as well.
    fn finish_frame(&mut self, frame: &Frame, is_object: bool) {
        let Some(object) = self.object.as_ref() else {
            return;
        };
        let generate = self.options.generate_captions;
        let mut found = Vec::new();
        if is_object {
            if generate
                && !frame.caption_seen
                && matches!(
                    object.header.kind_display.as_str(),
                    "Table" | "Page" | "Report" | "Query" | "XmlPort"
                )
            {
                found.push(build_unit(
                    object,
                    &[("Property", "Caption")],
                    object.header.name.clone(),
                    false,
                ));
            }
        } else if let Some(member) = &frame.member {
            let kind = member.id_kind(&object.header, frame.in_actions);
            let member_path = [(kind, member.name.as_str())];
            if kind == "Field" {
                if !frame.caption_seen {
                    found.push(build_unit(
                        object,
                        &[member_path[0], ("Property", "Caption")],
                        member.name.clone(),
                        false,
                    ));
                }
                if generate && member.is_option_type() && !frame.option_caption_seen {
                    found.push(build_unit(
                        object,
                        &[member_path[0], ("Property", "OptionCaption")],
                        frame.option_members.clone().unwrap_or_default(),
                        false,
                    ));
                }
            } else if kind == "Action"
                && generate
                && !frame.caption_seen
                && matches!(member.keyword.as_str(), "action" | "group")
            {
                found.push(build_unit(
                    object,
                    &[member_path[0], ("Property", "Caption")],
                    member.name.clone(),
                    false,
                ));
            }
        }
        self.units.extend(found);
    }

    /// Process one brace-free run of a line: an object header, a member header,
    /// and every `;`-terminated statement in it.
    ///
    /// Working in segments rather than whole lines is what makes a one-line member
    /// (`field(1; "No."; Code[20]) { Caption = 'No.'; }`) resolve against the field
    /// it declares: the brace between the two has already been applied to the stack
    /// by the time the caption is read.
    fn segment(&mut self, raw: &str, code: &str) {
        if self.stack.is_empty() {
            if let Some(header) = parse_object_header_line(code.trim()) {
                self.object = Some(self.index.object_context(header));
            }
            return;
        }
        if self.object.is_none() {
            return;
        }

        if let Some(member) = parse_member_block(code.trim()) {
            self.pending = Some(member);
        }
        if let Some(name) = parse_method_header(code.trim()) {
            self.method = Some(name);
        }

        for (statement, stripped) in statements(raw, code) {
            self.note_declared_properties(statement);
            if property_is_locked(stripped) {
                continue;
            }
            self.emit_statement(statement);
        }

        // Labels sit in `var` sections, which end where the body begins.
        if contains_word(code, "begin") {
            self.method = None;
        }
    }

    /// Record which captions a block declares, locked or not: a locked caption
    /// still means the field has one and needs no implicit caption.
    fn note_declared_properties(&mut self, statement: &str) {
        let Some((name, value)) = statement.split_once('=') else {
            return;
        };
        let name = name.trim();
        let anchor = self
            .stack
            .iter()
            .rposition(|frame| frame.member.is_some())
            .unwrap_or(0);
        let Some(frame) = self.stack.get_mut(anchor) else {
            return;
        };
        if name.eq_ignore_ascii_case("Caption") || name.eq_ignore_ascii_case("CaptionML") {
            frame.caption_seen = true;
        } else if name.eq_ignore_ascii_case("OptionCaption")
            || name.eq_ignore_ascii_case("OptionCaptionML")
        {
            frame.option_caption_seen = true;
        } else if name.eq_ignore_ascii_case("OptionMembers") {
            frame.option_members = Some(join_option_members(value));
        }
    }

    fn emit_statement(&mut self, statement: &str) {
        let Some(object) = self.object.as_ref() else {
            return;
        };
        let anchor = self.stack.iter().rev().find_map(|frame| {
            frame
                .member
                .as_ref()
                .map(|member| (member, frame.in_actions))
        });
        let mut found = Vec::new();

        // `labels { Name = 'text'; }` in a report: alc's `ReportLabel` symbols.
        if self.stack.last().is_some_and(|frame| frame.in_labels) {
            if let Some((name, value)) = statement.split_once('=') {
                let name = name.trim().unquote_identifier().to_string();
                if !name.is_empty() && value.trim_start().starts_with('\'') {
                    if let Some(text) = extract_single_quoted(value) {
                        let mut unit = build_unit(object, &[("ReportLabel", &name)], text, false);
                        unit.developer_note = statement_comment(statement);
                        found.push(unit);
                    }
                }
            }
            self.units.extend(found);
            return;
        }

        for property in TRANSLATABLE_PROPERTIES {
            let Some(value) = parse_property_value(statement, property) else {
                continue;
            };
            let mut path: Vec<(&str, &str)> = Vec::new();
            if let Some((member, in_actions)) = anchor {
                path.push((member.id_kind(&object.header, in_actions), &member.name));
            }
            path.push(("Property", property));
            let mut unit = build_unit(object, &path, value, false);
            unit.developer_note = statement_comment(statement);
            found.push(unit);
        }

        // `MyLabel: Label 'text';` — alc keys labels by the NamedType name. A
        // label in a procedure's `var` section also carries the method, and a
        // trigger's carries the member it sits in.
        if let Some((label_name, label_text)) = parse_label_declaration(statement) {
            let mut path: Vec<(&str, &str)> = Vec::new();
            if let Some(method) = self.method.as_deref() {
                if let Some((member, in_actions)) = anchor {
                    path.push((member.id_kind(&object.header, in_actions), &member.name));
                }
                path.push(("Method", method));
            }
            path.push(("NamedType", &label_name));
            let mut unit = build_unit(object, &path, label_text, true);
            unit.developer_note = statement_comment(statement);
            found.push(unit);
        }
        self.units.extend(found);
    }
}

/// One unit. `path` is the chain after the object: an optional member, then
/// `Property <name>`, `NamedType <label>` or `ReportLabel <name>`.
///
/// The id is the hashed form built on the root object, the note the readable
/// form built on the declaring object, as in alc's
/// `GetTranslationFileId` and `GetTranslationFileNamedId`.
fn build_unit(
    object: &ObjectContext,
    path: &[(&str, &str)],
    source: String,
    is_label: bool,
) -> TranslationUnit {
    let obj_type = &object.header.kind_display;
    let obj_name = &object.header.name;
    let mut id = format!("{} {}", object.root_kind, object.root_hash);
    let mut note = format!("{obj_type} {obj_name}");
    for (kind, name) in path {
        // The property component is hashed from its property name, the
        // others from the symbol name.
        id.push_str(&format!(" - {kind} {}", name_hash(name)));
        note.push_str(&format!(" - {kind} {name}"));
    }
    let mut unit =
        make_translation_unit(id, obj_type, object.header.id, obj_name, source, Some(note));
    // ObjectTargetSymbolId: a Label names the extension itself, everything
    // else the object it extends.
    unit.object_target = if is_label {
        object.own_target.clone()
    } else {
        object.target.clone()
    };
    unit
}

/// `procedure Name(...)` or `trigger Name()`, with the optional modifiers.
fn parse_method_header(trimmed: &str) -> Option<String> {
    let lower = trimmed.to_ascii_lowercase();
    let mut rest = lower.as_str();
    let mut offset = 0usize;
    for modifier in ["local ", "internal ", "protected "] {
        if let Some(after) = rest.strip_prefix(modifier) {
            offset += rest.len() - after.len();
            rest = after.trim_start();
            offset += after.len() - rest.len();
            break;
        }
    }
    let keyword_len = if rest.starts_with("procedure ") {
        "procedure ".len()
    } else if rest.starts_with("trigger ") {
        "trigger ".len()
    } else {
        return None;
    };
    let after = &trimmed[offset + keyword_len..];
    let name = after.split('(').next()?.trim().unquote_identifier();
    (!name.is_empty()).then(|| name.to_string())
}

/// Whether `word` appears in `code` as a whole word, ignoring case.
fn contains_word(code: &str, word: &str) -> bool {
    let lower = code.to_ascii_lowercase();
    let mut from = 0usize;
    while let Some(found) = lower[from..].find(word) {
        let at = from + found;
        let end = at + word.len();
        let before = lower[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        let after = lower[end..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        if !before && !after {
            return true;
        }
        from = end;
    }
    false
}

/// The text alc uses as the source of a synthesized `OptionCaption`
/// (`OptionTypeSymbol.OptionMembers`): the members joined by commas, quotes
/// removed, an empty first member kept.
fn join_option_members(value: &str) -> String {
    let mut members: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut quoted_text: Option<String> = None;
    let mut in_quotes = false;
    for ch in value.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                quoted_text.get_or_insert_with(String::new);
            }
            ',' if !in_quotes => {
                members.push(
                    quoted_text
                        .take()
                        .unwrap_or_else(|| current.trim().to_string()),
                );
                current.clear();
            }
            _ if in_quotes => quoted_text.get_or_insert_with(String::new).push(ch),
            _ => current.push(ch),
        }
    }
    members.push(
        quoted_text
            .take()
            .unwrap_or_else(|| current.trim().to_string()),
    );
    members.join(",")
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
    /// The data type of a table field, as written.
    data_type: String,
    /// The unnamed `actions` section marker, which anchors nothing itself but
    /// makes every block inside it an action.
    actions_marker: bool,
    /// The unnamed report `labels` section marker.
    labels_marker: bool,
}

impl MemberBlock {
    /// alc's id component for this member, the symbol kind of
    /// `LanguageFileUtilities.GetLanguageSymbolId`.
    fn id_kind(&self, header: &ObjectHeader, in_actions: bool) -> &'static str {
        match self.keyword.as_str() {
            "dataitem" => "ReportDataItem",
            "column" if header.kind_display.starts_with("Query") => "QueryColumn",
            "column" => "ReportColumn",
            "filter" => "QueryFilter",
            "value" => "EnumValue",
            "modify" => "Change",
            _ if in_actions || self.keyword == "action" || self.keyword == "actionref" => "Action",
            "field" if header.is_table_like => "Field",
            _ => "Control",
        }
    }

    fn is_option_type(&self) -> bool {
        self.data_type.to_ascii_lowercase().starts_with("option")
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
    "filter",
    "value",
    "modify",
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
            data_type: String::new(),
            actions_marker: true,
            labels_marker: false,
        });
    }
    if lower_head == "labels" {
        return Some(MemberBlock {
            keyword: "labels".to_string(),
            name: String::new(),
            data_type: String::new(),
            actions_marker: false,
            labels_marker: true,
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
    let data_type = if first.parse::<u32>().is_ok() {
        args.get(2)
            .map(|a| a.trim().to_string())
            .unwrap_or_default()
    } else {
        String::new()
    };
    Some(MemberBlock {
        keyword,
        name: name.to_string(),
        data_type,
        actions_marker: false,
        labels_marker: false,
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
        object_target: None,
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
    /// The object an extension extends or a customization customizes, when the
    /// header line names it. A header split over lines is read by
    /// `collect_object_headers`.
    pub(super) extends: Option<String>,
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
            extends: parse_target_clause(&rest2[name_end_offset(rest2)..]),
            name,
            is_table_like,
        });
    }
    None
}

/// Byte offset just after the object name in `rest` (`"Name" extends ...`).
fn name_end_offset(rest: &str) -> usize {
    let trimmed = rest.trim_start();
    let lead = rest.len() - trimmed.len();
    for quote in ['"', '\''] {
        if let Some(inner) = trimmed.strip_prefix(quote) {
            return lead + 1 + inner.find(quote).map_or(inner.len(), |i| i + 1);
        }
    }
    lead + trimmed.find(char::is_whitespace).unwrap_or(trimmed.len())
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
