//! Parser and formatter performance benchmarks.
//!
//! Run with:
//!   cargo bench -p al-syntax --bench parser
//!
//! Each benchmark sets up state outside the measurement loop so Criterion
//! only measures the parser/formatter cost.

use criterion::{black_box, criterion_group, criterion_main, Criterion};

use al_syntax::formatting::{format_al, FormatOptions};
use al_syntax::lint::lint;
use al_syntax::parser::AlParser;
use al_syntax::{extract_document_symbols, extract_semantic_tokens};
use tree_sitter::{InputEdit, Point};

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

/// A table with `field_count` fields, standing in for the 10 000-line files
/// the BC base app ships. The costs that grow with file size — one range
/// conversion per symbol, one per semantic token — are invisible at 80 lines.
fn large_table(field_count: usize) -> String {
    let mut al = String::from("table 50100 \"Large Table\"\n{\n    fields\n    {\n");
    for i in 1..=field_count {
        al.push_str(&format!(
            "        field({i}; \"Field {i}\"; Text[50])\n        {{\n            \
             Caption = 'Field {i}';\n            DataClassification = CustomerContent;\n        }}\n"
        ));
    }
    al.push_str("    }\n\n    var\n");
    for i in 1..=field_count {
        al.push_str(&format!("        Local{i}: Integer;\n"));
    }
    al.push_str("\n    procedure Compute()\n    var\n        Total: Integer;\n    begin\n");
    for i in 1..=field_count {
        al.push_str(&format!("        Total += Local{i};\n"));
    }
    al.push_str("    end;\n}\n");
    al
}

/// Insert `ch` at `byte_offset` and describe the insertion as an `InputEdit`.
///
/// The point deltas assume `byte_offset` sits on `row`/`column` and that `ch`
/// is a one-byte, non-newline character.
fn insert_one_char(
    source: &str,
    byte_offset: usize,
    row: usize,
    column: usize,
) -> (String, InputEdit) {
    let mut edited = String::with_capacity(source.len() + 1);
    edited.push_str(&source[..byte_offset]);
    edited.push('x');
    edited.push_str(&source[byte_offset..]);

    let position = Point { row, column };
    let edit = InputEdit {
        start_byte: byte_offset,
        old_end_byte: byte_offset,
        new_end_byte: byte_offset + 1,
        start_position: position,
        old_end_position: position,
        new_end_position: Point {
            row,
            column: column + 1,
        },
    };
    (edited, edit)
}

// bench_parse_incremental — the keystroke path: apply a one-character
// insertion to the old tree with `Tree::edit`, then re-parse the edited text
// against it. Re-parsing byte-identical text would report tree reuse instead.
fn bench_parse_incremental(c: &mut Criterion) {
    let mut cases: Vec<(&str, String, usize, usize, usize)> = Vec::new();
    for (name, source) in [("small_30loc", SMALL_AL), ("medium_80loc", MEDIUM_AL)] {
        // Insert into the first `Message(` argument, deep inside a procedure
        // body, so the edit invalidates a real subtree.
        let offset = source
            .find("Message('")
            .expect("fixture has a Message call")
            + "Message('".len();
        let row = source[..offset].matches('\n').count();
        let column = offset - source[..offset].rfind('\n').map_or(0, |i| i + 1);
        cases.push((name, source.to_string(), offset, row, column));
    }

    for (name, source, offset, row, column) in cases {
        c.bench_function(&format!("parser/parse_incremental_{name}"), |b| {
            let mut parser = AlParser::new();
            let (edited, edit) = insert_one_char(&source, offset, row, column);
            let mut prev = parser.parse(&source).tree;
            prev.edit(&edit);
            b.iter(|| {
                let r = parser.parse_incremental(black_box(&edited), &prev);
                black_box(r);
            });
        });
    }
}

// The per-file hot paths, on a fixture large enough for their growth with
// file size to show up.
fn bench_large_file(c: &mut Criterion) {
    let source = large_table(500);
    let mut parser = AlParser::new();
    let tree = parser.parse(&source).tree;

    c.bench_function("parser/parse_large_table_500_fields", |b| {
        b.iter(|| {
            let r = parser.parse(black_box(&source));
            black_box(r);
        });
    });

    c.bench_function("symbols/document_symbols_large_table_500_fields", |b| {
        b.iter(|| {
            let r = extract_document_symbols(black_box(&tree), black_box(&source));
            black_box(r);
        });
    });

    c.bench_function("tokens/semantic_tokens_large_table_500_fields", |b| {
        b.iter(|| {
            let r = extract_semantic_tokens(black_box(&tree), black_box(&source));
            black_box(r);
        });
    });

    c.bench_function("lint/lint_large_table_500_fields", |b| {
        b.iter(|| {
            let r = lint(black_box(&tree), black_box(&source));
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

criterion_group!(
    benches,
    bench_parse,
    bench_parse_incremental,
    bench_format,
    bench_large_file
);
criterion_main!(benches);
