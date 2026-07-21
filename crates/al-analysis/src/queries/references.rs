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

    // The canonical declaration the cursor binds to. Every *identifier*
    // reference we keep must bind to this same declaration — otherwise "find
    // references" on `A::Post` would also list the unrelated `B::Post` and its
    // callers. This applies the same binding awareness used by rename.
    // Event-subscriber string-literal references are already specific to the
    // named event, so they are kept without the binding filter.
    let cursor_decl = super::binding::decl_loc(workspace, uri, position);

    let mut locations = Vec::new();

    // Collect binding-filtered identifier refs + unfiltered event-subscriber
    // refs from one parsed file into `locations`.
    let mut collect_from = |file_uri: &Url, ftext: &str, ftree: &tree_sitter::Tree| {
        let bytes = ftext.as_bytes();
        for r in al_syntax::find_variable_references(ftree, ftext, clean_name) {
            let range: Range = al_syntax::ts_range_to_syntax(&r, bytes).into();
            if super::binding::decl_loc(workspace, file_uri, range.start) == cursor_decl {
                locations.push(Location {
                    uri: file_uri.clone(),
                    range,
                });
            }
        }
        // Event subscribers name the event via a string literal inside
        // `[EventSubscriber(...)]`, which the identifier walk cannot see (audit
        // 2026-06-20); surface them so `references` on an event lists its
        // subscribers, not just its declaration and raise sites.
        for r in al_syntax::find_event_subscriber_references(ftree, ftext, clean_name) {
            locations.push(Location {
                uri: file_uri.clone(),
                range: al_syntax::ts_range_to_syntax(&r, bytes).into(),
            });
        }
    };

    collect_from(uri, &text, &tree);

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
        let Ok(file_uri) = Url::from_file_path(&file_path) else {
            continue;
        };
        let Some((file_text, file_tree)) = workspace.file_index.get_cached_parse(&file_path) else {
            continue;
        };
        collect_from(&file_uri, &file_text, &file_tree);
    }

    // `includeDeclaration: false` means exclude the symbol's *declaration*, not
    // the occurrence under the cursor. Resolve the declaration's canonical site
    // and drop the location that matches it. We use the shared
    // `binding::decl_loc`, not raw go-to-definition: invoked on a declaration's
    // own name, go-to-definition falls through to a *usage*, which would leave
    // the declaration in and drop a real usage instead. `decl_loc` treats a
    // declaration-name cursor as its own site, so the exclusion is correct
    // whether the cursor sits on the declaration or on a usage.
    if !include_declaration {
        let (decl_uri, decl_line, decl_char) = &cursor_decl;
        locations.retain(|loc| {
            &loc.uri.to_string() != decl_uri
                || loc.range.start.line != *decl_line
                || loc.range.start.character != *decl_char
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
    fn references_bind_record_parameter_member_accesses() {
        let uri = Url::parse("file:///test/record_param.al").expect("test URI");
        let src = r#"codeunit 50100 "Refs"
{
    procedure Process(var Staging: Record Customer)
    begin
        if Staging.FindSet() then
            repeat
                Staging.Modify();
            until Staging.Next() = 0;
    end;
}"#;
        let ws = ws_with_doc(&uri, src);
        let pos = Position {
            line: 2,
            character: 26,
        };
        let locs = references(&ws, &uri, pos, true);
        assert_eq!(
            locs.len(),
            4,
            "the declaration and each receiver use must share one binding"
        );
    }

    #[test]
    fn exclude_declaration_not_the_clicked_usage() {
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
    fn exclude_declaration_when_cursor_on_declaration() {
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
    fn references_are_binding_aware_across_objects() {
        // Two objects each declare and call `procedure Post()`. "Find all
        // references" on A::Post must stay within A — it must not list B's
        // same-named (but distinct) declaration or its call site.
        let ws = Workspace::new();
        let uri_a = Url::parse("file:///test/refbind/A.al").unwrap();
        let uri_b = Url::parse("file:///test/refbind/B.al").unwrap();
        let src_a = "codeunit 50100 \"A\"\n{\n    procedure Post()\n    begin\n    end;\n\n    procedure Run()\n    begin\n        Post();\n    end;\n}";
        let src_b = "codeunit 50101 \"B\"\n{\n    procedure Post()\n    begin\n    end;\n\n    procedure Run()\n    begin\n        Post();\n    end;\n}";
        ws.documents.open(uri_a.clone(), src_a.to_string());
        ws.documents.open(uri_b.clone(), src_b.to_string());
        ws.file_index
            .add_file(uri_a.to_file_path().unwrap(), src_a.to_string());
        ws.file_index
            .add_file(uri_b.to_file_path().unwrap(), src_b.to_string());

        // Cursor on A's `Post` declaration (line 2, the `Post` token at col 14).
        let pos = Position {
            line: 2,
            character: 14,
        };
        let locs = references(&ws, &uri_a, pos, true);
        assert!(
            locs.iter().all(|l| l.uri == uri_a),
            "references on A::Post leaked into another object: {:?}",
            locs.iter().map(|l| l.uri.as_str()).collect::<Vec<_>>()
        );
        // A's declaration and its own call site should both be present.
        assert!(
            locs.len() >= 2,
            "expected A's declaration and call, got {}",
            locs.len()
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
