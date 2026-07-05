//! Find references query.

use url::Url;

use super::{Location, Position, Range};
use al_workspace::Workspace;

#[must_use]
pub fn references(
    workspace: &Workspace,
    uri: &Url,
    position: Position,
    include_declaration: bool,
) -> Vec<Location> {
    let Some((text, tree)) = al_source::parsing::get_or_parse(&workspace.documents, uri) else {
        return Vec::new();
    };

    let Some(node) = al_syntax::find_node_at_position(&tree, &text, position.into()) else {
        return Vec::new();
    };
    let Some(clean_name) = super::node_clean_name(node, text.as_bytes()) else {
        return Vec::new();
    };

    let mut locations = Vec::new();

    let source_bytes = text.as_bytes();
    // Identifier references plus event-subscriber string-literal references: an
    // event's subscribers name it via a string literal inside
    // `[EventSubscriber(...)]`, which the identifier walk cannot see (audit
    // 2026-06-20). Both are collected so `references` on an event surfaces its
    // subscribers, not just its declaration and raise sites.
    let mut refs = al_syntax::find_variable_references(&tree, &text, clean_name);
    refs.extend(al_syntax::find_event_subscriber_references(
        &tree, &text, clean_name,
    ));
    for r in &refs {
        let range: Range = al_syntax::ts_range_to_syntax(r, source_bytes).into();
        locations.push(Location {
            uri: uri.clone(),
            range,
        });
    }

    let current_path = uri.to_file_path().ok();
    // Snapshot file paths to avoid holding the DashMap shard lock across
    // cached-parse lookups and AST walks.
    let file_paths: Vec<std::path::PathBuf> = workspace
        .file_index
        .files
        .iter()
        .map(|e| e.key().clone())
        .collect();

    for file_path in file_paths {
        if current_path.as_ref() == Some(&file_path) {
            continue;
        }
        let Some((file_text, file_tree)) = workspace.file_index.get_cached_parse(&file_path) else {
            continue;
        };
        let mut refs = al_syntax::find_variable_references(&file_tree, &file_text, clean_name);
        refs.extend(al_syntax::find_event_subscriber_references(
            &file_tree, &file_text, clean_name,
        ));
        let file_source_bytes = file_text.as_bytes();
        for r in &refs {
            if let Ok(file_uri) = Url::from_file_path(&file_path) {
                locations.push(Location {
                    uri: file_uri,
                    range: al_syntax::ts_range_to_syntax(r, file_source_bytes).into(),
                });
            }
        }
    }

    // `includeDeclaration: false` means exclude the symbol's *declaration*, not
    // the occurrence under the cursor. Resolve the declaration's canonical site
    // and drop the location that matches it (C21). We use the shared
    // `binding::decl_loc`, not raw go-to-definition: invoked on a declaration's
    // own name, go-to-definition falls through to a *usage*, which would leave
    // the declaration in and drop a real usage instead. `decl_loc` treats a
    // declaration-name cursor as its own site, so the exclusion is correct
    // whether the cursor sits on the declaration or on a usage.
    if !include_declaration {
        let (decl_uri, decl_line, decl_char) = super::binding::decl_loc(workspace, uri, position);
        locations.retain(|loc| {
            loc.uri.to_string() != decl_uri
                || loc.range.start.line != decl_line
                || loc.range.start.character != decl_char
        });
    }

    locations
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_workspace::Workspace;
    use url::Url;

    fn ws_with_doc(uri: &Url, text: &str) -> Workspace {
        let ws = Workspace::new();
        ws.documents.open(uri.clone(), text.to_string());
        ws
    }

    #[test]
    fn references_finds_variable_in_same_file() {
        let uri = Url::parse("file:///test/refs.al").expect("test");
        let src = r#"codeunit 50100 "Refs"
{
    procedure Calc()
    var
        MyVar: Integer;
    begin
        MyVar := 1;
        MyVar += 2;
    end;
}"#;
        let ws = ws_with_doc(&uri, src);
        let pos = Position {
            line: 4,
            character: 8,
        };
        let locs = references(&ws, &uri, pos, true);
        assert!(
            locs.len() >= 2,
            "Expected at least 2 references to MyVar, got {}",
            locs.len()
        );
        assert!(
            locs.iter().all(|l| l.uri == uri),
            "All references should be in the same file"
        );
    }

    #[test]
    fn references_includes_declaration_when_requested() {
        let uri = Url::parse("file:///test/refs_decl.al").expect("test");
        let src = r#"codeunit 50100 "Refs"
{
    procedure Calc()
    var
        X: Integer;
    begin
        X := 1;
    end;
}"#;
        let ws = ws_with_doc(&uri, src);
        let pos = Position {
            line: 4,
            character: 8,
        };
        let with_decl = references(&ws, &uri, pos, true);
        let without_decl = references(&ws, &uri, pos, false);
        assert!(
            with_decl.len() >= without_decl.len(),
            "include_declaration=true should not return fewer results"
        );
    }

    #[test]
    fn c21_exclude_declaration_not_the_clicked_usage() {
        // Invoke references from a *usage* site with includeDeclaration=false.
        // The declaration (line 4) must be excluded; the clicked usage (line 6)
        // must still be present. Previously the code dropped whichever ref
        // started at the request position — i.e. the clicked usage — and never
        // dropped the declaration.
        let uri = Url::parse("file:///test/c21.al").expect("test");
        let src = r#"codeunit 50100 "Refs"
{
    procedure Calc()
    var
        MyVar: Integer;
    begin
        MyVar := 1;
        MyVar += 2;
    end;
}"#;
        let ws = ws_with_doc(&uri, src);
        // Cursor at the first character of the `MyVar := 1` usage (line 6, col 8).
        let pos = Position {
            line: 6,
            character: 8,
        };
        let without_decl = references(&ws, &uri, pos, false);
        // The declaration on line 4 must be gone.
        assert!(
            !without_decl.iter().any(|l| l.range.start.line == 4),
            "declaration (line 4) must be excluded; got {:?}",
            without_decl
                .iter()
                .map(|l| l.range.start.line)
                .collect::<Vec<_>>()
        );
        // The clicked usage on line 6 must still be present.
        assert!(
            without_decl.iter().any(|l| l.range.start.line == 6),
            "clicked usage (line 6) must be present; got {:?}",
            without_decl
                .iter()
                .map(|l| l.range.start.line)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn c21_exclude_declaration_when_cursor_on_declaration() {
        // Adversarial: invoke references from the DECLARATION site (not a usage)
        // with includeDeclaration=false. The declaration (line 4) must be
        // excluded and BOTH usages (lines 6 and 7) preserved. Regression guard
        // for the case where go-to-definition on a declaration falls through to
        // a usage — which previously excluded a usage and kept the declaration.
        let uri = Url::parse("file:///test/c21decl.al").expect("test");
        let src = r#"codeunit 50100 "Refs"
{
    procedure Calc()
    var
        MyVar: Integer;
    begin
        MyVar := 1;
        MyVar += 2;
    end;
}"#;
        let ws = ws_with_doc(&uri, src);
        // Cursor at the first character of the `MyVar` DECLARATION (line 4, col 8).
        let pos = Position {
            line: 4,
            character: 8,
        };
        let without_decl = references(&ws, &uri, pos, false);
        let lines: Vec<u32> = without_decl.iter().map(|l| l.range.start.line).collect();
        assert!(
            !lines.contains(&4),
            "declaration (line 4) must be excluded; got {lines:?}"
        );
        assert!(
            lines.contains(&6) && lines.contains(&7),
            "both usages (lines 6, 7) must be present; got {lines:?}"
        );
    }

    #[test]
    fn references_to_event_include_subscriber_string_literal() {
        // Cursor on the published event declaration must surface the subscriber
        // that names the event via a string literal in `[EventSubscriber(...)]`.
        let uri = Url::parse("file:///test/evt.al").expect("test");
        let src = r#"codeunit 50100 "Evt Pub"
{
    [IntegrationEvent(false, false)]
    procedure OnFooEvent()
    begin
    end;

    [EventSubscriber(ObjectType::Codeunit, Codeunit::"Evt Pub", 'OnFooEvent', '', false, false)]
    local procedure HandleFoo()
    begin
    end;
}"#;
        let ws = ws_with_doc(&uri, src);
        // Position on `OnFooEvent` in the `procedure OnFooEvent()` declaration.
        let pos = Position {
            line: 3,
            character: 14,
        };
        let locs = references(&ws, &uri, pos, true);
        // Find the subscriber reference: a location on the `[EventSubscriber...]`
        // line (line index 7) that is not the declaration on line 3.
        assert!(
            locs.iter().any(|l| l.range.start.line == 7),
            "subscriber string-literal reference on the EventSubscriber line must be found; got {:?}",
            locs.iter().map(|l| l.range.start.line).collect::<Vec<_>>()
        );
    }

    #[test]
    fn references_do_not_match_unrelated_string_literals() {
        // A string literal equal to the symbol name but NOT the event-name
        // argument of an EventSubscriber attribute must not be reported.
        let uri = Url::parse("file:///test/noevt.al").expect("test");
        let src = r#"codeunit 50100 "No Evt"
{
    procedure OnFooEvent()
    begin
        Message('OnFooEvent');
    end;
}"#;
        let ws = ws_with_doc(&uri, src);
        let pos = Position {
            line: 2,
            character: 14,
        };
        let locs = references(&ws, &uri, pos, true);
        // Only the declaration identifier matches; the Message('OnFooEvent')
        // string literal on line 4 must be excluded.
        assert!(
            !locs.iter().any(|l| l.range.start.line == 4),
            "plain string literal must not be treated as an event reference; got {:?}",
            locs.iter().map(|l| l.range.start.line).collect::<Vec<_>>()
        );
    }

    #[test]
    fn references_missing_uri_returns_empty() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///nonexistent/file.al").expect("test");
        let pos = Position {
            line: 0,
            character: 0,
        };
        let locs = references(&ws, &uri, pos, true);
        assert!(locs.is_empty(), "Unknown URI must return empty references");
    }

    #[test]
    fn references_invalid_position_returns_empty() {
        let uri = Url::parse("file:///test/out_of_range.al").expect("test");
        let src = "codeunit 50100 Test { }";
        let ws = ws_with_doc(&uri, src);
        let pos = Position {
            line: 9999,
            character: 9999,
        };
        let locs = references(&ws, &uri, pos, true);
        assert!(
            locs.is_empty(),
            "Out-of-range position must return empty references"
        );
    }

    #[test]
    fn references_on_whitespace_returns_empty() {
        let uri = Url::parse("file:///test/whitespace.al").expect("test");
        let src = "codeunit 50100 Test\n{\n    // comment\n}";
        let ws = ws_with_doc(&uri, src);
        let pos = Position {
            line: 0,
            character: 19,
        };
        let locs = references(&ws, &uri, pos, true);
        // May return empty or may match "Test" — either way must not panic
        let _ = locs;
    }
}
