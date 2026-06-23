//! Parser/formatter performance benchmarks for al-core.
//!
//! T050: closes the deferred observability gap that previously prevented
//! us from detecting regressions in `AlParser::parse`,
//! `AlParser::parse_incremental`, and `format_al`. These three functions
//! sit on the LSP keystroke-triggered hot path, so a 2× regression in any
//! one of them is user-visible (lag while typing).
//!
//! Run with:
//!   cargo bench -p al-core --bench parser
//!
//! Each benchmark sets up state outside the measurement loop so Criterion
//! only measures the parser/formatter cost.

use criterion::{black_box, criterion_group, criterion_main, Criterion};

use al_lsp::syntax::formatting::{format_al, FormatOptions};
use al_lsp::syntax::parser::AlParser;

// A small AL codeunit — keystroke-typical workload (≈30 lines).
const SMALL_AL: &str = r#"
codeunit 50100 "Sample Codeunit"
{
    procedure Add(A: Integer; B: Integer): Integer
    begin
        exit(A + B);
    end;

    procedure Concat(Prefix: Text; Suffix: Text) Result: Text
    begin
        Result := Prefix + ' ' + Suffix;
    end;

    procedure Loop(Count: Integer)
    var
        I: Integer;
    begin
        for I := 1 to Count do
            Message('iteration %1', I);
    end;
}
"#;

// A medium-sized AL file with a few procedures and triggers (≈80 lines).
const MEDIUM_AL: &str = r#"
codeunit 50101 "Medium Codeunit"
{
    Subtype = Test;

    [Test]
    procedure TestOne()
    var
        Result: Integer;
    begin
        Result := 1 + 2;
        if Result <> 3 then
            Error('Expected 3, got %1', Result);
    end;

    [Test]
    procedure TestTwo()
    var
        Result: Text;
    begin
        Result := 'Hello' + ' ' + 'World';
        if Result <> 'Hello World' then
            Error('Got %1', Result);
    end;

    procedure Helper(Input: Text): Text
    var
        Buffer: Text;
        I: Integer;
    begin
        for I := 1 to StrLen(Input) do
            Buffer += CopyStr(Input, I, 1);
        exit(Buffer);
    end;

    trigger OnRun()
    begin
        Message('Running tests');
    end;
}
"#;

fn bench_parse(c: &mut Criterion) {
    c.bench_function("parser/parse_small_30loc", |b| {
        let mut parser = AlParser::new();
        b.iter(|| {
            let r = parser.parse(black_box(SMALL_AL));
            black_box(r);
        });
    });

    c.bench_function("parser/parse_medium_80loc", |b| {
        let mut parser = AlParser::new();
        b.iter(|| {
            let r = parser.parse(black_box(MEDIUM_AL));
            black_box(r);
        });
    });
}

// bench_parse_incremental — re-parse with a prior tree.
// Models a single-character edit between calls — the typical keystroke path.
fn bench_parse_incremental(c: &mut Criterion) {
    c.bench_function("parser/parse_incremental_small_30loc", |b| {
        let mut parser = AlParser::new();
        let prev = parser.parse(SMALL_AL).tree;
        b.iter(|| {
            let r = parser.parse_incremental(black_box(SMALL_AL), &prev);
            black_box(r);
        });
    });

    c.bench_function("parser/parse_incremental_medium_80loc", |b| {
        let mut parser = AlParser::new();
        let prev = parser.parse(MEDIUM_AL).tree;
        b.iter(|| {
            let r = parser.parse_incremental(black_box(MEDIUM_AL), &prev);
            black_box(r);
        });
    });
}

fn bench_format(c: &mut Criterion) {
    let opts = FormatOptions::default();

    c.bench_function("formatter/format_al_small_30loc", |b| {
        b.iter(|| {
            let r = format_al(black_box(SMALL_AL), black_box(&opts));
            black_box(r);
        });
    });

    c.bench_function("formatter/format_al_medium_80loc", |b| {
        b.iter(|| {
            let r = format_al(black_box(MEDIUM_AL), black_box(&opts));
            black_box(r);
        });
    });
}

criterion_group!(benches, bench_parse, bench_parse_incremental, bench_format);
criterion_main!(benches);
