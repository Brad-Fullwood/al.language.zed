//! Synthetic snapshot archives for tests, built at run time so no binary
//! fixture is committed. Compiled for unit tests and under the `test-support`
//! feature, which `al-lsp` enables in its dev-dependencies only.

use std::io::{Cursor, Write as _};

// ---------------------------------------------------------------------------
// Test only FlatBuffers builder. Like the real builder it fills the buffer from
// the back, so children are written before their parents and every offset
// points forward. A position is the number of bytes written so far ("used").
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct B {
    rev: Vec<u8>,
}

pub enum F {
    I32(i32),
    I8(i8),
    Bool(bool),
    I64(i64),
    Raw(Vec<u8>),
    Off(usize),
}

impl B {
    pub fn push(&mut self, chunk: &[u8]) -> usize {
        self.rev.extend(chunk.iter().rev());
        self.rev.len()
    }

    pub fn string(&mut self, s: &str) -> usize {
        let mut c = (s.len() as u32).to_le_bytes().to_vec();
        c.extend_from_slice(s.as_bytes());
        c.push(0);
        self.push(&c)
    }

    pub fn vector(&mut self, elems: &[usize]) -> usize {
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

    pub fn table(&mut self, fields: &[(usize, F)]) -> usize {
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

    pub fn finish(mut self, root: usize) -> Vec<u8> {
        let used_after = self.rev.len() + 4;
        self.push(&((used_after - root) as u32).to_le_bytes());
        self.rev.reverse();
        self.rev
    }
}

pub fn span(from: (u16, u16), to: (u16, u16)) -> F {
    // On disk: To first, then From.
    let mut r = Vec::new();
    for v in [to.0, to.1, from.0, from.1] {
        r.extend_from_slice(&v.to_le_bytes());
    }
    F::Raw(r)
}

pub fn var(b: &mut B, name: &str, value: &str, ty: &str, children: &[usize]) -> usize {
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

pub fn obj_id(b: &mut B, ty: i8, number: i32) -> usize {
    b.table(&[(0, F::I32(number)), (1, F::I8(ty))])
}

/// Build a record variable shaped like the server's: Fields, Filters, Keys.
pub fn record(b: &mut B) -> usize {
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
pub type FrameSpec = (&'static str, (i8, i32), &'static str, (u16, u16));

pub struct Ctx {
    pub id: (i8, i32),
    pub method: &'static str,
    pub frames: Vec<FrameSpec>,
    pub lines: Vec<LineSpec>,
    pub exception: Option<(&'static str, i32, (u16, u16))>,
}

pub struct LineSpec {
    pub ordinal: i32,
    pub line: u16,
    pub vars: bool,
}

pub fn build_mdc(c: &Ctx) -> Vec<u8> {
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

pub fn plain_ctx() -> Ctx {
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

pub fn version_bytes(major: i32) -> Vec<u8> {
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
pub fn real_version_bytes() -> Vec<u8> {
    let mut real = vec![0u8; 56];
    real.extend_from_slice(&[4, 0, 4, 0, 4, 0, 0, 0]);
    real
}

pub fn make_zip(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
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
// Zip
// ---------------------------------------------------------------------------

pub fn synthetic_zip() -> Vec<u8> {
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
