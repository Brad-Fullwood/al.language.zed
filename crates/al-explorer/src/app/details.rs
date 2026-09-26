//! Details-pane rendering, lazy member hydration, and "open in editor"
//! for the object browser's selected object.

use al_symbols::source_availability::is_workspace_package;
use std::sync::Arc;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use al_protocol::DaemonClient;

use crate::cli::commands::request_checked;
use crate::types;
use crate::{DetailTarget, display_object_id};

use super::App;

impl App {
    /// the startup symbol dump is slim (no member arrays — a full
    /// dump is ~60 MB JSON). Hydrate the selected object's members from the
    /// daemon on demand, replacing the slim entry in place.
    fn hydrate_selected_object(&mut self) {
        let Some(selected) = self.object_list_state.selected() else {
            return;
        };
        let needs_members = match self.current_objects.get(selected) {
            Some(e) => {
                e.methods.is_empty()
                    && e.fields.is_empty()
                    && e.controls.is_empty()
                    && e.enum_values.is_empty()
                    && e.keys.is_empty()
                    && e.properties.is_empty()
            }
            None => false,
        };
        if !needs_members {
            return;
        }
        let (kind, id, name, package) = match self.current_objects.get(selected) {
            Some(e) => (e.kind, e.id, e.name.clone(), e.package.clone()),
            None => return,
        };
        if self.daemon_client.is_none() {
            match DaemonClient::connect(&self.project_root) {
                Ok(client) => self.daemon_client = Some(client),
                Err(error) => {
                    self.init_status = Some(format!(
                        "Workspace interaction failed: cannot connect to daemon: {error}"
                    ));
                    return;
                }
            }
        }
        let Some(client) = self.daemon_client.as_mut() else {
            return;
        };
        let result = request_checked(
            client,
            "object",
            Some(serde_json::json!({
                "kind": format!("{:?}", kind),
                "name": name,
            })),
        );
        let val = match result {
            Ok(value) => value,
            Err(error) => {
                self.daemon_client = None;
                self.init_status = Some(format!(
                    "Workspace interaction failed while loading {kind:?} {name:?}: {error}"
                ));
                return;
            }
        };
        let full: Vec<types::SymbolEntry> = match serde_json::from_value(val) {
            Ok(v) => v,
            Err(error) => {
                self.init_status = Some(format!(
                    "Workspace interaction failed: invalid object data for {kind:?} \
                     {name:?}: {error}"
                ));
                return;
            }
        };
        // `object` returns every match for (kind, name). Hydrating the first
        // result can attach members from a dependency object to a same-named
        // workspace object, so require the full identity selected in the slim
        // search result. The two workspace package spellings are equivalent.
        let same_package = |candidate: &str| {
            candidate.eq_ignore_ascii_case(&package)
                || is_workspace_package(candidate) && is_workspace_package(&package)
        };
        let Some(hydrated) = full
            .iter()
            .find(|entry| entry.id == id && same_package(&entry.package))
        else {
            self.init_status = Some(format!(
                "Workspace interaction failed: object lookup did not return the selected \
                 {kind:?} {id} {name:?} from package {package:?}"
            ));
            return;
        };
        self.current_objects[selected] = Arc::new(hydrated.clone());
        self.init_status = None;
    }

    pub(crate) fn update_details_items(&mut self) {
        self.hydrate_selected_object();
        self.details_items.clear();

        if let Some(selected) = self.object_list_state.selected()
            && let Some(entry) = self.current_objects.get(selected).cloned()
        {
            self.push_line(Line::from(vec![
                Span::styled(
                    format!("{:?} ", entry.kind),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    display_object_id(&entry)
                        .map(|id| id.to_string())
                        .unwrap_or_default(),
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw(" ".to_string()),
                Span::styled(
                    entry.name.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]));

            if let Some(extends) = &entry.extends {
                self.push_line(label_line("Extends", extends.clone()));
            }
            self.push_line(label_line("Package", entry.package.clone()));
            if let Some(availability) = &entry.source_availability {
                self.push_line(label_line("Source", availability.replace('_', " ")));
            }

            if !entry.properties.is_empty() {
                self.push_section("Properties:".to_string());
                for property in &entry.properties {
                    self.push_line(indented_pair(
                        &property.name,
                        format!(" = {}", property.value),
                        Color::DarkGray,
                        20,
                    ));
                }
            }

            if !entry.keys.is_empty() {
                self.push_section(format!("Keys ({}):", entry.keys.len()));
                for key in &entry.keys {
                    let line = indented_pair(
                        &key.name,
                        format!(" ({})", key.field_names.join(", ")),
                        Color::Cyan,
                        20,
                    );
                    self.push_member(&key.name, line);
                }
            }

            if !entry.fields.is_empty() {
                self.push_section(format!("Fields ({}):", entry.fields.len()));
                for field in &entry.fields {
                    let line = Line::from(vec![
                        Span::styled(
                            format!("    {:<4} ", field.id),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::styled(
                            format!("{:<30}", field.name),
                            Style::default().fg(Color::White),
                        ),
                        Span::styled(
                            format!(" : {}", field.type_name),
                            Style::default().fg(Color::Cyan),
                        ),
                    ]);
                    self.push_member(&field.name, line);
                }
            }

            if !entry.controls.is_empty() {
                self.push_section(format!("Controls/Actions ({}):", entry.controls.len()));
                for control in &entry.controls {
                    let line = indented_pair(
                        &control.kind,
                        format!(" {}", control.name),
                        Color::Magenta,
                        15,
                    );
                    self.push_member(&control.name, line);
                }
            }

            if !entry.enum_values.is_empty() {
                self.push_section(format!("Values ({}):", entry.enum_values.len()));
                for value in &entry.enum_values {
                    let line = Line::from(vec![
                        Span::styled(
                            format!("    {:<4} ", value.ordinal),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::raw(value.name.clone()),
                    ]);
                    self.push_member(&value.name, line);
                }
            }

            if !entry.methods.is_empty() {
                self.push_section(format!("Procedures ({}):", entry.methods.len()));
                for method in &entry.methods {
                    self.push_member(&method.name, procedure_line(method));
                }
            }
        }

        if self.details_items.is_empty() {
            self.push_line(Line::from("No object selected".to_string()));
        }
    }

    /// A details row that is not a member, so pressing enter on it does
    /// nothing.
    fn push_line(&mut self, line: Line<'static>) {
        self.details_items.push((None, line));
    }

    /// A blank separator followed by a bold heading.
    fn push_section(&mut self, heading: String) {
        self.push_line(Line::from(String::new()));
        self.push_line(Line::from(Span::styled(
            heading,
            Style::default().add_modifier(Modifier::BOLD),
        )));
    }

    /// A details row naming a member, so `open_selected_object` can jump to it.
    fn push_member(&mut self, name: &str, line: Line<'static>) {
        self.details_items.push((
            Some(DetailTarget {
                name: name.to_string(),
            }),
            line,
        ));
    }
}

/// `Label: value`, with the label bold.
fn label_line(label: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label}: "),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(value),
    ])
}

/// An indented body row: a coloured name padded to `width`, then `rest`
/// verbatim.
fn indented_pair(name: &str, rest: String, name_color: Color, width: usize) -> Line<'static> {
    Line::from(vec![
        Span::raw("    ".to_string()),
        Span::styled(format!("{name:<width$}"), Style::default().fg(name_color)),
        Span::raw(rest),
    ])
}

/// `local Name(a, b) : Return`, with the modifier column kept even when the
/// procedure is public so the names line up.
fn procedure_line(method: &types::MethodSymbol) -> Line<'static> {
    let mut spans = vec![Span::raw("    ".to_string())];
    if method.is_local {
        spans.push(Span::styled("local ", Style::default().fg(Color::DarkGray)));
    } else {
        spans.push(Span::raw("      ".to_string()));
    }
    spans.push(Span::styled(
        method.name.clone(),
        Style::default().fg(Color::Green),
    ));
    spans.push(Span::raw("(".to_string()));
    spans.push(Span::raw(
        method
            .parameters
            .iter()
            .map(|parameter| parameter.name.to_string())
            .collect::<Vec<_>>()
            .join(", "),
    ));
    spans.push(Span::raw(")".to_string()));
    if let Some(return_type) = &method.return_type {
        spans.push(Span::styled(
            format!(" : {return_type}"),
            Style::default().fg(Color::Cyan),
        ));
    }
    Line::from(spans)
}

impl App {
    pub(crate) fn open_selected_object(&mut self) {
        let target_member: Option<DetailTarget> = self
            .details_list_state
            .selected()
            .and_then(|idx| self.details_items.get(idx))
            .and_then(|(m, _)| m.clone());

        let Some(selected) = self.object_list_state.selected() else {
            self.init_status =
                Some("Workspace interaction failed: no object is selected".to_string());
            return;
        };
        let Some(entry) = self.current_objects.get(selected) else {
            self.init_status =
                Some("Workspace interaction failed: selected object is unavailable".to_string());
            return;
        };
        let entry_name = entry.name.clone();
        let entry_kind = entry.kind;
        let entry_id = entry.id;
        let entry_package = entry.package.clone();

        // Reconnect if the persistent client has been dropped.
        if self.daemon_client.is_none() {
            match DaemonClient::connect(&self.project_root) {
                Ok(client) => self.daemon_client = Some(client),
                Err(error) => {
                    self.init_status = Some(format!(
                        "Workspace interaction failed: cannot connect to daemon for \
                         {}: {error}",
                        self.project_root.display()
                    ));
                    return;
                }
            }
        }
        let Some(client) = self.daemon_client.as_mut() else {
            self.init_status =
                Some("Workspace interaction failed: daemon client is unavailable".to_string());
            return;
        };
        let loc_result = request_checked(
            client,
            "location",
            Some(serde_json::json!({
                "name": entry_name,
                "kind": format!("{:?}", entry_kind),
                "id": entry_id,
                "package": entry_package,
            })),
        );
        let location = match loc_result {
            Ok(value) => value,
            Err(error) => {
                // Connection may have dropped; reset so next call reconnects.
                self.daemon_client = None;
                self.init_status = Some(format!(
                    "Workspace interaction failed while resolving {entry_kind:?} \
                     {entry_id} {entry_name:?}: {error}"
                ));
                return;
            }
        };
        let Some(path_str) = location.get("path").and_then(|value| value.as_str()) else {
            self.init_status = Some(format!(
                "Workspace interaction failed: location response for {entry_name:?} \
                 has no path"
            ));
            return;
        };
        let abs_path = std::path::Path::new(path_str);
        let line = if let Some(member) = &target_member {
            match find_member_line_in_file(abs_path, &member.name) {
                Ok(Some(line)) => line.saturating_add(1),
                Ok(None) => {
                    self.init_status = Some(format!(
                        "Workspace interaction failed: member {:?} was not found in {}",
                        member.name,
                        abs_path.display()
                    ));
                    return;
                }
                Err(error) => {
                    self.init_status = Some(format!(
                        "Workspace interaction failed while locating member {:?}: {error}",
                        member.name
                    ));
                    return;
                }
            }
        } else {
            1
        };
        // use `zed <path>:<line>:<col>` CLI instead of zed:// URL which is
        // unreliable on Linux.
        let file_spec = format!("{}:{}:1", path_str, line);
        match std::process::Command::new("zed").arg(&file_spec).spawn() {
            Ok(_) => self.init_status = None,
            Err(error) => {
                self.init_status = Some(format!(
                    "Workspace interaction failed: could not spawn 'zed {file_spec}': {error}"
                ));
            }
        }
    }
}

/// Scan a text file for the first line containing `member_name` as a whole word.
///
/// Match must be surrounded by non-identifier characters (or start/end of line)
/// so a 1-character field name doesn't accidentally match every line that
/// happens to contain that letter.
fn find_member_line_in_file(
    path: &std::path::Path,
    member_name: &str,
) -> Result<Option<u32>, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let lower = member_name.to_lowercase();
    let is_ident_byte = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    for (i, line) in content.lines().enumerate() {
        let line_lower = line.to_lowercase();
        let bytes = line_lower.as_bytes();
        let mut start = 0;
        while let Some(found) = line_lower[start..].find(&lower) {
            let abs = start + found;
            let before_ok = abs == 0 || !is_ident_byte(bytes[abs - 1]);
            let end = abs + lower.len();
            let after_ok = end == bytes.len() || !is_ident_byte(bytes[end]);
            if before_ok && after_ok {
                // Saturate rather than silently wrap: `i as u32` would truncate
                // modulo 2^32 for a (pathological) >4-billion-line file, handing
                // the editor a bogus line number. Clamp to u32::MAX instead.
                return Ok(Some(u32::try_from(i).unwrap_or(u32::MAX)));
            }
            // Advance by one *character*: `abs + 1` lands inside a multi-byte
            // character when the searched name starts with one, and the next
            // `line_lower[start..]` panics on the non-boundary index. Quoted
            // non-ASCII identifiers are ordinary in Nordic and German AL.
            start = abs + line_lower[abs..].chars().next().map_or(1, char::len_utf8);
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(source: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Table.al");
        std::fs::write(&path, source).unwrap();
        (dir, path)
    }

    #[test]
    fn a_non_ascii_member_name_does_not_panic() {
        // The first hit fails the whole-word check, so the scan continues from
        // just past it. Advancing one *byte* landed inside the leading `Ä` and
        // killed the TUI with "byte index 1 is not a char boundary".
        let (_dir, path) = write("xÄrsredovisning := 1;\nÄrsredovisning := 2;\n");
        assert_eq!(
            find_member_line_in_file(&path, "Ärsredovisning").unwrap(),
            Some(1)
        );
    }

    #[test]
    fn a_name_that_never_matches_as_a_whole_word_is_not_found() {
        let (_dir, path) = write("xÄrsredovisningy := 1;\n");
        assert_eq!(
            find_member_line_in_file(&path, "Ärsredovisning").unwrap(),
            None
        );
    }

    #[test]
    fn an_ascii_member_name_still_matches_on_its_own_line() {
        let (_dir, path) = write("field(1; Amount; Decimal)\n");
        assert_eq!(find_member_line_in_file(&path, "Amount").unwrap(), Some(0));
    }
}

#[cfg(test)]
mod details_items_tests {
    use super::*;
    use crate::types::{
        FieldSymbol, KeySymbol, MethodSymbol, ObjectKind, ParameterSymbol, PropertyValue,
    };

    /// An entry carrying members, so `hydrate_selected_object` returns before
    /// it would reach for a daemon connection.
    fn hydrated_table() -> types::SymbolEntry {
        types::SymbolEntry {
            kind: ObjectKind::Table,
            id: 50100,
            name: "Customer Ext".to_string(),
            extends: Some("Customer".to_string()),
            package: "MyApp".to_string(),
            source_availability: Some("from_source".to_string()),
            methods: vec![MethodSymbol {
                name: "Recalculate".to_string(),
                parameters: vec![ParameterSymbol {
                    name: "Amount".to_string(),
                    type_name: "Decimal".to_string(),
                    is_var: false,
                }],
                return_type: Some("Boolean".to_string()),
                is_local: true,
            }],
            fields: vec![FieldSymbol {
                id: 1,
                name: "No.".to_string(),
                type_name: "Code[20]".to_string(),
            }],
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: vec![KeySymbol {
                name: "PK".to_string(),
                field_names: vec!["No.".to_string()],
            }],
            properties: vec![PropertyValue {
                name: "DataClassification".to_string(),
                value: "CustomerContent".to_string(),
            }],
        }
    }

    fn app_with(entry: types::SymbolEntry) -> App {
        let mut app = App::new();
        app.current_objects = vec![Arc::new(entry)];
        app.object_list_state.select(Some(0));
        app
    }

    fn rendered(app: &App) -> Vec<String> {
        app.details_items
            .iter()
            .map(|(_, line)| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    /// The rows a reader sees, in order, for a hydrated table.
    #[test]
    fn a_hydrated_entry_renders_header_then_one_section_per_member_kind() {
        let mut app = app_with(hydrated_table());
        app.update_details_items();

        let lines = rendered(&app);
        assert_eq!(lines[0], "Table 50100 Customer Ext");
        assert_eq!(lines[1], "Extends: Customer");
        assert_eq!(lines[2], "Package: MyApp");
        assert_eq!(lines[3], "Source: from source");
        assert!(
            lines.iter().any(|l| l == "Properties:"),
            "expected a Properties heading in {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l == "Keys (1):"),
            "expected a Keys heading in {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l == "Fields (1):"),
            "expected a Fields heading in {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l == "Procedures (1):"),
            "expected a Procedures heading in {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("local Recalculate(Amount) : Boolean")),
            "expected the rendered procedure signature in {lines:?}"
        );
    }

    /// Only member rows carry a `DetailTarget`; headings, blanks and the
    /// identity lines carry `None`, which is what stops enter from acting on
    /// them.
    #[test]
    fn only_member_rows_are_jump_targets() {
        let mut app = app_with(hydrated_table());
        app.update_details_items();

        let targets: Vec<String> = app
            .details_items
            .iter()
            .filter_map(|(target, _)| target.as_ref().map(|t| t.name.clone()))
            .collect();
        assert_eq!(
            targets,
            vec![
                "PK".to_string(),
                "No.".to_string(),
                "Recalculate".to_string()
            ],
            "keys, fields and procedures are the jumpable rows"
        );
    }

    /// An entry with no members at all would send `hydrate_selected_object`
    /// looking for a daemon, so an empty section must not be rendered either.
    #[test]
    fn sections_are_omitted_when_the_member_list_is_empty() {
        let mut entry = hydrated_table();
        entry.keys.clear();
        entry.fields.clear();
        entry.extends = None;
        entry.source_availability = None;
        let mut app = app_with(entry);
        app.update_details_items();

        let lines = rendered(&app);
        assert!(
            !lines.iter().any(|l| l.starts_with("Keys (")),
            "no keys means no Keys heading: {lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l.starts_with("Fields (")),
            "no fields means no Fields heading: {lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l.starts_with("Extends: ")),
            "no base object means no Extends row: {lines:?}"
        );
    }

    #[test]
    fn no_selection_renders_the_placeholder() {
        let mut app = App::new();
        app.update_details_items();
        assert_eq!(rendered(&app), vec!["No object selected".to_string()]);
    }
}
