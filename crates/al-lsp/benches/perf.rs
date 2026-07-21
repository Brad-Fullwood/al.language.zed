//! Deterministic, fixture-based performance benchmarks for the symbol /
//! insight hot paths.
//!
//! The hot paths benchmarked here
//! all sit behind interactive LSP queries (workspace load, go-to-definition,
//! completion, `al impact`, `al trace`), so a regression in any one of them is
//! user-visible.
//!
//! Run with:
//!   cargo bench -p al-lsp --bench perf
//! Quick smoke-run:
//!   cargo bench -p al-lsp --bench perf -- --warm-up-time 1 --measurement-time 2
//! Single group:
//!   cargo bench -p al-lsp --bench perf -- symbols
//!   cargo bench -p al-lsp --bench perf -- insight
//!   cargo bench -p al-lsp --bench perf -- completion
//! Compile only (no run):
//!   cargo bench -p al-lsp --bench perf --no-run
//!
//! Determinism: the fixture is a *synthetic in-bench* AL workspace built from a
//! fixed seed of object names/ids (no randomness, no clock, no filesystem), so
//! the same work is measured on every run and the memory metric is stable.
//! See `Docs/benchmarks.md` for how to read the output, including the one-shot
//! `[MEMORY]` line (indexed symbol count / serialized bytes / graph size).
//!
//! Each benchmark sets up state OUTSIDE the measurement loop so Criterion only
//! times the hot path (the one exception, `symbols/cold_load_parse_index`,
//! deliberately includes the JSON parse + index because that *is* the cold-load
//! hot path).

use std::sync::atomic::{AtomicBool, Ordering};

use criterion::{black_box, criterion_group, criterion_main, Criterion};

use al_analysis::queries::{completions::completions, Position};
use al_insight::analysis::table_impact;
use al_insight::graph::InsightGraph;
use al_insight::index::CallGraph;
use al_insight::search::trace_event;
use al_symbols::{
    AttributeSymbol, EnumValueSymbol, FieldSymbol, KeySymbol, MethodSymbol, ObjectKind,
    ParameterSymbol, PropertyValue, SymbolEntry, SymbolIndex, VariableSymbol,
};
use al_workspace::Workspace;
use url::Url;

// ---------------------------------------------------------------------------
// Fixture sizing. A single SCALE knob keeps the synthetic workspace
// proportional and easy to grow if the hot paths get faster. The defaults
// model a small-to-medium BC app (~640 objects, a few methods/fields each):
// large enough that lookups/builds take measurable time, small enough that a
// full `cargo bench` finishes in seconds.
const N_TABLES: i32 = 200;
const N_CODEUNITS: i32 = 200;
const N_PAGES: i32 = 120;
const N_ENUMS: i32 = 80;
const N_INTERFACES: i32 = 40;

const TABLE_ID_BASE: i32 = 50000;
const CODEUNIT_ID_BASE: i32 = 60000;
const PAGE_ID_BASE: i32 = 70000;
const ENUM_ID_BASE: i32 = 80000;
const INTERFACE_ID_BASE: i32 = 90000;

// Object 0 is the "hot" target every other object points at, so `impact` and
// `trace` queries against it exercise a realistic fan-in.
const HOT_TABLE: &str = "BenchTable0";
const HOT_EVENT: &str = "OnBenchEvent0";

// ---------------------------------------------------------------------------
// Small constructors (the model structs only derive Default for SymbolEntry).

fn attr(name: &str, args: &[&str]) -> AttributeSymbol {
    AttributeSymbol {
        name: name.to_string(),
        arguments: args.iter().map(|s| (*s).to_string()).collect(),
    }
}

fn method(
    name: &str,
    attributes: Vec<AttributeSymbol>,
    parameters: Vec<ParameterSymbol>,
) -> MethodSymbol {
    MethodSymbol {
        name: name.to_string(),
        parameters,
        return_type: None,
        attributes,
        is_local: false,
    }
}

fn rec_param(name: &str, table: &str) -> ParameterSymbol {
    ParameterSymbol {
        name: name.to_string(),
        type_name: format!("Record \"{table}\""),
        is_var: false,
    }
}

fn rec_var(name: &str, table: &str) -> VariableSymbol {
    VariableSymbol {
        name: name.to_string(),
        type_name: format!("Record \"{table}\""),
        is_protected: false,
    }
}

// ---------------------------------------------------------------------------
// Deterministic fixture generation.

/// Build the full set of synthetic symbol entries. Pure function of the SCALE
/// constants — identical output on every call.
fn make_entries() -> Vec<SymbolEntry> {
    let mut entries = Vec::with_capacity(
        (N_TABLES + N_CODEUNITS + N_PAGES + N_ENUMS + N_INTERFACES) as usize + 4,
    );

    // Tables: a PK plus a "Linked" field whose TableRelation points at the hot
    // table — gives `impact(HOT_TABLE)` a Relation hit from every table and the
    // insight graph a RelatesTo edge from every table.
    for i in 0..N_TABLES {
        entries.push(SymbolEntry {
            kind: ObjectKind::Table,
            id: TABLE_ID_BASE + i,
            name: format!("BenchTable{i}"),
            package: "BenchPkg".to_string(),
            fields: vec![
                FieldSymbol {
                    id: 1,
                    name: "No.".to_string(),
                    type_name: "Code[20]".to_string(),
                    properties: Vec::new(),
                },
                FieldSymbol {
                    id: 2,
                    name: "Linked".to_string(),
                    type_name: "Code[20]".to_string(),
                    properties: vec![PropertyValue {
                        name: "TableRelation".to_string(),
                        value: format!("\"{HOT_TABLE}\""),
                    }],
                },
            ],
            keys: vec![KeySymbol {
                name: "PK".to_string(),
                field_names: vec!["No.".to_string()],
                properties: Vec::new(),
            }],
            ..Default::default()
        });
    }

    // A couple of table extensions over the hot table so `impact` sees Extends.
    for i in 0..2 {
        entries.push(SymbolEntry {
            kind: ObjectKind::TableExtension,
            id: TABLE_ID_BASE + N_TABLES + i,
            name: format!("BenchTableExt{i}"),
            package: "BenchPkg".to_string(),
            extends: Some(HOT_TABLE.to_string()),
            fields: vec![FieldSymbol {
                id: 100 + i,
                name: format!("ExtraField{i}"),
                type_name: "Integer".to_string(),
                properties: Vec::new(),
            }],
            ..Default::default()
        });
    }

    // Codeunits form a ring: codeunit i publishes OnBenchEvent{i} and subscribes
    // to the previous codeunit's event. trace(OnBenchEvent0) therefore walks the
    // whole ring (multi-hop) up to max_depth. Each also touches a record so the
    // impact query has RecordVariable / RecordParameter hits.
    for i in 0..N_CODEUNITS {
        let prev = (i + N_CODEUNITS - 1) % N_CODEUNITS;
        let table_ref = format!("BenchTable{}", i % N_TABLES);
        entries.push(SymbolEntry {
            kind: ObjectKind::Codeunit,
            id: CODEUNIT_ID_BASE + i,
            name: format!("BenchCodeunit{i}"),
            package: "BenchPkg".to_string(),
            methods: vec![
                // Publisher.
                method(
                    &format!("OnBenchEvent{i}"),
                    vec![attr("IntegrationEvent", &["false", "false"])],
                    Vec::new(),
                ),
                // Subscriber to the previous codeunit's event.
                method(
                    &format!("HandleEvent{i}"),
                    vec![attr(
                        "EventSubscriber",
                        &[
                            "ObjectType::Codeunit",
                            &format!("Codeunit::\"BenchCodeunit{prev}\""),
                            &format!("OnBenchEvent{prev}"),
                        ],
                    )],
                    Vec::new(),
                ),
                // A plain procedure with a record parameter (impact: RecordParameter).
                method(
                    &format!("DoWork{i}"),
                    Vec::new(),
                    vec![rec_param("Rec", &table_ref)],
                ),
            ],
            // Record variable (impact: RecordVariable).
            variables: vec![rec_var("WorkRec", &table_ref)],
            ..Default::default()
        });
    }

    for i in 0..N_PAGES {
        entries.push(SymbolEntry {
            kind: ObjectKind::Page,
            id: PAGE_ID_BASE + i,
            name: format!("BenchPage{i}"),
            package: "BenchPkg".to_string(),
            properties: vec![PropertyValue {
                name: "SourceTable".to_string(),
                value: format!("BenchTable{}", i % N_TABLES),
            }],
            methods: vec![method(&format!("OnOpenPage{i}"), Vec::new(), Vec::new())],
            ..Default::default()
        });
    }

    for i in 0..N_ENUMS {
        entries.push(SymbolEntry {
            kind: ObjectKind::Enum,
            id: ENUM_ID_BASE + i,
            name: format!("BenchEnum{i}"),
            package: "BenchPkg".to_string(),
            enum_values: vec![
                EnumValueSymbol {
                    ordinal: 0,
                    name: "Open".to_string(),
                },
                EnumValueSymbol {
                    ordinal: 1,
                    name: "Closed".to_string(),
                },
            ],
            ..Default::default()
        });
    }

    for i in 0..N_INTERFACES {
        entries.push(SymbolEntry {
            kind: ObjectKind::Interface,
            id: INTERFACE_ID_BASE + i,
            name: format!("BenchInterface{i}"),
            package: "BenchPkg".to_string(),
            methods: vec![method(
                &format!("Run{i}"),
                Vec::new(),
                vec![ParameterSymbol {
                    name: "Input".to_string(),
                    type_name: "Integer".to_string(),
                    is_var: false,
                }],
            )],
            ..Default::default()
        });
    }

    entries
}

/// Build and index the fixture (the "warm" state shared by lookup benches).
fn build_index() -> SymbolIndex {
    let idx = SymbolIndex::new();
    idx.add_entries_owned(make_entries());
    idx
}

/// Build the insight graph from an indexed fixture (publishes/subscribes/etc.).
fn build_graph(symbols: &SymbolIndex) -> InsightGraph {
    let mut g = InsightGraph::new();
    g.build_from_index(symbols);
    g
}

// ---------------------------------------------------------------------------
// Memory metric — printed exactly once per process, regardless of which group
// runs (so it shows even under a `-- <filter>`). Reports the approximate memory
// footprint of the index alongside the timing benches.

fn report_stats_once() {
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::SeqCst) {
        return;
    }
    let entries = make_entries();
    let count = entries.len();
    // Serialized-JSON length is an *approximate* memory metric: it is the
    // on-disk SymbolReference footprint, a stable proxy for in-memory size
    // (the live index also holds Arc/DashMap overhead not counted here).
    let bytes = serde_json::to_string(&entries)
        .map(|s| s.len())
        .unwrap_or(0);
    let idx = SymbolIndex::new();
    idx.add_entries_owned(entries);
    let graph = build_graph(&idx);
    eprintln!(
        "\n[C6 MEMORY] indexed_symbols={count} serialized_bytes={bytes} \
         bytes_per_symbol={} insight_nodes={} insight_edges={}\n",
        bytes / count.max(1),
        graph.node_count(),
        graph.edge_count(),
    );
}

// ---------------------------------------------------------------------------
// Benchmarks.

/// Cold load (parse + index) and warm lookups (by name & id, plus search).
fn bench_symbols(c: &mut Criterion) {
    report_stats_once();

    // Cold load: serde parse of the symbol JSON + concurrent index insert.
    // This is the real workspace-open hot path (production parses
    // SymbolReference.json and feeds it into the same `add_entries_owned`).
    let entries = make_entries();
    let json = serde_json::to_string(&entries).expect("serialize fixture");
    c.bench_function("symbols/cold_load_parse_index", |b| {
        b.iter(|| {
            let parsed: Vec<SymbolEntry> =
                serde_json::from_str(black_box(&json)).expect("parse fixture");
            let idx = SymbolIndex::new();
            idx.add_entries_owned(parsed);
            black_box(idx.len())
        });
    });

    // Warm lookups against a pre-built index.
    let idx = build_index();
    let mut group = c.benchmark_group("symbols/warm_lookup");
    group.bench_function("get_by_name", |b| {
        b.iter(|| black_box(idx.get_by_name(black_box("BenchTable100"))));
    });
    group.bench_function("find_by_name", |b| {
        b.iter(|| black_box(idx.find_by_name(black_box("BenchCodeunit150"))));
    });
    group.bench_function("get_by_id", |b| {
        b.iter(|| black_box(idx.get_by_id(ObjectKind::Table, black_box(TABLE_ID_BASE + 100))));
    });
    group.bench_function("search_substring", |b| {
        b.iter(|| black_box(idx.search(black_box("Bench"), 50)));
    });
    group.finish();
}

/// Insight engine: graph build (cold), trace, table-impact, call-graph build.
fn bench_insight(c: &mut Criterion) {
    report_stats_once();

    let symbols = build_index();

    // Cold build of the insight graph from the symbol index.
    c.bench_function("insight/build_graph_from_index", |b| {
        b.iter(|| {
            let g = build_graph(black_box(&symbols));
            black_box(g.edge_count())
        });
    });

    // Trace + impact + call-graph run against a pre-built graph/index.
    let graph = build_graph(&symbols);
    c.bench_function("insight/trace_event", |b| {
        b.iter(|| black_box(trace_event(black_box(&graph), HOT_EVENT, 10)));
    });
    c.bench_function("insight/table_impact", |b| {
        b.iter(|| black_box(table_impact(black_box(&symbols), HOT_TABLE)));
    });
    c.bench_function("insight/callgraph_from_insight", |b| {
        b.iter(|| black_box(CallGraph::build_from_insight(black_box(&graph))));
    });
}

/// Completion at a position. Two contexts: a type position (exercises the
/// symbol index via `get_by_kind`) and the default keystroke context.
fn bench_completion(c: &mut Criterion) {
    report_stats_once();

    let ws = Workspace::new();
    ws.symbols.add_entries_owned(make_entries());

    let uri = Url::parse("file:///bench/BenchCompletion.al").expect("uri");
    // Build the document line-by-line so cursor positions are derived from the
    // text, not hand-counted (robust to edits).
    let lines = [
        "codeunit 50000 BenchCompletion", // 0
        "{",                              // 1
        "    procedure DoWork()",         // 2
        "    var",                        // 3
        "        MyVar : ",               // 4  <- type position (cursor at EOL)
        "    begin",                      // 5
        "        Message",                // 6  <- default context (cursor at EOL)
        "    end;",                       // 7
        "}",                              // 8
    ];
    let text = lines.join("\n");
    ws.documents.open(uri.clone(), text);

    let type_pos = Position {
        line: 4,
        character: lines[4].chars().count() as u32,
    };
    let default_pos = Position {
        line: 6,
        character: lines[6].chars().count() as u32,
    };

    c.bench_function("completion/type_position", |b| {
        b.iter(|| black_box(completions(black_box(&ws), black_box(&uri), type_pos)));
    });
    c.bench_function("completion/default", |b| {
        b.iter(|| black_box(completions(black_box(&ws), black_box(&uri), default_pos)));
    });
}

criterion_group!(benches, bench_symbols, bench_insight, bench_completion);
criterion_main!(benches);
