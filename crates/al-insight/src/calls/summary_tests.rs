//! A graph built from source summaries must equal the graph built from the
//! trees of the same sources.

use super::*;
use al_source::file_index::FileIndex;
use petgraph::visit::EdgeRef;

/// Constructs the summary path must keep: interface dispatch, record
/// triggers, `Codeunit.Run`, an event with a same-file subscriber, a
/// `[TryFunction]` with a write, a `Commit()`, an overloaded name, a
/// temporary record, and several objects in one file.
const TRICKY: &str = r#"interface "Fixture Shipper"
{
    procedure Ship(Qty: Integer);
}

codeunit 50300 "Fixture Truck" implements "Fixture Shipper"
{
    procedure Ship(Qty: Integer)
    var
        Entry: Record "Fixture Entry";
    begin
        Entry.Insert(true);
        OnAfterShip(Qty);
    end;

    [IntegrationEvent(false, false)]
    local procedure OnAfterShip(Qty: Integer)
    begin
    end;
}

codeunit 50301 "Fixture Dispatcher"
{
    trigger OnRun()
    begin
        TryPost();
    end;

    procedure Dispatch(Shipper: Interface "Fixture Shipper")
    var
        Truck: Codeunit "Fixture Truck";
    begin
        Shipper.Ship(1);
        Truck.Ship(2);
        Codeunit.Run(Codeunit::"Fixture Truck");
        Helper();
        Helper(1);
    end;

    local procedure Helper()
    begin
        Commit();
    end;

    local procedure Helper(Value: Integer)
    var
        Entry: Record "Fixture Entry";
    begin
        Entry.Modify();
    end;

    [TryFunction]
    procedure TryPost()
    var
        Entry: Record "Fixture Entry";
        Buffer: Record "Fixture Entry" temporary;
    begin
        Entry.Delete();
        Buffer.Insert();
    end;

    [EventSubscriber(ObjectType::Codeunit, Codeunit::"Fixture Truck", 'OnAfterShip', '', false, false)]
    local procedure HandleShip(Qty: Integer)
    begin
        Dispatch(Qty);
    end;

    [EventSubscriber(ObjectType::Table, Database::"Fixture Entry", 'OnAfterInsertEvent', '', false, false)]
    local procedure HandleEntryInsert(var Rec: Record "Fixture Entry"; RunTrigger: Boolean)
    begin
        Helper();
    end;
}

table 50302 "Fixture Entry"
{
    fields
    {
        field(1; "No."; Code[20]) { }
    }

    trigger OnInsert()
    begin
    end;
}
"#;

/// The harness project's sources plus [`TRICKY`], under dependency-like paths.
fn fixture_sources() -> Vec<(std::path::PathBuf, String)> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../al-test-harness/data/test_al_project/src");
    let mut sources: Vec<(std::path::PathBuf, String)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("fixture project {}: {error}", dir.display()))
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "al"))
        .map(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&path).unwrap();
            (
                std::path::PathBuf::from(format!("/deps/fixture/{name}")),
                text,
            )
        })
        .collect();
    sources.push((
        std::path::PathBuf::from("/deps/fixture/Tricky.al"),
        TRICKY.to_string(),
    ));
    sources.sort();
    assert!(sources.len() > 20, "fixture project is missing");
    sources
}

/// Package symbols for the fixture objects, as a loaded `.app` would give
/// them, so member calls and interface dispatch have something to resolve to.
fn fixture_symbols(sources: &[(std::path::PathBuf, String)]) -> SymbolIndex {
    let index = FileIndex::new();
    for (path, text) in sources {
        index.add_file(path.clone(), text.clone());
    }
    let symbols = SymbolIndex::new();
    register_workspace_nodes(&index, &symbols, &mut InsightGraph::new()).unwrap();
    // The implicit table events a compiled package lists for every table.
    let table_events = ["Insert", "Modify", "Delete"]
        .iter()
        .flat_map(|op| [format!("OnBefore{op}Event"), format!("OnAfter{op}Event")])
        .map(|name| al_symbols::MethodSymbol {
            name,
            parameters: vec![],
            return_type: None,
            is_local: false,
            attributes: vec![al_symbols::AttributeSymbol {
                name: "IntegrationEvent".to_string(),
                arguments: vec!["false".to_string(), "false".to_string()],
            }],
        })
        .collect();
    symbols.add_entries(&[al_symbols::SymbolEntry {
        kind: ObjectKind::Table,
        id: 50302,
        name: "Fixture Entry".to_string(),
        package: "Fixture".to_string(),
        methods: table_events,
        ..Default::default()
    }]);
    symbols
}

fn describe_nodes(graph: &InsightGraph) -> Vec<String> {
    graph
        .graph
        .node_indices()
        .map(|index| format!("{:?}", graph.graph[index]))
        .collect()
}

fn describe_insight_edges(graph: &InsightGraph) -> Vec<String> {
    let mut edges: Vec<String> = graph
        .graph
        .edge_references()
        .map(|edge| {
            format!(
                "{:?} -{:?}-> {:?}",
                graph.graph[edge.source()],
                edge.weight(),
                graph.graph[edge.target()]
            )
        })
        .collect();
    edges.sort();
    edges
}

fn describe_call_graph(graph: &CallGraph) -> (Vec<String>, Vec<String>) {
    let mut ids: Vec<NodeId> = graph.node_ids().collect();
    ids.sort_by_key(|id| id.0);
    let describe = |id: NodeId| {
        graph
            .node_info(id)
            .map(|info| format!("{}:{}::{}", info.node_type, info.object, info.name))
            .unwrap_or_else(|| format!("#{}", id.0))
    };
    let mut edges = Vec::new();
    let mut states = Vec::new();
    for id in ids {
        states.push(format!("{} {:?}", describe(id), graph.resolution_state(id)));
        for edge in graph.callees_of(id) {
            edges.push(format!(
                "{} -{:?}-> {}",
                describe(edge.from),
                edge.kind,
                describe(edge.to)
            ));
        }
    }
    edges.sort();
    (edges, states)
}

struct Built {
    insight: InsightGraph,
    eager: CallGraph,
    eager_resolved: usize,
    complete: CallGraph,
    complete_resolved: usize,
}

fn build_from_trees(sources: &[(std::path::PathBuf, String)], symbols: &SymbolIndex) -> Built {
    let index = FileIndex::new();
    for (path, text) in sources {
        index.add_file(path.clone(), text.clone());
    }
    let mut insight = InsightGraph::new();
    insight.build_from_index(symbols);
    register_dependency_source_nodes(&index, &mut insight).unwrap();
    let mut eager = CallGraph::build_from_insight(&insight);
    let eager_resolved =
        populate_workspace_call_edges(&index, symbols, &insight, &mut eager).unwrap();
    let mut complete = CallGraph::build_from_insight(&insight);
    let complete_resolved =
        resolve_all_workspace_call_edges(&index, symbols, &insight, &mut complete).unwrap();
    Built {
        insight,
        eager,
        eager_resolved,
        complete,
        complete_resolved,
    }
}

fn summarize(
    sources: &[(std::path::PathBuf, String)],
) -> Vec<(std::path::PathBuf, SourceFileSummary)> {
    sources
        .iter()
        .map(|(path, text)| {
            let parsed = al_syntax::AlParser::parse_quick(text);
            assert!(
                parsed.errors.is_empty(),
                "{} does not parse",
                path.display()
            );
            (
                path.clone(),
                SourceFileSummary::from_tree(path.to_string_lossy(), &parsed.tree, text),
            )
        })
        .collect()
}

fn build_from_summaries(
    summaries: &[(std::path::PathBuf, SourceFileSummary)],
    symbols: &SymbolIndex,
) -> Built {
    let files: Vec<SummarizedFile<'_>> = summaries
        .iter()
        .map(|(path, summary)| (path.as_path(), summary))
        .collect();
    let mut insight = InsightGraph::new();
    insight.build_from_index(symbols);
    register_dependency_summary_nodes(&files, &mut insight).unwrap();
    let mut eager = CallGraph::build_from_insight(&insight);
    let eager_resolved =
        populate_summary_call_edges(&files, symbols, &insight, &mut eager).unwrap();
    let mut complete = CallGraph::build_from_insight(&insight);
    let complete_resolved =
        resolve_all_summary_call_edges(&files, symbols, &insight, &mut complete).unwrap();
    Built {
        insight,
        eager,
        eager_resolved,
        complete,
        complete_resolved,
    }
}

#[test]
fn summary_graph_equals_tree_graph_node_for_node_and_edge_for_edge() {
    let sources = fixture_sources();
    let symbols = fixture_symbols(&sources);
    let trees = build_from_trees(&sources, &symbols);
    let summaries = build_from_summaries(&summarize(&sources), &symbols);

    // Same nodes in the same order, so node ids agree as well.
    assert_eq!(
        describe_nodes(&summaries.insight),
        describe_nodes(&trees.insight)
    );
    assert_eq!(
        describe_insight_edges(&summaries.insight),
        describe_insight_edges(&trees.insight)
    );

    assert_eq!(summaries.eager_resolved, trees.eager_resolved);
    assert_eq!(
        describe_call_graph(&summaries.eager),
        describe_call_graph(&trees.eager)
    );
    assert_eq!(summaries.complete_resolved, trees.complete_resolved);
    let (complete_edges, _) = describe_call_graph(&trees.complete);
    assert_eq!(
        describe_call_graph(&summaries.complete),
        describe_call_graph(&trees.complete)
    );

    // The comparison means something only if the fixture produced edges of
    // every kind the resolver adds.
    for kind in [
        "DirectCall",
        "IndirectCall",
        "RecordTrigger",
        "EventSubscription",
    ] {
        assert!(
            complete_edges
                .iter()
                .any(|edge| edge.contains(&format!("-{kind}->"))),
            "no {kind} edge in the fixture graph: {complete_edges:#?}"
        );
    }
}

#[test]
fn summary_keeps_every_object_of_a_multi_object_file_and_its_effects() {
    let parsed = al_syntax::AlParser::parse_quick(TRICKY);
    let summary = SourceFileSummary::from_tree("Tricky.al", &parsed.tree, TRICKY);
    let names: Vec<&str> = summary.objects.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Fixture Shipper",
            "Fixture Truck",
            "Fixture Dispatcher",
            "Fixture Entry"
        ]
    );
    assert_eq!(summary.effects, file_effect_sites(&parsed.tree, TRICKY));

    let try_post = summary
        .effects
        .iter()
        .find(|effect| effect.name == "TryPost")
        .expect("TryPost effects");
    let labels: Vec<&str> = try_post.writes.iter().map(|w| w.label.as_str()).collect();
    assert_eq!(
        labels,
        ["database write Entry.Delete()"],
        "a temporary record is not a write"
    );
    assert!(summary
        .effects
        .iter()
        .any(|effect| effect.name == "Helper" && effect.commits.len() == 1));
}

#[test]
fn summary_survives_a_json_round_trip() {
    let summaries = summarize(&fixture_sources());
    for (path, summary) in &summaries {
        let json = serde_json::to_vec(summary).unwrap();
        let back: SourceFileSummary = serde_json::from_slice(&json).unwrap();
        assert_eq!(&back, summary, "{}", path.display());
    }
}
