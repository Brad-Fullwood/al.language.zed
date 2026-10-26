//! Tests for the snapshot archive reader. All data here is synthetic.

use super::*;
use std::io::Write as _;

// ---------------------------------------------------------------------------
// Test only FlatBuffers builder. Like the real builder it fills the buffer from
// the back, so children are written before their parents and every offset
// points forward. A position is the number of bytes written so far ("used").
// ---------------------------------------------------------------------------

#[derive(Default)]
struct B {
    rev: Vec<u8>,
}

enum F {
    I32(i32),
    I8(i8),
    Bool(bool),
    I64(i64),
    Raw(Vec<u8>),
    Off(usize),
}

impl B {
    fn push(&mut self, chunk: &[u8]) -> usize {
        self.rev.extend(chunk.iter().rev());
        self.rev.len()
    }

    fn string(&mut self, s: &str) -> usize {
        let mut c = (s.len() as u32).to_le_bytes().to_vec();
        c.extend_from_slice(s.as_bytes());
        c.push(0);
        self.push(&c)
    }

    fn vector(&mut self, elems: &[usize]) -> usize {
        let start = self.rev.len();
        let total = 4 + 4 * elems.len();
        let used_after = start + total;
        let mut c = (elems.len() as u32).to_le_bytes().to_vec();
        for (i, e) in elems.iter().enumerate() {
            let field_used = used_after - (4 + 4 * i);
            c.extend_from_slice(&((field_used - e) as u32).to_le_bytes());
        }
        self.push(&c)
    }

    fn table(&mut self, fields: &[(usize, F)]) -> usize {
        let nslots = fields.iter().map(|(s, _)| s + 1).max().unwrap_or(0);
        let mut body = vec![0u8; 4];
        let mut offs = vec![0u16; nslots];
        let mut offpatch: Vec<(usize, usize)> = Vec::new();
        for (slot, f) in fields {
            offs[*slot] = body.len() as u16;
            match f {
                F::I32(v) => body.extend_from_slice(&v.to_le_bytes()),
                F::I8(v) => body.push(*v as u8),
                F::Bool(v) => body.push(u8::from(*v)),
                F::I64(v) => body.extend_from_slice(&v.to_le_bytes()),
                F::Raw(r) => body.extend_from_slice(r),
                F::Off(t) => {
                    offpatch.push((body.len(), *t));
                    body.extend_from_slice(&[0; 4]);
                }
            }
        }
        let vt_len = 4 + 2 * nslots;
        body[0..4].copy_from_slice(&(vt_len as i32).to_le_bytes());
        let used_t = self.rev.len() + body.len();
        for (o, t) in offpatch {
            let field_used = used_t - o;
            body[o..o + 4].copy_from_slice(&((field_used - t) as u32).to_le_bytes());
        }
        self.push(&body);
        let mut vt = (vt_len as u16).to_le_bytes().to_vec();
        vt.extend_from_slice(&(body.len() as u16).to_le_bytes());
        for o in offs {
            vt.extend_from_slice(&o.to_le_bytes());
        }
        self.push(&vt);
        used_t
    }

    fn finish(mut self, root: usize) -> Vec<u8> {
        let used_after = self.rev.len() + 4;
        self.push(&((used_after - root) as u32).to_le_bytes());
        self.rev.reverse();
        self.rev
    }
}

fn span(from: (u16, u16), to: (u16, u16)) -> F {
    // On disk: To first, then From.
    let mut r = Vec::new();
    for v in [to.0, to.1, from.0, from.1] {
        r.extend_from_slice(&v.to_le_bytes());
    }
    F::Raw(r)
}

fn var(b: &mut B, name: &str, value: &str, ty: &str, children: &[usize]) -> usize {
    let mut f = vec![(1, F::Bool(!children.is_empty()))];
    if !name.is_empty() {
        f.push((2, F::Off(b.string(name))));
    }
    if !value.is_empty() {
        f.push((3, F::Off(b.string(value))));
    }
    if !ty.is_empty() {
        f.push((4, F::Off(b.string(ty))));
    }
    if !children.is_empty() {
        f.push((0, F::Off(b.vector(children))));
    }
    b.table(&f)
}

fn obj_id(b: &mut B, ty: i8, number: i32) -> usize {
    b.table(&[(0, F::I32(number)), (1, F::I8(ty))])
}

/// Build a record variable shaped like the server's: Fields, Filters, Keys.
fn record(b: &mut B) -> usize {
    let f1 = var(b, "No.", "'SO1'", "Code[20]", &[]);
    let f2 = var(b, "Name", "'Stark'", "Text[100]", &[]);
    let f3 = var(b, "Amount", "12.5", "Decimal", &[]);
    let fields = var(b, "Fields", "Count = 3", "", &[f1, f2, f3]);
    let filters = var(b, "Filters", "No.: SO1", "Text", &[]);
    let k1 = var(b, "No.", "Ascending", "", &[]);
    let keys = var(b, "Keys", "No.", "", &[k1]);
    var(
        b,
        "Rec",
        "'SO1'",
        "Table Sales Header (36)",
        &[fields, filters, keys],
    )
}

/// Method, object (type, number), object name, statement (line, column).
type FrameSpec = (&'static str, (i8, i32), &'static str, (u16, u16));

struct Ctx {
    id: (i8, i32),
    method: &'static str,
    frames: Vec<FrameSpec>,
    lines: Vec<LineSpec>,
    exception: Option<(&'static str, i32, (u16, u16))>,
}

struct LineSpec {
    ordinal: i32,
    line: u16,
    vars: bool,
}

fn build_mdc(c: &Ctx) -> Vec<u8> {
    let mut b = B::default();
    let mut lc_offs = Vec::new();
    for l in &c.lines {
        let mut f = vec![
            (0, F::I32(l.ordinal)),
            (1, span((l.line, 8), (l.line, 52))),
            (3, F::I64(63_928_635_026_723_638)),
        ];
        if l.vars {
            let rec = record(&mut b);
            let g = var(&mut b, "<Globals>", "", "", &[rec]);
            let loc = var(&mut b, "Cust", "'C1'", "Code[20]", &[]);
            f.push((2, F::Off(b.vector(&[g, loc]))));
        }
        lc_offs.push(b.table(&f));
    }
    let mut fr_offs = Vec::new();
    for (m, id, obj, at) in &c.frames {
        let s = b.string(m);
        let i = obj_id(&mut b, id.0, id.1);
        let o = b.string(obj);
        fr_offs.push(b.table(&[
            (0, F::Off(s)),
            (1, F::Off(i)),
            (2, F::Off(o)),
            (3, span((at.0, at.1), (at.0, at.1 + 10))),
        ]));
    }
    let mut f: Vec<(usize, F)> = Vec::new();
    let id = obj_id(&mut b, c.id.0, c.id.1);
    f.push((0, F::Off(id)));
    let m = b.string(c.method);
    f.push((1, F::Off(m)));
    if let Some((msg, ln, at)) = c.exception {
        let ms = b.string(msg);
        let e = b.table(&[
            (0, F::Off(ms)),
            (1, F::I32(ln)),
            (2, span((at.0, at.1), (at.0, at.1 + 4))),
        ]);
        f.push((2, F::Off(e)));
    }
    if !fr_offs.is_empty() {
        f.push((3, F::Off(b.vector(&fr_offs))));
    }
    if !lc_offs.is_empty() {
        f.push((4, F::Off(b.vector(&lc_offs))));
    }
    let mut ts = 100i64.to_le_bytes().to_vec();
    ts.extend_from_slice(&250i64.to_le_bytes());
    f.push((5, F::Raw(ts)));
    let an = b.string("Base");
    let ap = b.string("Pub");
    let av = b.string("28.0.1.2");
    let ai = b.string("0123456789abcdef0123456789abcdef");
    let app = b.table(&[
        (0, F::Off(an)),
        (1, F::Off(ap)),
        (2, F::Off(av)),
        (3, F::Off(ai)),
    ]);
    f.push((6, F::Off(app)));
    f.push((7, F::I8(1)));
    let root = b.table(&f);
    b.finish(root)
}

fn plain_ctx() -> Ctx {
    Ctx {
        id: (8, 42),
        method: "OnAfterGetRecord",
        frames: vec![
            ("OnAfterGetRecord", (8, 42), "Sales Order", (2568, 8)),
            ("Run", (5, 80), "Sales-Post", (10, 4)),
        ],
        lines: vec![
            LineSpec {
                ordinal: 0,
                line: 2568,
                vars: true,
            },
            LineSpec {
                ordinal: 1,
                line: 2569,
                vars: false,
            },
        ],
        exception: None,
    }
}

fn version_bytes(major: i32) -> Vec<u8> {
    let mut b = B::default();
    let t = b.table(&[
        (0, F::I32(major)),
        (1, F::I32(1)),
        (2, F::I32(2)),
        (3, F::I32(3)),
    ]);
    b.finish(t)
}

/// The 64 bytes found in real files: a zero root offset, zeros, and an empty
/// table (vtable at 56, table at 60).
fn real_version_bytes() -> Vec<u8> {
    let mut real = vec![0u8; 56];
    real.extend_from_slice(&[4, 0, 4, 0, 4, 0, 0, 0]);
    real
}

fn make_zip(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, data) in entries {
        w.start_file(*name, opts).unwrap();
        w.write_all(data).unwrap();
    }
    w.finish().unwrap().into_inner()
}

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

#[test]
fn decodes_every_table_and_struct() {
    let mut c = plain_ctx();
    c.exception = Some(("Boom", 1, (2569, 8)));
    let m = decode_method_context(&build_mdc(&c), 7).unwrap();
    assert_eq!(m.index, 7);
    assert_eq!(m.id.object_type, ObjectType::Page);
    assert_eq!(m.id.number, 42);
    assert_eq!(m.method_name, "OnAfterGetRecord");
    assert_eq!(m.frames.len(), 2);
    assert_eq!(m.frames[1].method_name, "Run");
    assert_eq!(m.frames[1].id.object_type, ObjectType::CodeUnit);
    assert_eq!(m.frames[1].object_name, "Sales-Post");
    assert_eq!(m.frames[0].statement_span.from.line, 2568);
    assert_eq!(m.frames[0].statement_span.from.column, 8);
    assert_eq!(m.frames[0].statement_span.to.column, 18);
    assert_eq!(m.line_contexts.len(), 2);
    let lc = &m.line_contexts[0];
    assert_eq!(lc.ordinal, 0);
    assert_eq!(lc.timestamp, 63_928_635_026_723_638);
    assert_eq!(
        lc.statement_span.from,
        SourcePosition {
            line: 2568,
            column: 8
        }
    );
    assert_eq!(
        lc.statement_span.to,
        SourcePosition {
            line: 2568,
            column: 52
        }
    );
    assert_eq!(lc.statement_span.first_line(), 2569);
    assert_eq!(lc.variables.len(), 2);
    assert!(m.line_contexts[1].variables.is_empty());
    assert_eq!(
        m.execution_span,
        TimeSpan {
            start_time: 100,
            end_time: 250
        }
    );
    let app = m.app.unwrap();
    assert_eq!(app.app_name, "Base");
    assert_eq!(app.app_publisher, "Pub");
    assert_eq!(app.app_version, "28.0.1.2");
    assert_eq!(app.app_id.len(), 32);
    assert!(m.is_builtin_code_unit_call);
    assert!(!m.is_incomplete_measurement);
    let e = m.exception.unwrap();
    assert_eq!(e.message, "Boom");
    assert_eq!(e.line_number, 1);
    assert_eq!(e.statement_span.from.line, 2569);
}

#[test]
fn nested_variables_decode_as_a_tree() {
    let m = decode_method_context(&build_mdc(&plain_ctx()), 0).unwrap();
    let globals = &m.line_contexts[0].variables[0];
    assert_eq!(globals.name, "<Globals>");
    assert!(globals.has_children);
    let rec = &globals.children[0];
    assert_eq!(rec.name, "Rec");
    assert_eq!(rec.type_name, "Table Sales Header (36)");
    assert_eq!(rec.children.len(), 3);
    assert_eq!(rec.children[0].name, "Fields");
    assert_eq!(rec.children[0].children[2].value, "12.5");
    let local = &m.line_contexts[0].variables[1];
    assert!(!local.has_children && local.children.is_empty());
}

#[test]
fn absent_fields_give_defaults() {
    let mut b = B::default();
    let t = b.table(&[]);
    let m = decode_method_context(&b.finish(t), 3).unwrap();
    assert_eq!(m.index, 3);
    assert_eq!(m.id, ObjectId::default());
    assert_eq!(m.method_name, "");
    assert!(m.exception.is_none() && m.app.is_none());
    assert!(m.frames.is_empty() && m.line_contexts.is_empty());
    assert_eq!(m.execution_span, TimeSpan::default());
    assert!(!m.is_builtin_code_unit_call);

    let mut b = B::default();
    let v = var(&mut b, "", "", "", &[]);
    let vv = b.vector(&[v]);
    let lc = b.table(&[(2, F::Off(vv))]);
    let vec = b.vector(&[lc]);
    let root = b.table(&[(4, F::Off(vec))]);
    let m = decode_method_context(&b.finish(root), 0).unwrap();
    let v = &m.line_contexts[0].variables[0];
    assert_eq!(
        (v.name.as_str(), v.value.as_str(), v.type_name.as_str()),
        ("", "", "")
    );
    assert_eq!(m.line_contexts[0].statement_span, SourceSpan::default());
}

#[test]
fn unknown_object_type_is_kept() {
    let mut b = B::default();
    let id = obj_id(&mut b, 99, 5);
    let root = b.table(&[(0, F::Off(id))]);
    let m = decode_method_context(&b.finish(root), 0).unwrap();
    assert_eq!(m.id.object_type, ObjectType::Other(99));
    assert_eq!(m.id.object_type.name(), "Unknown99");
}

#[test]
fn object_type_names() {
    assert_eq!(
        ObjectType::from_name("codeunit"),
        Some(ObjectType::CodeUnit)
    );
    assert_eq!(
        ObjectType::from_name("PageExtension"),
        Some(ObjectType::PageExtension)
    );
    assert_eq!(ObjectType::from_name("nope"), None);
    assert_eq!(ObjectType::CodeUnit.display_name(), "Codeunit");
    for (n, t) in ObjectType::NAMED {
        assert_eq!(ObjectType::from_i8(n), t);
        assert_eq!(t.as_i8(), n);
    }
}

#[test]
fn variable_depth_cap() {
    let chain = |depth: usize| {
        let mut b = B::default();
        let mut cur = var(&mut b, "leaf", "", "", &[]);
        for _ in 1..depth {
            cur = var(&mut b, "n", "", "", &[cur]);
        }
        let vecs = b.vector(&[cur]);
        let lc = b.table(&[(2, F::Off(vecs))]);
        let lcs = b.vector(&[lc]);
        let root = b.table(&[(4, F::Off(lcs))]);
        b.finish(root)
    };
    assert!(decode_method_context(&chain(MAX_VARIABLE_DEPTH), 0).is_ok());
    let err = decode_method_context(&chain(MAX_VARIABLE_DEPTH + 6), 0).unwrap_err();
    assert!(err.to_string().contains("deeper"), "{err}");
}

#[test]
fn every_truncation_is_an_error_not_a_panic() {
    let mut c = plain_ctx();
    c.exception = Some(("Boom", 1, (2569, 8)));
    let full = build_mdc(&c);
    assert!(decode_method_context(&full, 0).is_ok());
    for n in 0..full.len() {
        let r = decode_method_context(&full[..n], 0);
        // The final NUL of the last string is never read.
        if n + 1 < full.len() {
            assert!(
                r.is_err(),
                "truncation to {n} of {} bytes decoded",
                full.len()
            );
        }
    }
}

#[test]
fn corrupt_bytes_never_panic() {
    let full = build_mdc(&plain_ctx());
    for i in 0..full.len() {
        for fill in [0x00u8, 0xFF, 0x7F, 0x80] {
            let mut d = full.clone();
            d[i] = fill;
            let _ = decode_method_context(&d, 0);
        }
    }
    // Self referencing offsets: root points at itself.
    let _ = decode_method_context(&[0, 0, 0, 0, 0, 0, 0, 0], 0);
    let _ = decode_method_context(&[4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 0);
    let _ = decode_method_context(&[0xFF; 64], 0);
}

#[test]
fn shared_subtrees_hit_the_node_budget() {
    // A variable whose children vector lists the same child 1000 times, nested
    // 4 deep, would expand to 10^12 nodes.
    let mut b = B::default();
    let mut cur = var(&mut b, "leaf", "", "", &[]);
    for _ in 0..4 {
        cur = var(&mut b, "n", "", "", &vec![cur; 1000]);
    }
    let vecs = b.vector(&[cur]);
    let lc = b.table(&[(2, F::Off(vecs))]);
    let lcs = b.vector(&[lc]);
    let root = b.table(&[(4, F::Off(lcs))]);
    let err = decode_method_context(&b.finish(root), 0).unwrap_err();
    assert!(
        err.to_string().contains("tables in one method context"),
        "{err}"
    );
}

// ---------------------------------------------------------------------------
// Version
// ---------------------------------------------------------------------------

#[test]
fn real_version_bytes_read_as_3_0_0_0() {
    let v = decode_version(&real_version_bytes()).unwrap();
    assert_eq!(v.to_string(), "3.0.0.0");
    assert_eq!(real_version_bytes().len(), 64);
    // The reading through the trailing empty table gives the same.
    let bytes = real_version_bytes();
    let tab = Tab::at(&bytes, 60, false).unwrap();
    assert_eq!(version_fields(&tab).unwrap().to_string(), "3.0.0.0");
}

#[test]
fn proper_version_table_is_read() {
    assert_eq!(
        decode_version(&version_bytes(3)).unwrap().to_string(),
        "3.1.2.3"
    );
    assert_eq!(decode_version(&version_bytes(4)).unwrap().major, 4);
}

#[test]
fn version_guard() {
    let ok = |major: i32| check_version(decode_version(&version_bytes(major)).unwrap());
    assert!(ok(3).is_ok());
    let err = ok(4).unwrap_err().to_string();
    assert!(
        err.contains("4.1.2.3") && err.contains("supports format 3.x"),
        "{err}"
    );
    assert!(decode_version(&[]).is_err());
    assert!(decode_version(&[1, 2]).is_err());
    assert!(decode_version(&[200, 0, 0, 0, 0, 0, 0, 0]).is_err());
}

// ---------------------------------------------------------------------------
// Zip
// ---------------------------------------------------------------------------

fn sample_zip() -> Vec<u8> {
    let first = plain_ctx();
    let mut second = plain_ctx();
    second.method = "OnAfterGetRecord";
    second.lines = vec![
        LineSpec {
            ordinal: 0,
            line: 2568,
            vars: false,
        },
        LineSpec {
            ordinal: 1,
            line: 2569,
            vars: false,
        },
    ];
    second.exception = Some(("Item is missing", 1, (2569, 8)));
    let other = Ctx {
        id: (5, 80),
        method: "Run",
        frames: vec![("Run", (5, 80), "Sales-Post", (10, 4))],
        lines: vec![],
        exception: None,
    };
    let src = "line one\r\nline two\r\nline three\r\n";
    make_zip(&[
        ("version", real_version_bytes()),
        // Written out of order on purpose: 10 must sort after 9 numerically.
        ("10.mdc", build_mdc(&second)),
        ("9.mdc", build_mdc(&other)),
        ("2.mdc", build_mdc(&first)),
        ("Page%42.al", src.as_bytes().to_vec()),
        ("notes.txt", b"ignored".to_vec()),
        ("x.alcpuprofile", b"{}".to_vec()),
    ])
}

#[test]
fn zip_end_to_end() {
    let a = SnapshotArchive::from_bytes(&sample_zip()).unwrap();
    assert_eq!(a.version().to_string(), "3.0.0.0");
    let idx: Vec<usize> = a.method_contexts().iter().map(|m| m.index).collect();
    assert_eq!(idx, [2, 9, 10]);
    assert_eq!(a.source(ObjectType::Page, 42).unwrap().lines().count(), 3);
    assert_eq!(a.source_line(ObjectType::Page, 42, 2), Some("line two"));
    assert_eq!(a.source_line(ObjectType::Page, 42, 0), None);
    assert!(a.source(ObjectType::Page, 43).is_none());
    assert_eq!(a.source_ids(), vec![(ObjectType::Page, 42)]);

    let stops = a.stops();
    assert_eq!(stops.len(), 2);
    let s = &stops[0];
    assert_eq!(s.kind, StopKind::Snappoint);
    assert_eq!((s.context_index, s.line_context_index), (2, 0));
    assert_eq!(s.line(), 2569);
    assert_eq!(s.frames.len(), 2);
    assert_eq!(s.frames[0].method_name, "OnAfterGetRecord");
    assert_eq!(s.frames[0].object_name, "Sales Order");
    assert_eq!(s.frames[0].source_file_name(), "Page%42.al");
    // The caller line comes from its own span: line 10 (0-based) is 11.
    assert_eq!(
        (s.frames[1].method_name.as_str(), s.frames[1].line),
        ("Run", 11)
    );
    assert_eq!(s.globals.len(), 1);
    assert_eq!(s.globals[0].name, "Rec");
    assert_eq!(s.locals.len(), 1);
    assert_eq!(s.locals[0].name, "Cust");

    let e = &stops[1];
    assert_eq!(e.kind, StopKind::Error);
    assert_eq!(e.error.as_deref(), Some("Item is missing"));
    assert_eq!(e.line(), 2570);
    assert!(e.locals.is_empty() && e.globals.is_empty());

    let text = render_stops(&stops, &RenderOptions::default());
    assert_eq!(
        text,
        "== Stop 1: Page 42 \"Sales Order\" line 2569 (OnAfterGetRecord)\n\
         Call stack:\n\
         \x20 Page 42 \"Sales Order\" OnAfterGetRecord line 2569\n\
         \x20 Codeunit 80 \"Sales-Post\" Run line 11\n\
         Locals:\n\
         \x20 Cust = 'C1'\n\
         Globals:\n\
         \x20 Rec (Table Sales Header (36)): No. = 'SO1', filters: No.: SO1\n\
         \n\
         == Error capture 2: Page 42 \"Sales Order\" line 2570 (OnAfterGetRecord)\n\
         Call stack:\n\
         \x20 Page 42 \"Sales Order\" OnAfterGetRecord line 2570\n\
         \x20 Codeunit 80 \"Sales-Post\" Run line 11\n\
         Locals:\n\
         \x20 ERROR: Item is missing\n\
         \n"
    );

    let opts = RenderOptions {
        vars: Some(vec!["rec".into()]),
        fields: vec!["name".into(), "AMOUNT".into()],
        only_stop: Some(1),
    };
    let text = render_stops(&stops, &opts);
    assert!(!text.contains("Locals:") && !text.contains("Error capture"));
    assert!(text.contains(
        "Rec (Table Sales Header (36)): No. = 'SO1', Name = 'Stark', Amount = 12.5, filters: No.: SO1"
    ));
    assert!(render_stops(
        &stops,
        &RenderOptions {
            only_stop: Some(9),
            ..Default::default()
        }
    )
    .contains("no stop 9"));
    assert!(render_stops(&[], &RenderOptions::default()).starts_with("no snappoint hits"));

    let json = serde_json::to_value(&stops[0]).unwrap();
    assert_eq!(json["frames"][0]["object"]["objectType"], "Page");
    assert_eq!(json["globals"][0]["children"][0]["type"], "");
    let ctx_json = serde_json::to_value(&a.method_contexts()[0]).unwrap();
    assert_eq!(ctx_json["methodName"], "OnAfterGetRecord");
    assert_eq!(
        ctx_json["lineContexts"][0]["statementSpan"]["from"]["line"],
        2568
    );
}

#[test]
fn scope_split_without_globals_node() {
    let v = |n: &str| Variable {
        name: n.into(),
        ..Default::default()
    };
    let (l, g) = split_scopes(&[v("A"), v("B")]);
    assert_eq!((l.len(), g.len()), (2, 0));
    let (l, g) = split_scopes(&[
        Variable {
            name: "<globals>".into(),
            children: vec![v("G1")],
            ..Default::default()
        },
        v("L1"),
    ]);
    assert_eq!((l[0].name.as_str(), g[0].name.as_str()), ("L1", "G1"));
}

#[test]
fn missing_or_wrong_version_is_refused() {
    let mdc = build_mdc(&plain_ctx());
    let none = make_zip(&[("0.mdc", mdc.clone())]);
    let err = SnapshotArchive::from_bytes(&none).unwrap_err().to_string();
    assert!(
        err.contains("no version entry") && err.contains("3.x"),
        "{err}"
    );

    let v4 = make_zip(&[("version", version_bytes(4)), ("0.mdc", mdc)]);
    let err = SnapshotArchive::from_bytes(&v4).unwrap_err().to_string();
    assert!(
        err.contains("4.1.2.3") && err.contains("supports format 3.x"),
        "{err}"
    );
}

#[test]
fn corrupt_mdc_in_zip_names_the_entry() {
    let z = make_zip(&[("version", real_version_bytes()), ("5.mdc", vec![9, 9, 9])]);
    let err = SnapshotArchive::from_bytes(&z).unwrap_err().to_string();
    assert!(err.contains("5.mdc"), "{err}");
    // A zero length entry is skipped.
    let z = make_zip(&[("version", real_version_bytes()), ("5.mdc", vec![])]);
    assert!(SnapshotArchive::from_bytes(&z)
        .unwrap()
        .method_contexts()
        .is_empty());
}

#[test]
fn not_a_zip_is_an_error() {
    assert!(matches!(
        SnapshotArchive::from_bytes(b"hello"),
        Err(ArchiveError::Zip(_))
    ));
}

#[test]
fn zip_caps() {
    let z = make_zip(&[
        ("version", real_version_bytes()),
        ("Page%1.al", vec![b'x'; 5000]),
    ]);
    let with = |entries, entry_bytes, total_bytes| {
        SnapshotArchive::from_reader_with(
            Cursor::new(&z),
            Limits {
                entries,
                entry_bytes,
                total_bytes,
            },
        )
    };
    assert!(with(10, 6000, 10_000).is_ok());
    // One entry too big (declared and inflated size).
    assert!(matches!(
        with(10, 4000, 10_000),
        Err(ArchiveError::Limit(_))
    ));
    // Total too big although each entry fits.
    assert!(matches!(with(10, 6000, 5000), Err(ArchiveError::Limit(_))));
    // Too many entries.
    assert!(matches!(with(1, 6000, 10_000), Err(ArchiveError::Limit(_))));
    // Entries we do not read do not count against the size caps.
    let z2 = make_zip(&[
        ("version", real_version_bytes()),
        ("junk.bin", vec![0u8; 100_000]),
    ]);
    let l = Limits {
        entries: 10,
        entry_bytes: 1000,
        total_bytes: 1000,
    };
    assert!(SnapshotArchive::from_reader_with(Cursor::new(&z2), l).is_ok());
}

#[test]
fn entry_names_are_classified() {
    assert!(matches!(classify("12.mdc"), Some(EntryKind::Mdc(12))));
    assert!(classify("x.mdc").is_none());
    assert!(classify("-1.mdc").is_none());
    assert!(matches!(
        classify("dir/CodeUnit%80.al"),
        Some(EntryKind::Source(ObjectType::CodeUnit, 80))
    ));
    assert!(classify("Bogus%1.al").is_none());
    assert!(classify("Page%x.al").is_none());
    assert!(matches!(classify("version"), Some(EntryKind::Version)));
}

// ---------------------------------------------------------------------------
// Real data. Run with AL_SNAPSHOT_FIXTURE_DIR set and --include-ignored.
// ---------------------------------------------------------------------------

fn fixture_dir() -> std::path::PathBuf {
    std::env::var_os("AL_SNAPSHOT_FIXTURE_DIR")
        .expect("AL_SNAPSHOT_FIXTURE_DIR is not set")
        .into()
}

fn golden(id: &str) -> serde_json::Value {
    let dir = fixture_dir();
    let name = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| n.starts_with(&format!("golden-{id}")))
        .unwrap();
    serde_json::from_slice(&std::fs::read(dir.join(name)).unwrap()).unwrap()
}

fn open_fixture(id: &str) -> SnapshotArchive {
    let dir = fixture_dir();
    let name = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| n.starts_with(id) && n.ends_with(".zip"))
        .unwrap();
    SnapshotArchive::open(&dir.join(name)).unwrap()
}

/// Compare to the host's variable dump, which stops expanding after 3 levels
/// of children. A missing `children` key in the golden file means none.
fn compare_vars(
    path: &str,
    level: usize,
    ours: &[Variable],
    gold: &[serde_json::Value],
    diffs: &mut Vec<String>,
) {
    if ours.len() != gold.len() {
        diffs.push(format!(
            "{path}: {} variables vs host {}",
            ours.len(),
            gold.len()
        ));
        return;
    }
    for (o, g) in ours.iter().zip(gold) {
        let gs = |k: &str| g[k].as_str().unwrap_or("").to_string();
        let here = format!("{path}/{}", o.name);
        if o.name != gs("name") {
            diffs.push(format!("{here}: name vs host {:?}", gs("name")));
        }
        if o.value != gs("value") {
            diffs.push(format!(
                "{here}: value {:?} vs host {:?}",
                o.value,
                gs("value")
            ));
        }
        if o.type_name != gs("type") {
            diffs.push(format!(
                "{here}: type {:?} vs host {:?}",
                o.type_name,
                gs("type")
            ));
        }
        if level < 3 {
            let empty = Vec::new();
            let gc = g["children"].as_array().unwrap_or(&empty);
            compare_vars(&here, level + 1, &o.children, gc, diffs);
        }
    }
}

#[test]
#[ignore = "needs AL_SNAPSHOT_FIXTURE_DIR with customer snapshot zips"]
fn golden_83a73940() {
    let a = open_fixture("83a73940");
    assert_eq!(a.version().to_string(), "3.0.0.0");
    let stops = a.stops();
    let g = golden("83a73940");
    let gstops = g["stops"].as_array().unwrap();
    assert_eq!(stops.len(), gstops.len());
    let mut diffs = Vec::new();
    for (i, (s, gs)) in stops.iter().zip(gstops).enumerate() {
        let gf = &gs["frames"][0];
        assert_eq!(s.frames[0].method_name, gf["name"].as_str().unwrap());
        assert_eq!(
            i64::from(s.line()),
            gf["line"].as_i64().unwrap(),
            "stop {}",
            i + 1
        );
        assert_eq!(s.frames[0].object_name, gf["sourceName"].as_str().unwrap());
        assert_eq!(s.frames.len(), gs["frames"].as_array().unwrap().len());
        assert_eq!(
            format!("/tmp/snapshotfiles/{}", s.frames[0].source_file_name()),
            gf["path"].as_str().unwrap()
        );
        for (scope, ours) in [("Locals", &s.locals), ("Globals", &s.globals)] {
            let gv = gs["scopes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|x| x["name"] == scope)
                .unwrap()["variables"]
                .as_array()
                .unwrap();
            compare_vars(&format!("stop{}/{scope}", i + 1), 0, ours, gv, &mut diffs);
        }
    }
    assert_eq!(
        stops.iter().map(Stop::line).collect::<Vec<_>>(),
        [2569, 2570, 2569, 2570]
    );
    assert_eq!(stops[0].globals.len(), 59);
    assert_eq!(stops[0].locals.len(), 1);
    assert!(stops[0].globals.iter().any(|v| v.name == "Rec"));
    assert!(
        diffs.is_empty(),
        "{} differences, first: {:#?}",
        diffs.len(),
        &diffs[..diffs.len().min(10)]
    );

    // The renderer runs on real data.
    let text = render_stops(&stops, &RenderOptions::default());
    assert!(text.contains("== Stop 1: Page 42 \"Sales Order\" line 2569 (OnAfterGetRecord)"));
    assert!(text.contains("Document Type = Order"));
    // The golden source line matches our 1-based line.
    let line = a.source_line(ObjectType::Page, 42, 2569).unwrap();
    assert!(line.contains("WorkDescription"), "{line}");
}

#[test]
#[ignore = "needs AL_SNAPSHOT_FIXTURE_DIR with customer snapshot zips"]
fn golden_a0d62542() {
    let a = open_fixture("a0d62542");
    assert_eq!(a.version().to_string(), "3.0.0.0");
    assert!(a.stops().is_empty());
    assert!(!a.method_contexts().is_empty());
    let g = golden("a0d62542");
    // The host shows one placeholder stop here. That is replay adapter behaviour.
    assert_eq!(g["stops"].as_array().unwrap().len(), 1);
}
