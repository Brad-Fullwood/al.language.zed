//! A grammar-driven generator for AL source text, plus mutators for real fixture files.
//!
//! The generator emits source that the AL grammar parses without ERROR nodes, so a
//! property can assert "formatting a clean file keeps it clean" without first having to
//! decide whether the input was clean. Shapes it covers: objects (codeunit, table, page,
//! enum), procedures with parameters and return types, `var` sections, `if`/`case`/
//! `repeat`/`for`/`while`/`with`, record member access, quoted identifiers containing
//! spaces, dots, parentheses, doubled quotes and non-ASCII, line and block comments, and
//! preprocessor directives.

#![allow(dead_code)]

use proptest::prelude::*;

/// An identifier, either bare or quoted. The quoted forms are the ones that break
/// naive tokenisers.
pub fn ident() -> impl Strategy<Value = String> {
    prop_oneof![
        8 => prop::sample::select(vec![
            "Customer", "Item", "MyVar", "x", "Rec", "Total", "i", "Handled", "Amount2",
        ])
        .prop_map(String::from),
        1 => prop::sample::select(vec![
            "\"Item No.\"",
            "\"Amount (LCY)\"",
            "\"My \"\"Quoted\"\" Name\"",
            "\"Betrag für Küche\"",
            "\"日本語フィールド\"",
            "\"Name With  Spaces\"",
            "\"end\"",
            "\"begin\"",
        ])
        .prop_map(String::from),
    ]
}

fn literal() -> impl Strategy<Value = String> {
    prop_oneof![
        prop::sample::select(vec!["0", "1", "42", "-7", "1.5", "0.0"]).prop_map(String::from),
        prop::sample::select(vec!["true", "false"]).prop_map(String::from),
        prop::sample::select(vec![
            "''",
            "'hello'",
            "'it''s'",
            "'a // not a comment'",
            "'begin end'",
            "'/* not a comment */'",
            "'ünïcödé'",
        ])
        .prop_map(String::from),
    ]
}

fn expr() -> impl Strategy<Value = String> {
    let leaf = prop_oneof![
        literal(),
        ident(),
        (ident(), ident()).prop_map(|(a, b)| format!("{a}.{b}")),
        (ident(), ident()).prop_map(|(a, b)| format!("{a}.Get({b})")),
    ];
    leaf.prop_recursive(3, 24, 3, |inner| {
        prop_oneof![
            (
                inner.clone(),
                inner.clone(),
                prop::sample::select(vec![
                    "+", "-", "*", "/", "=", "<>", "<", ">=", "AND", "OR", "DIV", "MOD"
                ])
            )
                .prop_map(|(a, b, op)| format!("{a} {op} {b}")),
            inner.clone().prop_map(|a| format!("({a})")),
            (ident(), inner.clone(), inner.clone()).prop_map(|(f, a, b)| format!("{f}({a}, {b})")),
            inner.prop_map(|a| format!("NOT {a}")),
        ]
    })
}

fn comment() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        "// a line comment",
        "// comment with end; and begin",
        "/* block */",
        "/* block with\n   a second line */",
        "// ünïcödé cömmënt",
    ])
    .prop_map(String::from)
}

/// One statement, rendered without leading indentation. Lines are joined with `\n`.
pub fn stmt() -> impl Strategy<Value = String> {
    let leaf = prop_oneof![
        (ident(), expr()).prop_map(|(n, e)| format!("{n} := {e};")),
        (ident(), expr()).prop_map(|(n, e)| format!("{n}.Modify({e});")),
        ident().prop_map(|n| format!("{n}.Insert(true);")),
        ident().prop_map(|n| format!("{n}();")),
        expr().prop_map(|e| format!("exit({e});")),
        Just("exit;".to_string()),
        comment(),
        Just("begin\nend;".to_string()),
    ];
    leaf.prop_recursive(4, 40, 4, |inner| {
        prop_oneof![
            // if ... then <stmt>
            (expr(), inner.clone()).prop_map(|(c, s)| format!("if {c} then\n{}", indent(&body(s)))),
            // if ... then begin ... end;
            (expr(), block(inner.clone()))
                .prop_map(|(c, b)| { format!("if {c} then begin\n{b}\nend;") }),
            // if ... then <stmt> else <stmt>
            (expr(), inner.clone(), inner.clone()).prop_map(|(c, a, b)| {
                format!(
                    "if {c} then\n{}\nelse\n{}",
                    indent(&strip_semi(&body(a))),
                    indent(&body(b))
                )
            }),
            // if ... then begin ... end else begin ... end;
            (expr(), block(inner.clone()), block(inner.clone())).prop_map(|(c, a, b)| {
                format!("if {c} then begin\n{a}\nend\nelse begin\n{b}\nend;")
            }),
            // case
            (expr(), inner.clone(), inner.clone()).prop_map(|(c, a, b)| {
                format!(
                    "case {c} of\n1:\n{}\nelse\n{}\nend;",
                    indent(&body(a)),
                    indent(&body(b))
                )
            }),
            // repeat until
            (block(inner.clone()), expr()).prop_map(|(b, c)| format!("repeat\n{b}\nuntil {c};")),
            // for do begin
            (ident(), expr(), expr(), block(inner.clone())).prop_map(|(v, a, b, body)| {
                format!("for {v} := {a} to {b} do begin\n{body}\nend;")
            }),
            // while do
            (expr(), inner.clone())
                .prop_map(|(c, s)| format!("while {c} do\n{}", indent(&body(s)))),
            // with do begin
            (ident(), block(inner.clone()))
                .prop_map(|(r, b)| format!("with {r} do begin\n{b}\nend;")),
            // plain begin/end block as a statement
            block(inner).prop_map(|b| format!("begin\n{b}\nend;")),
        ]
    })
}

/// A statement usable where the grammar wants exactly one statement (a `then`, `else`,
/// `do` or case-arm body). A bare comment is not a statement there, so substitute one.
fn body(s: String) -> String {
    let t = s.trim_start();
    if t.starts_with("//") || t.starts_with("/*") {
        "exit;".to_string()
    } else {
        s
    }
}

/// AL forbids the `;` before `else`, so the `then` branch of an if/else drops it.
fn strip_semi(s: &str) -> String {
    s.trim_end().strip_suffix(';').unwrap_or(s).to_string()
}

fn block(
    inner: impl Strategy<Value = String> + Clone + 'static,
) -> impl Strategy<Value = String> + Clone {
    prop::collection::vec(inner, 1..3)
        .prop_map(|v| v.iter().map(|s| indent(s)).collect::<Vec<_>>().join("\n"))
        .boxed()
}

fn indent(s: &str) -> String {
    s.lines()
        .map(|l| {
            if l.is_empty() {
                String::new()
            } else {
                format!("    {l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn var_section() -> impl Strategy<Value = String> {
    prop::collection::vec(
        (
            ident(),
            prop::sample::select(vec![
                "Integer",
                "Decimal",
                "Text[50]",
                "Code[20]",
                "Boolean",
                "Record Customer",
                "Record \"Sales Line\"",
                "List of [Text]",
                "Dictionary of [Code[20], Integer]",
            ]),
        ),
        1..4,
    )
    .prop_map(|decls| {
        let body = decls
            .iter()
            .map(|(n, t)| format!("        {n}: {t};"))
            .collect::<Vec<_>>()
            .join("\n");
        format!("    var\n{body}")
    })
}

fn procedure() -> impl Strategy<Value = String> {
    (
        prop::option::of(prop::sample::select(vec![
            "    [Test]",
            "    [TryFunction]",
            "    [BusinessEvent(false)]",
        ])),
        ident(),
        prop::collection::vec(
            (
                ident(),
                prop::sample::select(vec!["Integer", "Text", "Code[20]"]),
            ),
            0..3,
        ),
        prop::option::of(prop::sample::select(vec!["Integer", "Boolean", "Text"])),
        prop::option::of(var_section()),
        prop::collection::vec(stmt(), 0..4),
    )
        .prop_map(|(attr, name, params, ret, vars, body)| {
            let params = params
                .iter()
                .map(|(n, t)| format!("{n}: {t}"))
                .collect::<Vec<_>>()
                .join("; ");
            let ret = ret.map(|r| format!(": {r}")).unwrap_or_default();
            let body = body
                .iter()
                .map(|s| {
                    s.lines()
                        .map(|l| {
                            if l.is_empty() {
                                String::new()
                            } else {
                                format!("        {l}")
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .collect::<Vec<_>>()
                .join("\n");
            let mut out = String::new();
            if let Some(a) = attr {
                out.push_str(a);
                out.push('\n');
            }
            out.push_str(&format!("    procedure {name}({params}){ret}\n"));
            if let Some(v) = vars {
                out.push_str(&v);
                out.push('\n');
            }
            out.push_str("    begin\n");
            if !body.is_empty() {
                out.push_str(&body);
                out.push('\n');
            }
            out.push_str("    end;");
            out
        })
}

fn preprocessor_wrap(body: String) -> impl Strategy<Value = String> {
    prop_oneof![
        3 => Just(body.clone()),
        1 => Just(format!("#if not CLEAN22\n{body}\n#endif")),
        1 => Just(format!("#pragma warning disable AA0005\n{body}\n#pragma warning restore AA0005")),
    ]
}

/// A whole AL file that the grammar accepts.
pub fn al_object() -> impl Strategy<Value = String> {
    let codeunit = (
        prop::sample::select(vec![50100u32, 50101, 70000]),
        ident(),
        prop::collection::vec(procedure(), 1..3),
    )
        .prop_map(|(id, name, procs)| {
            format!("codeunit {id} {name}\n{{\n{}\n}}", procs.join("\n\n"))
        });

    let table = (
        prop::sample::select(vec![50100u32, 50110]),
        ident(),
        prop::collection::vec((prop::sample::select(vec![1u32, 2, 10]), ident()), 1..3),
        prop::collection::vec(procedure(), 0..2),
    )
        .prop_map(|(id, name, fields, procs)| {
            let fields = fields
                .iter()
                .map(|(no, fname)| {
                    format!(
                        "        field({no}; {fname}; Code[20])\n        {{\n            Caption = '{}';\n        }}",
                        no
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            let procs = if procs.is_empty() {
                String::new()
            } else {
                format!("\n\n{}", procs.join("\n\n"))
            };
            format!("table {id} {name}\n{{\n    DataClassification = CustomerContent;\n\n    fields\n    {{\n{fields}\n    }}{procs}\n}}")
        });

    let page = (prop::sample::select(vec![50100u32, 50120]), ident(), ident()).prop_map(
        |(id, name, field)| {
            format!(
                "page {id} {name}\n{{\n    PageType = Card;\n    SourceTable = Customer;\n\n    layout\n    {{\n        area(content)\n        {{\n            group(General)\n            {{\n                field({field}; Rec.{field})\n                {{\n                    ApplicationArea = All;\n                }}\n            }}\n        }}\n    }}\n}}"
            )
        },
    );

    let enum_obj = (prop::sample::select(vec![50100u32]), ident()).prop_map(|(id, name)| {
        format!("enum {id} {name}\n{{\n    Extensible = true;\n\n    value(0; \" \")\n    {{\n        Caption = '';\n    }}\n    value(1; Open)\n    {{\n        Caption = 'Open';\n    }}\n}}")
    });

    prop_oneof![codeunit, table, page, enum_obj].prop_flat_map(preprocessor_wrap)
}

/// Mutate real source text: the fuzzing half of the generator. Returns the mutated text.
pub fn mutate(source: &str, ops: &[Mutation]) -> String {
    let mut lines: Vec<String> = source.lines().map(String::from).collect();
    for op in ops {
        if lines.is_empty() {
            break;
        }
        match *op {
            Mutation::DropLine(i) => {
                let i = i % lines.len();
                lines.remove(i);
            }
            Mutation::DuplicateLine(i) => {
                let i = i % lines.len();
                let l = lines[i].clone();
                lines.insert(i, l);
            }
            Mutation::SwapLines(i, j) => {
                let (i, j) = (i % lines.len(), j % lines.len());
                lines.swap(i, j);
            }
            Mutation::Reindent(i, n) => {
                let i = i % lines.len();
                let t = lines[i].trim_start().to_string();
                lines[i] = format!("{}{t}", " ".repeat(n % 17));
            }
            Mutation::TabIndent(i) => {
                let i = i % lines.len();
                let t = lines[i].trim_start().to_string();
                lines[i] = format!("\t{t}");
            }
            Mutation::BlankLine(i) => {
                let i = i % lines.len();
                lines.insert(i, String::new());
            }
            Mutation::TrailingSpace(i) => {
                let i = i % lines.len();
                lines[i].push_str("   ");
            }
            Mutation::JoinLines(i) => {
                let i = i % lines.len();
                if i + 1 < lines.len() {
                    let next = lines.remove(i + 1);
                    lines[i] = format!("{} {}", lines[i], next.trim_start());
                }
            }
        }
    }
    lines.join("\n")
}

#[derive(Debug, Clone, Copy)]
pub enum Mutation {
    DropLine(usize),
    DuplicateLine(usize),
    SwapLines(usize, usize),
    Reindent(usize, usize),
    TabIndent(usize),
    BlankLine(usize),
    TrailingSpace(usize),
    JoinLines(usize),
}

pub fn mutation() -> impl Strategy<Value = Mutation> {
    prop_oneof![
        (0usize..500).prop_map(Mutation::DropLine),
        (0usize..500).prop_map(Mutation::DuplicateLine),
        (0usize..500, 0usize..500).prop_map(|(a, b)| Mutation::SwapLines(a, b)),
        (0usize..500, 0usize..20).prop_map(|(a, b)| Mutation::Reindent(a, b)),
        (0usize..500).prop_map(Mutation::TabIndent),
        (0usize..500).prop_map(Mutation::BlankLine),
        (0usize..500).prop_map(Mutation::TrailingSpace),
        (0usize..500).prop_map(Mutation::JoinLines),
    ]
}

/// Every `.al` file shipped as test data, as (path, contents).
pub fn fixture_files() -> Vec<(String, String)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let mut out = Vec::new();
    collect_al(&root, &mut out);
    out.sort();
    out
}

fn collect_al(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path
                .file_name()
                .is_some_and(|n| n == "target" || n == ".git")
            {
                continue;
            }
            collect_al(&path, out);
        } else if path.extension().is_some_and(|e| e == "al") {
            if let Ok(text) = std::fs::read_to_string(&path) {
                out.push((path.display().to_string(), text));
            }
        }
    }
}
