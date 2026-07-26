//! Details-pane rendering, lazy member hydration, and "open in editor"
//! for the object browser's selected object.

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
            && let Some(entry) = self.current_objects.get(selected)
        {
            self.details_items.push((
                None,
                Line::from(vec![
                    Span::styled(
                        format!("{:?} ", entry.kind),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        display_object_id(entry)
                            .map(|id| id.to_string())
                            .unwrap_or_default(),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::raw(" ".to_string()),
                    Span::styled(
                        entry.name.clone(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                ]),
            ));

            if let Some(extends) = &entry.extends {
                self.details_items.push((
                    None,
                    Line::from(vec![
                        Span::styled(
                            "Extends: ".to_string(),
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(extends.clone()),
                    ]),
                ));
            }

            self.details_items.push((
                None,
                Line::from(vec![
                    Span::styled(
                        "Package: ".to_string(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(entry.package.clone()),
                ]),
            ));

            if let Some(availability) = &entry.source_availability {
                self.details_items.push((
                    None,
                    Line::from(vec![
                        Span::styled(
                            "Source: ".to_string(),
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(availability.replace('_', " ")),
                    ]),
                ));
            }

            if !entry.properties.is_empty() {
                self.details_items.push((None, Line::from("".to_string())));
                self.details_items.push((
                    None,
                    Line::from(Span::styled(
                        "Properties:".to_string(),
                        Style::default().add_modifier(Modifier::BOLD),
                    )),
                ));
                for p in entry.properties.iter() {
                    self.details_items.push((
                        None,
                        Line::from(vec![
                            Span::raw("    ".to_string()),
                            Span::styled(
                                format!("{:<20}", p.name),
                                Style::default().fg(Color::DarkGray),
                            ),
                            Span::raw(" = ".to_string()),
                            Span::raw(p.value.clone()),
                        ]),
                    ));
                }
            }

            if !entry.keys.is_empty() {
                self.details_items.push((None, Line::from("".to_string())));
                self.details_items.push((
                    None,
                    Line::from(Span::styled(
                        format!("Keys ({}):", entry.keys.len()),
                        Style::default().add_modifier(Modifier::BOLD),
                    )),
                ));
                for k in entry.keys.iter() {
                    let fields = k.field_names.join(", ");
                    self.details_items.push((
                        Some(DetailTarget {
                            name: k.name.clone(),
                        }),
                        Line::from(vec![
                            Span::raw("    ".to_string()),
                            Span::styled(
                                format!("{:<20}", k.name),
                                Style::default().fg(Color::Cyan),
                            ),
                            Span::raw(format!(" ({})", fields)),
                        ]),
                    ));
                }
            }

            if !entry.fields.is_empty() {
                self.details_items.push((None, Line::from("".to_string())));
                self.details_items.push((
                    None,
                    Line::from(Span::styled(
                        format!("Fields ({}):", entry.fields.len()),
                        Style::default().add_modifier(Modifier::BOLD),
                    )),
                ));
                for f in entry.fields.iter() {
                    self.details_items.push((
                        Some(DetailTarget {
                            name: f.name.clone(),
                        }),
                        Line::from(vec![
                            Span::styled(
                                format!("    {:<4} ", f.id),
                                Style::default().fg(Color::DarkGray),
                            ),
                            Span::styled(
                                format!("{:<30}", f.name),
                                Style::default().fg(Color::White),
                            ),
                            Span::styled(
                                format!(" : {}", f.type_name),
                                Style::default().fg(Color::Cyan),
                            ),
                        ]),
                    ));
                }
            }

            if !entry.controls.is_empty() {
                self.details_items.push((None, Line::from("".to_string())));
                self.details_items.push((
                    None,
                    Line::from(Span::styled(
                        format!("Controls/Actions ({}):", entry.controls.len()),
                        Style::default().add_modifier(Modifier::BOLD),
                    )),
                ));
                for c in entry.controls.iter() {
                    self.details_items.push((
                        Some(DetailTarget {
                            name: c.name.clone(),
                        }),
                        Line::from(vec![
                            Span::raw("    ".to_string()),
                            Span::styled(
                                format!("{:<15}", c.kind),
                                Style::default().fg(Color::Magenta),
                            ),
                            Span::raw(format!(" {}", c.name)),
                        ]),
                    ));
                }
            }

            if !entry.enum_values.is_empty() {
                self.details_items.push((None, Line::from("".to_string())));
                self.details_items.push((
                    None,
                    Line::from(Span::styled(
                        format!("Values ({}):", entry.enum_values.len()),
                        Style::default().add_modifier(Modifier::BOLD),
                    )),
                ));
                for v in entry.enum_values.iter() {
                    self.details_items.push((
                        Some(DetailTarget {
                            name: v.name.clone(),
                        }),
                        Line::from(vec![
                            Span::styled(
                                format!("    {:<4} ", v.ordinal),
                                Style::default().fg(Color::DarkGray),
                            ),
                            Span::raw(v.name.clone()),
                        ]),
                    ));
                }
            }

            if !entry.methods.is_empty() {
                self.details_items.push((None, Line::from("".to_string())));
                self.details_items.push((
                    None,
                    Line::from(Span::styled(
                        format!("Procedures ({}):", entry.methods.len()),
                        Style::default().add_modifier(Modifier::BOLD),
                    )),
                ));
                for m in entry.methods.iter() {
                    let mut spans = vec![Span::raw("    ".to_string())];
                    if m.is_local {
                        spans.push(Span::styled("local ", Style::default().fg(Color::DarkGray)));
                    } else {
                        spans.push(Span::raw("      ".to_string()));
                    }
                    spans.push(Span::styled(
                        m.name.clone(),
                        Style::default().fg(Color::Green),
                    ));
                    spans.push(Span::raw("(".to_string()));

                    let params = m
                        .parameters
                        .iter()
                        .map(|p| p.name.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    spans.push(Span::raw(params));

                    spans.push(Span::raw(")".to_string()));

                    if let Some(ret) = &m.return_type {
                        spans.push(Span::styled(
                            format!(" : {}", ret),
                            Style::default().fg(Color::Cyan),
                        ));
                    }

                    self.details_items.push((
                        Some(DetailTarget {
                            name: m.name.clone(),
                        }),
                        Line::from(spans),
                    ));
                }
            }
        }

        if self.details_items.is_empty() {
            self.details_items
                .push((None, Line::from("No object selected".to_string())));
        }
    }

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

fn is_workspace_package(package: &str) -> bool {
    package.eq_ignore_ascii_case("workspace") || package.eq_ignore_ascii_case("(workspace)")
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
            start = abs + 1;
        }
    }
    Ok(None)
}
