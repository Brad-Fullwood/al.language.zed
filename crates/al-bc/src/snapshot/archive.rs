//! Reading a snapshot debugger zip: the recorded method contexts, the object
//! sources and the format version.
//!
//! A snapshot zip holds three kinds of entry:
//!
//! - `N.mdc`: one FlatBuffers `MethodContext` (one method invocation), read in
//!   numeric `N` order.
//! - `<Kind>%<id>.al`: the AL source of one object, `Kind` spelled as the
//!   `ObjectType` names (`CodeUnit`, `Page`, `Table` ...).
//! - `version`: a FlatBuffers `Version` table.
//!
//! # FlatBuffers reader
//!
//! The `flatbuffers` crate was checked and not used. Without `flatc` generated
//! code its table accessors are `unsafe`, its verifier needs generated types,
//! `flatc` is not installed here, and the schema is twelve small types. A
//! hand written reader that bounds checks every offset, length and vtable entry
//! is shorter than the glue it would replace. Malformed input returns
//! [`ArchiveError::Format`] and never panics. Variable nesting is capped at
//! [`MAX_VARIABLE_DEPTH`] and the number of decoded tables per buffer at
//! [`MAX_NODES`], which also stops shared sub-trees from expanding without limit.
//!
//! # Lines
//!
//! Spans in the file are 0-based. [`SourceSpan::first_line`] and
//! [`Stop::line`] give 1-based lines, the base editors and the debug adapter
//! protocol use.
//!
//! # Stops and scopes
//!
//! [`SnapshotArchive::stops`] returns the points a replay stops at, in recorded
//! order: method context `N` order, then line context order. A line context that
//! carries `Variables` is a snappoint stop. A method context with an
//! `Exception` gives an error stop at `LineContexts[Exception.LineNumber]`, or
//! at the exception's own statement span when that index is out of range.
//!
//! The variables of a stop split the way the host replay does it: when the
//! first variable is named `<Globals>` (case insensitive) its children are the
//! Globals scope and the remaining variables are the Locals scope. Otherwise
//! every variable is a local and Globals is empty. The host also reports a
//! scope "Database Statistics" (always empty in the samples, there is no field
//! for it in the format) and, for a stop without variables, a placeholder
//! variable `EmptyNodeMessage`. Both are replay adapter behaviour and are not
//! produced here. For an error stop the host adds a local holding the message,
//! this reader keeps it in [`Stop::error`] instead.

use std::collections::HashMap;
use std::fmt::{self, Write as _};
use std::io::{Cursor, Read, Seek};
use std::path::Path;

use serde::Serialize;
use thiserror::Error;

/// Highest variable nesting depth accepted. Real data nests about 6 levels.
pub const MAX_VARIABLE_DEPTH: usize = 64;
/// Most tables (variables, line contexts, frames) decoded from one `.mdc`.
pub const MAX_NODES: usize = 2_000_000;
/// Most elements accepted in one vector.
pub const MAX_VECTOR_LEN: usize = 1_000_000;
/// Most entries accepted in one zip.
pub const MAX_ENTRIES: usize = 200_000;
/// Largest uncompressed size accepted for one entry we read.
pub const MAX_ENTRY_BYTES: u64 = 64 * 1024 * 1024;
/// Largest total of uncompressed bytes read from one zip.
pub const MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;

const SUPPORTED_MAJOR: i32 = 3;

/// Errors from reading a snapshot archive.
#[derive(Debug, Error)]
pub enum ArchiveError {
    #[error("cannot read snapshot file: {0}")]
    Io(#[from] std::io::Error),
    #[error("snapshot zip is not readable: {0}")]
    Zip(String),
    #[error("snapshot zip exceeds a safety limit: {0}")]
    Limit(String),
    #[error("snapshot data is malformed: {0}")]
    Format(String),
    #[error("{0}")]
    Version(String),
}

fn bad<T>(msg: impl Into<String>) -> Result<T, ArchiveError> {
    Err(ArchiveError::Format(msg.into()))
}

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

/// AL object type as stored in `ApplicationObjectId.Type` and used for the
/// `<Kind>%<id>.al` entry names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjectType {
    TableData,
    Table,
    Form,
    Report,
    Dataport,
    CodeUnit,
    XmlPort,
    MenuSuite,
    Page,
    Query,
    System,
    FieldNumber,
    LimitedUsageTableData,
    TablePage,
    PageExtension,
    TableExtension,
    Enum,
    EnumExtension,
    ReportExtension,
    Interface,
    /// A value this reader does not know.
    Other(i8),
}

impl ObjectType {
    const NAMED: [(i8, ObjectType); 20] = [
        (0, ObjectType::TableData),
        (1, ObjectType::Table),
        (2, ObjectType::Form),
        (3, ObjectType::Report),
        (4, ObjectType::Dataport),
        (5, ObjectType::CodeUnit),
        (6, ObjectType::XmlPort),
        (7, ObjectType::MenuSuite),
        (8, ObjectType::Page),
        (9, ObjectType::Query),
        (10, ObjectType::System),
        (11, ObjectType::FieldNumber),
        (12, ObjectType::LimitedUsageTableData),
        (13, ObjectType::TablePage),
        (14, ObjectType::PageExtension),
        (15, ObjectType::TableExtension),
        (16, ObjectType::Enum),
        (17, ObjectType::EnumExtension),
        (22, ObjectType::ReportExtension),
        (28, ObjectType::Interface),
    ];

    /// Map the stored `int8` to a type.
    pub fn from_i8(v: i8) -> ObjectType {
        Self::NAMED
            .iter()
            .find(|(n, _)| *n == v)
            .map_or(ObjectType::Other(v), |(_, t)| *t)
    }

    /// The stored `int8`.
    pub fn as_i8(self) -> i8 {
        match self {
            ObjectType::Other(v) => v,
            t => Self::NAMED
                .iter()
                .find(|(_, n)| *n == t)
                .map_or(0, |(v, _)| *v),
        }
    }

    /// Name as spelled in the zip entry names (`CodeUnit`, `Page` ...).
    pub fn name(self) -> String {
        match self {
            ObjectType::Other(v) => format!("Unknown{v}"),
            t => format!("{t:?}"),
        }
    }

    /// Parse an entry name kind, ignoring case like the host's `Enum.TryParse`.
    pub fn from_name(s: &str) -> Option<ObjectType> {
        Self::NAMED
            .iter()
            .map(|(_, t)| *t)
            .find(|t| format!("{t:?}").eq_ignore_ascii_case(s))
    }

    /// Name for people: AL spells it `Codeunit`.
    pub fn display_name(self) -> String {
        match self {
            ObjectType::CodeUnit => "Codeunit".to_string(),
            t => t.name(),
        }
    }
}

impl Serialize for ObjectType {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.name())
    }
}

/// Object type and number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectId {
    pub object_type: ObjectType,
    pub number: i32,
}

impl Default for ObjectId {
    fn default() -> Self {
        ObjectId {
            object_type: ObjectType::TableData,
            number: 0,
        }
    }
}

/// A position in an `.al` file, 0-based.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcePosition {
    pub line: u16,
    pub column: u16,
}

/// A statement's extent in the `.al` file, 0-based, `to` exclusive.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSpan {
    pub from: SourcePosition,
    pub to: SourcePosition,
}

impl SourceSpan {
    /// 1-based line where the statement starts.
    pub fn first_line(&self) -> u32 {
        u32::from(self.from.line) + 1
    }

    /// 1-based line where the statement ends.
    pub fn last_line(&self) -> u32 {
        u32::from(self.to.line) + 1
    }
}

/// Wall time of a method call, microseconds since 0001-01-01 00:00:00 UTC.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimeSpan {
    pub start_time: i64,
    pub end_time: i64,
}

/// The extension that owns an object.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeclaringApplication {
    pub app_name: String,
    pub app_publisher: String,
    pub app_version: String,
    /// 32 hex digits, no dashes.
    pub app_id: String,
}

/// The error a method call ended in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExceptionContext {
    pub message: String,
    /// Index into the method's `line_contexts`.
    pub line_number: i32,
    pub statement_span: SourceSpan,
}

/// A node of the variable tree. `value` and `type_name` are display text
/// already formatted by the server, an empty string is stored as absent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Variable {
    pub name: String,
    pub value: String,
    #[serde(rename = "type")]
    pub type_name: String,
    pub has_children: bool,
    pub children: Vec<Variable>,
}

/// One executed statement of a method call.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LineContext {
    /// Statement ordinal inside the method, 0-based. Not a source line.
    pub ordinal: i32,
    pub statement_span: SourceSpan,
    /// Only present on snappoint statements.
    pub variables: Vec<Variable>,
    /// Microseconds since 0001-01-01 00:00:00 UTC.
    pub timestamp: i64,
}

/// One entry of a recorded call stack.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StackFrame {
    pub method_name: String,
    pub id: ObjectId,
    pub object_name: String,
    pub statement_span: SourceSpan,
}

/// One recorded method invocation (`N.mdc`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MethodContext {
    /// The `N` of the entry name.
    pub index: usize,
    pub id: ObjectId,
    pub method_name: String,
    pub exception: Option<ExceptionContext>,
    /// `frames[0]` is this method, then the callers, innermost first.
    pub frames: Vec<StackFrame>,
    pub line_contexts: Vec<LineContext>,
    pub execution_span: TimeSpan,
    pub app: Option<DeclaringApplication>,
    pub is_builtin_code_unit_call: bool,
    pub is_incomplete_measurement: bool,
}

/// Snapshot format version from the `version` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FormatVersion {
    pub major: i32,
    pub minor: i32,
    pub build: i32,
    pub revision: i32,
}

impl fmt::Display for FormatVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}.{}.{}.{}",
            self.major, self.minor, self.build, self.revision
        )
    }
}

// ---------------------------------------------------------------------------
// Bounds checked FlatBuffers reader
// ---------------------------------------------------------------------------

fn bytes_at<const N: usize>(b: &[u8], pos: usize) -> Result<[u8; N], ArchiveError> {
    let end = pos.checked_add(N);
    match end.and_then(|e| b.get(pos..e)) {
        Some(s) => {
            let mut a = [0u8; N];
            a.copy_from_slice(s);
            Ok(a)
        }
        None => bad(format!(
            "read of {N} bytes at {pos} is outside the {} byte buffer",
            b.len()
        )),
    }
}

fn u16_at(b: &[u8], pos: usize) -> Result<u16, ArchiveError> {
    Ok(u16::from_le_bytes(bytes_at(b, pos)?))
}
fn u32_at(b: &[u8], pos: usize) -> Result<u32, ArchiveError> {
    Ok(u32::from_le_bytes(bytes_at(b, pos)?))
}
fn i32_at(b: &[u8], pos: usize) -> Result<i32, ArchiveError> {
    Ok(i32::from_le_bytes(bytes_at(b, pos)?))
}
fn i64_at(b: &[u8], pos: usize) -> Result<i64, ArchiveError> {
    Ok(i64::from_le_bytes(bytes_at(b, pos)?))
}

/// Position reached by following the unsigned forward offset stored at `pos`.
fn follow(b: &[u8], pos: usize) -> Result<usize, ArchiveError> {
    let off = u32_at(b, pos)? as usize;
    match pos.checked_add(off) {
        Some(t) if t < b.len() => Ok(t),
        _ => bad(format!(
            "offset {off} at {pos} leaves the {} byte buffer",
            b.len()
        )),
    }
}

/// A table located in the buffer, with its vtable checked.
#[derive(Clone, Copy)]
struct Tab<'a> {
    b: &'a [u8],
    pos: usize,
    vt: usize,
    vt_len: usize,
    tab_len: Option<usize>,
}

impl<'a> Tab<'a> {
    /// `lenient` accepts a vtable shorter than its 4 byte header as "no fields",
    /// which is how the `version` entry reads in real files.
    fn at(b: &'a [u8], pos: usize, lenient: bool) -> Result<Tab<'a>, ArchiveError> {
        let so = i64::from(i32_at(b, pos)?);
        let vt = pos as i64 - so;
        if vt < 0 || vt as usize >= b.len().max(1) {
            return bad(format!("vtable of table at {pos} is outside the buffer"));
        }
        let vt = vt as usize;
        let vt_len = usize::from(u16_at(b, vt)?);
        if vt_len < 4 {
            if lenient {
                return Ok(Tab {
                    b,
                    pos,
                    vt,
                    vt_len: 0,
                    tab_len: None,
                });
            }
            return bad(format!(
                "vtable at {vt} is {vt_len} bytes, shorter than its header"
            ));
        }
        if vt + vt_len > b.len() {
            return bad(format!("vtable at {vt} runs past the end of the buffer"));
        }
        let tab_len = usize::from(u16_at(b, vt + 2)?);
        if pos + tab_len > b.len() {
            return bad(format!("table at {pos} runs past the end of the buffer"));
        }
        Ok(Tab {
            b,
            pos,
            vt,
            vt_len,
            tab_len: Some(tab_len),
        })
    }

    /// Absolute position of a slot's data, `None` when the field is absent.
    fn field(&self, slot: usize) -> Result<Option<usize>, ArchiveError> {
        let o = 4 + 2 * slot;
        if o + 2 > self.vt_len {
            return Ok(None);
        }
        let off = usize::from(u16_at(self.b, self.vt + o)?);
        if off == 0 {
            return Ok(None);
        }
        if let Some(t) = self.tab_len {
            if off >= t {
                return bad(format!("field offset {off} is outside a {t} byte table"));
            }
        }
        Ok(Some(self.pos + off))
    }

    fn i32(&self, slot: usize, default: i32) -> Result<i32, ArchiveError> {
        self.field(slot)?.map_or(Ok(default), |p| i32_at(self.b, p))
    }
    fn i64(&self, slot: usize) -> Result<i64, ArchiveError> {
        self.field(slot)?.map_or(Ok(0), |p| i64_at(self.b, p))
    }
    fn i8(&self, slot: usize) -> Result<i8, ArchiveError> {
        self.field(slot)?
            .map_or(Ok(0), |p| Ok(bytes_at::<1>(self.b, p)?[0] as i8))
    }
    fn bool(&self, slot: usize) -> Result<bool, ArchiveError> {
        self.field(slot)?
            .map_or(Ok(false), |p| Ok(bytes_at::<1>(self.b, p)?[0] != 0))
    }

    /// A string field. Absent reads as "". Invalid UTF-8 is replaced.
    fn string(&self, slot: usize) -> Result<String, ArchiveError> {
        let Some(p) = self.field(slot)? else {
            return Ok(String::new());
        };
        let q = follow(self.b, p)?;
        let n = u32_at(self.b, q)? as usize;
        let start = q + 4;
        let Some(raw) = start.checked_add(n).and_then(|e| self.b.get(start..e)) else {
            return bad(format!(
                "string of {n} bytes at {q} runs past the end of the buffer"
            ));
        };
        Ok(String::from_utf8_lossy(raw).into_owned())
    }

    fn table(&self, slot: usize) -> Result<Option<Tab<'a>>, ArchiveError> {
        match self.field(slot)? {
            None => Ok(None),
            Some(p) => Ok(Some(Tab::at(self.b, follow(self.b, p)?, false)?)),
        }
    }

    /// Tables of a vector field.
    fn tables(&self, slot: usize) -> Result<Vec<Tab<'a>>, ArchiveError> {
        let Some(p) = self.field(slot)? else {
            return Ok(Vec::new());
        };
        let v = follow(self.b, p)?;
        let n = u32_at(self.b, v)? as usize;
        if n > MAX_VECTOR_LEN {
            return bad(format!(
                "vector of {n} elements exceeds the limit {MAX_VECTOR_LEN}"
            ));
        }
        let table_end = n
            .checked_mul(4)
            .and_then(|x| x.checked_add(v + 4))
            .unwrap_or(usize::MAX);
        if table_end > self.b.len() {
            return bad(format!(
                "vector of {n} elements at {v} runs past the end of the buffer"
            ));
        }
        (0..n)
            .map(|i| {
                let slot_pos = v + 4 + 4 * i;
                Tab::at(self.b, follow(self.b, slot_pos)?, false)
            })
            .collect()
    }

    fn span(&self, slot: usize) -> Result<SourceSpan, ArchiveError> {
        let Some(p) = self.field(slot)? else {
            return Ok(SourceSpan::default());
        };
        // On disk the struct is written To first, then From.
        let to = SourcePosition {
            line: u16_at(self.b, p)?,
            column: u16_at(self.b, p + 2)?,
        };
        let from = SourcePosition {
            line: u16_at(self.b, p + 4)?,
            column: u16_at(self.b, p + 6)?,
        };
        Ok(SourceSpan { from, to })
    }

    fn time_span(&self, slot: usize) -> Result<TimeSpan, ArchiveError> {
        let Some(p) = self.field(slot)? else {
            return Ok(TimeSpan::default());
        };
        Ok(TimeSpan {
            start_time: i64_at(self.b, p)?,
            end_time: i64_at(self.b, p + 8)?,
        })
    }

    fn object_id(&self, slot: usize) -> Result<Option<ObjectId>, ArchiveError> {
        Ok(match self.table(slot)? {
            None => None,
            Some(t) => Some(ObjectId {
                number: t.i32(0, 0)?,
                object_type: ObjectType::from_i8(t.i8(1)?),
            }),
        })
    }
}

/// Counts decoded tables so shared sub-trees cannot expand without limit.
struct Budget(usize);

impl Budget {
    fn take(&mut self) -> Result<(), ArchiveError> {
        if self.0 == 0 {
            return bad(format!(
                "more than {MAX_NODES} tables in one method context"
            ));
        }
        self.0 -= 1;
        Ok(())
    }
}

fn decode_variable(
    t: Tab<'_>,
    depth: usize,
    budget: &mut Budget,
) -> Result<Variable, ArchiveError> {
    budget.take()?;
    if depth > MAX_VARIABLE_DEPTH {
        return bad(format!(
            "variables nest deeper than {MAX_VARIABLE_DEPTH} levels"
        ));
    }
    let mut children = Vec::new();
    for c in t.tables(0)? {
        children.push(decode_variable(c, depth + 1, budget)?);
    }
    Ok(Variable {
        has_children: t.bool(1)?,
        name: t.string(2)?,
        value: t.string(3)?,
        type_name: t.string(4)?,
        children,
    })
}

fn decode_variables(
    t: &Tab<'_>,
    slot: usize,
    budget: &mut Budget,
) -> Result<Vec<Variable>, ArchiveError> {
    t.tables(slot)?
        .into_iter()
        .map(|c| decode_variable(c, 1, budget))
        .collect()
}

fn root_table(b: &[u8], lenient: bool) -> Result<Tab<'_>, ArchiveError> {
    let root = u32_at(b, 0)? as usize;
    if root >= b.len() {
        return bad(format!(
            "root offset {root} is outside the {} byte buffer",
            b.len()
        ));
    }
    Tab::at(b, root, lenient)
}

/// Decode one `N.mdc` buffer. `index` is the `N` of the entry name.
pub fn decode_method_context(bytes: &[u8], index: usize) -> Result<MethodContext, ArchiveError> {
    let t = root_table(bytes, false)?;
    let mut budget = Budget(MAX_NODES);

    let exception = match t.table(2)? {
        None => None,
        Some(e) => Some(ExceptionContext {
            message: e.string(0)?,
            line_number: e.i32(1, 0)?,
            statement_span: e.span(2)?,
        }),
    };

    let mut frames = Vec::new();
    for f in t.tables(3)? {
        budget.take()?;
        frames.push(StackFrame {
            method_name: f.string(0)?,
            id: f.object_id(1)?.unwrap_or_default(),
            object_name: f.string(2)?,
            statement_span: f.span(3)?,
        });
    }

    let mut line_contexts = Vec::new();
    for l in t.tables(4)? {
        budget.take()?;
        line_contexts.push(LineContext {
            ordinal: l.i32(0, 0)?,
            statement_span: l.span(1)?,
            variables: decode_variables(&l, 2, &mut budget)?,
            timestamp: l.i64(3)?,
        });
    }

    let app = match t.table(6)? {
        None => None,
        Some(a) => Some(DeclaringApplication {
            app_name: a.string(0)?,
            app_publisher: a.string(1)?,
            app_version: a.string(2)?,
            app_id: a.string(3)?,
        }),
    };

    Ok(MethodContext {
        index,
        id: t.object_id(0)?.unwrap_or_default(),
        method_name: t.string(1)?,
        exception,
        frames,
        line_contexts,
        execution_span: t.time_span(5)?,
        app,
        is_builtin_code_unit_call: t.i8(7)? == 1,
        is_incomplete_measurement: t.i8(8)? == 1,
    })
}

fn version_fields(t: &Tab<'_>) -> Result<FormatVersion, ArchiveError> {
    Ok(FormatVersion {
        major: t.i32(0, SUPPORTED_MAJOR)?,
        minor: t.i32(1, 0)?,
        build: t.i32(2, 0)?,
        revision: t.i32(3, 0)?,
    })
}

/// Decode the `version` entry.
///
/// Real files are 64 bytes whose root offset is 0. The host reads the table at
/// position 0, finds a vtable size of 0 and applies every default (3.0.0.0).
/// The last 8 bytes hold a complete empty table, which also decodes to the
/// defaults, so both readings agree. This reader takes the table at the root
/// offset and accepts a vtable shorter than its header as "no fields".
pub fn decode_version(bytes: &[u8]) -> Result<FormatVersion, ArchiveError> {
    if bytes.is_empty() {
        return Err(ArchiveError::Version(
            "snapshot version entry is empty: this reader supports format 3.x".to_string(),
        ));
    }
    let root = u32_at(bytes, 0)? as usize;
    if root >= bytes.len() {
        return bad(format!("version root offset {root} is outside the buffer"));
    }
    version_fields(&Tab::at(bytes, root, true)?)
}

fn check_version(v: FormatVersion) -> Result<FormatVersion, ArchiveError> {
    if v.major == SUPPORTED_MAJOR {
        Ok(v)
    } else {
        Err(ArchiveError::Version(format!(
            "snapshot format version {v} is not supported: this reader supports format {SUPPORTED_MAJOR}.x"
        )))
    }
}

// ---------------------------------------------------------------------------
// Archive
// ---------------------------------------------------------------------------

/// Size caps applied while reading a zip.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub entries: usize,
    pub entry_bytes: u64,
    pub total_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            entries: MAX_ENTRIES,
            entry_bytes: MAX_ENTRY_BYTES,
            total_bytes: MAX_TOTAL_BYTES,
        }
    }
}

/// A snapshot zip read into memory.
#[derive(Debug, Clone)]
pub struct SnapshotArchive {
    version: FormatVersion,
    contexts: Vec<MethodContext>,
    sources: HashMap<(ObjectType, i32), String>,
}

enum EntryKind {
    Version,
    Mdc(usize),
    Source(ObjectType, i32),
}

fn classify(full_name: &str) -> Option<EntryKind> {
    let name = full_name.rsplit('/').next().unwrap_or(full_name);
    if name == "version" {
        return Some(EntryKind::Version);
    }
    if let Some(stem) = name.strip_suffix(".mdc") {
        if !stem.is_empty() && stem.len() <= 18 && stem.bytes().all(|c| c.is_ascii_digit()) {
            return stem.parse().ok().map(EntryKind::Mdc);
        }
        return None;
    }
    let stem = name.strip_suffix(".al")?;
    let (kind, id) = stem.split_once('%')?;
    Some(EntryKind::Source(
        ObjectType::from_name(kind)?,
        id.parse().ok()?,
    ))
}

impl SnapshotArchive {
    /// Read a snapshot zip from disk.
    pub fn open(path: &Path) -> Result<Self, ArchiveError> {
        Self::from_reader(std::fs::File::open(path)?)
    }

    /// Read a snapshot zip held in memory.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ArchiveError> {
        Self::from_reader(Cursor::new(bytes))
    }

    /// Read a snapshot zip. Entry count, per entry size and total size are
    /// capped, the sizes by the bytes actually inflated, not the declared size.
    pub fn from_reader<R: Read + Seek>(reader: R) -> Result<Self, ArchiveError> {
        Self::from_reader_with(reader, Limits::default())
    }

    /// Like [`from_reader`](Self::from_reader) with explicit size caps.
    pub fn from_reader_with<R: Read + Seek>(
        reader: R,
        limits: Limits,
    ) -> Result<Self, ArchiveError> {
        let Limits {
            entries: max_entries,
            entry_bytes: max_entry,
            total_bytes: max_total,
        } = limits;
        let mut zip = zip::ZipArchive::new(reader).map_err(|e| ArchiveError::Zip(e.to_string()))?;
        if zip.len() > max_entries {
            return Err(ArchiveError::Limit(format!(
                "{} entries, the limit is {max_entries}",
                zip.len()
            )));
        }
        let mut total = 0u64;
        let mut version_bytes: Option<Vec<u8>> = None;
        let mut mdcs: Vec<(usize, String, Vec<u8>)> = Vec::new();
        let mut sources = HashMap::new();
        for i in 0..zip.len() {
            let entry = zip
                .by_index(i)
                .map_err(|e| ArchiveError::Zip(e.to_string()))?;
            if entry.is_dir() {
                continue;
            }
            let name = entry
                .name()
                .map_err(|e| ArchiveError::Zip(e.to_string()))?
                .to_string();
            let Some(kind) = classify(&name) else {
                continue;
            };
            if entry.size() > max_entry {
                return Err(ArchiveError::Limit(format!(
                    "entry {name} declares {} bytes, the limit is {max_entry}",
                    entry.size()
                )));
            }
            let mut data = Vec::new();
            entry.take(max_entry + 1).read_to_end(&mut data)?;
            if data.len() as u64 > max_entry {
                return Err(ArchiveError::Limit(format!(
                    "entry {name} inflates past {max_entry} bytes"
                )));
            }
            total += data.len() as u64;
            if total > max_total {
                return Err(ArchiveError::Limit(format!(
                    "entries inflate past {max_total} bytes in total"
                )));
            }
            match kind {
                EntryKind::Version => version_bytes = Some(data),
                EntryKind::Mdc(n) => mdcs.push((n, name, data)),
                EntryKind::Source(t, id) => {
                    sources.insert((t, id), String::from_utf8_lossy(&data).into_owned());
                }
            }
        }

        // The version guard runs before any method context is decoded.
        let Some(vb) = version_bytes else {
            return Err(ArchiveError::Version(
                "snapshot has no version entry (found none): this reader supports format 3.x"
                    .to_string(),
            ));
        };
        let version = check_version(decode_version(&vb)?)?;

        mdcs.sort_by_key(|(n, _, _)| *n);
        let mut contexts = Vec::with_capacity(mdcs.len());
        for (n, name, data) in mdcs {
            // The host reads a zero length entry as null and skips it.
            if data.is_empty() {
                continue;
            }
            let ctx = decode_method_context(&data, n)
                .map_err(|e| ArchiveError::Format(format!("{name}: {e}")))?;
            contexts.push(ctx);
        }
        Ok(SnapshotArchive {
            version,
            contexts,
            sources,
        })
    }

    pub fn version(&self) -> FormatVersion {
        self.version
    }

    /// Method contexts in numeric `N` order.
    pub fn method_contexts(&self) -> &[MethodContext] {
        &self.contexts
    }

    /// The AL source of an object, if the zip ships it.
    pub fn source(&self, object_type: ObjectType, number: i32) -> Option<&str> {
        self.sources.get(&(object_type, number)).map(String::as_str)
    }

    /// One line of an object's source, `line` 1-based.
    pub fn source_line(&self, object_type: ObjectType, number: i32, line: u32) -> Option<&str> {
        let idx = usize::try_from(line.checked_sub(1)?).ok()?;
        self.source(object_type, number)?.lines().nth(idx)
    }

    /// Objects whose source is in the zip.
    pub fn source_ids(&self) -> Vec<(ObjectType, i32)> {
        let mut v: Vec<_> = self.sources.keys().copied().collect();
        v.sort_by_key(|(t, n)| (t.as_i8(), *n));
        v
    }

    /// Stops in recorded order. See the module docs for what a stop is.
    pub fn stops(&self) -> Vec<Stop> {
        let mut out = Vec::new();
        for ctx in &self.contexts {
            let exc_idx = ctx.exception.as_ref().and_then(|e| {
                usize::try_from(e.line_number)
                    .ok()
                    .filter(|i| *i < ctx.line_contexts.len())
            });
            for (i, lc) in ctx.line_contexts.iter().enumerate() {
                if !lc.variables.is_empty() {
                    out.push(build_stop(ctx, i, lc.statement_span, &lc.variables, None));
                }
                if exc_idx == Some(i) {
                    out.push(error_stop(ctx));
                }
            }
            if ctx.exception.is_some() && exc_idx.is_none() {
                out.push(error_stop(ctx));
            }
        }
        out
    }
}

fn error_stop(ctx: &MethodContext) -> Stop {
    let exc = ctx.exception.clone().unwrap_or_default();
    let lc = usize::try_from(exc.line_number)
        .ok()
        .and_then(|i| ctx.line_contexts.get(i).map(|l| (i, l)));
    match lc {
        Some((i, l)) => build_stop(ctx, i, l.statement_span, &l.variables, Some(exc.message)),
        None => build_stop(ctx, 0, exc.statement_span, &[], Some(exc.message)),
    }
}

/// Split recorded variables into Locals and Globals the way the host does.
fn split_scopes(vars: &[Variable]) -> (Vec<Variable>, Vec<Variable>) {
    match vars.first() {
        Some(g) if g.name.eq_ignore_ascii_case("<Globals>") => {
            (vars[1..].to_vec(), g.children.clone())
        }
        _ => (vars.to_vec(), Vec::new()),
    }
}

fn build_stop(
    ctx: &MethodContext,
    line_context: usize,
    span: SourceSpan,
    vars: &[Variable],
    error: Option<String>,
) -> Stop {
    let mut frames = Vec::new();
    let top = ctx.frames.first();
    frames.push(Frame {
        method_name: top
            .map(|f| f.method_name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| ctx.method_name.clone()),
        object: top.map_or(ctx.id, |f| f.id),
        object_name: top.map(|f| f.object_name.clone()).unwrap_or_default(),
        line: span.first_line(),
    });
    for f in ctx.frames.iter().skip(1) {
        frames.push(Frame {
            method_name: f.method_name.clone(),
            object: f.id,
            object_name: f.object_name.clone(),
            line: f.statement_span.first_line(),
        });
    }
    let (locals, globals) = split_scopes(vars);
    Stop {
        kind: if error.is_some() {
            StopKind::Error
        } else {
            StopKind::Snappoint
        },
        context_index: ctx.index,
        line_context_index: line_context,
        frames,
        locals,
        globals,
        error,
    }
}

/// Why a stop exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StopKind {
    Snappoint,
    Error,
}

/// One frame of a stop's call stack. `line` is 1-based.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Frame {
    pub method_name: String,
    pub object: ObjectId,
    pub object_name: String,
    pub line: u32,
}

impl Frame {
    /// Entry name of the object's source in the zip, for example `Page%42.al`.
    pub fn source_file_name(&self) -> String {
        format!(
            "{}%{}.al",
            self.object.object_type.name(),
            self.object.number
        )
    }
}

/// A point the replay stops at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stop {
    pub kind: StopKind,
    /// `N` of the method context.
    pub context_index: usize,
    /// Index into that context's line contexts.
    pub line_context_index: usize,
    /// Top frame first, at the stop line, then the callers.
    pub frames: Vec<Frame>,
    pub locals: Vec<Variable>,
    pub globals: Vec<Variable>,
    /// Exception message of an error stop.
    pub error: Option<String>,
}

impl Stop {
    /// 1-based line of the stop in the top frame.
    pub fn line(&self) -> u32 {
        self.frames.first().map_or(0, |f| f.line)
    }

    pub fn top_frame(&self) -> Option<&Frame> {
        self.frames.first()
    }
}

// ---------------------------------------------------------------------------
// Text renderer (mirrors print_stops in tools/business-central/bc-snapshot)
// ---------------------------------------------------------------------------

/// What [`render_stops`] prints.
#[derive(Debug, Clone, Default)]
pub struct RenderOptions {
    /// Print only variables with these names (case insensitive). `None` prints all.
    pub vars: Option<Vec<String>>,
    /// Record fields to print besides the primary key fields (case insensitive).
    pub fields: Vec<String>,
    /// Print only this stop, 1-based. `None` prints all.
    pub only_stop: Option<usize>,
}

fn short(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let cut: String = s.chars().take(n.saturating_sub(3)).collect();
        format!("{cut}...")
    }
}

fn qname(n: &str) -> String {
    if n.chars().any(|c| !(c.is_ascii_alphanumeric() || c == '_')) {
        format!("\"{n}\"")
    } else {
        n.to_string()
    }
}

fn object_label(f: &Frame) -> String {
    format!(
        "{} {} {}",
        f.object.object_type.display_name(),
        f.object.number,
        qname(&f.object_name)
    )
    .trim_end()
    .to_string()
}

fn record_parts(children: &[Variable], want: &[String]) -> Vec<String> {
    let find = |name: &str| children.iter().rev().find(|c| c.name == name);
    let fields: &[Variable] = find("Fields").map_or(&[], |f| &f.children);
    let key: Vec<String> = find("Keys")
        .and_then(|k| k.children.first())
        .map(|k| k.name.split(',').map(|s| s.trim().to_lowercase()).collect())
        .unwrap_or_default();
    let mut picked: Vec<usize> = Vec::new();
    for k in &key {
        for (i, c) in fields.iter().enumerate() {
            if c.name.to_lowercase() == *k {
                picked.push(i);
            }
        }
    }
    for (i, c) in fields.iter().enumerate() {
        if want.contains(&c.name.to_lowercase()) && !picked.contains(&i) {
            picked.push(i);
        }
    }
    let mut parts: Vec<String> = picked
        .iter()
        .map(|&i| format!("{} = {}", fields[i].name, short(&fields[i].value, 60)))
        .collect();
    if let Some(filters) = find("Filters") {
        if !filters.value.is_empty() {
            parts.push(format!("filters: {}", short(&filters.value, 120)));
        }
    }
    parts
}

fn fmt_var(v: &Variable, want: &[String]) -> String {
    let ch = &v.children;
    if ch.is_empty() {
        return format!("{} = {}", qname(&v.name), short(&v.value, 160));
    }
    if ch.iter().any(|c| c.name == "Fields") && ch.iter().any(|c| c.name == "Keys") {
        return format!(
            "{} ({}): {}",
            qname(&v.name),
            v.type_name,
            record_parts(ch, want).join(", ")
        );
    }
    let parts: Vec<String> = ch
        .iter()
        .take(5)
        .filter(|c| !c.name.starts_with('<'))
        .map(|c| format!("{} = {}", c.name, short(&c.value, 60)))
        .collect();
    let mut s = format!(
        "{} ({}) = {}",
        qname(&v.name),
        v.type_name,
        short(&v.value, 60)
    );
    if !parts.is_empty() {
        s.push_str(": ");
        s.push_str(&parts.join(", "));
    }
    s
}

/// Render one stop. `n` is the 1-based number shown in the header.
pub fn render_stop(stop: &Stop, n: usize, opts: &RenderOptions) -> String {
    let want_vars: Option<Vec<String>> = opts
        .vars
        .as_ref()
        .map(|v| v.iter().map(|s| s.trim().to_lowercase()).collect());
    let want_fields: Vec<String> = opts
        .fields
        .iter()
        .map(|s| s.trim().to_lowercase())
        .collect();
    let mut out = String::new();
    let label = match stop.kind {
        StopKind::Error => "Error capture",
        StopKind::Snappoint => "Stop",
    };
    if let Some(top) = stop.top_frame() {
        let _ = writeln!(
            out,
            "== {label} {n}: {} line {} ({})",
            object_label(top),
            top.line,
            top.method_name
        );
    } else {
        let _ = writeln!(out, "== {label} {n}");
    }
    out.push_str("Call stack:\n");
    for f in &stop.frames {
        let _ = writeln!(
            out,
            "  {} {} line {}",
            object_label(f),
            f.method_name,
            f.line
        );
    }
    for (scope, vars) in [("Locals", &stop.locals), ("Globals", &stop.globals)] {
        let shown: Vec<&Variable> = vars
            .iter()
            .filter(|v| {
                want_vars
                    .as_ref()
                    .is_none_or(|w| w.contains(&v.name.to_lowercase()))
            })
            .collect();
        let error = if scope == "Locals" {
            stop.error.as_deref()
        } else {
            None
        };
        if shown.is_empty() && error.is_none() {
            continue;
        }
        let _ = writeln!(out, "{scope}:");
        if let Some(e) = error {
            let _ = writeln!(out, "  ERROR: {e}");
        }
        for v in shown {
            let _ = writeln!(out, "  {}", fmt_var(v, &want_fields));
        }
    }
    out.push('\n');
    out
}

/// Render all stops, or the one named by `opts.only_stop`.
pub fn render_stops(stops: &[Stop], opts: &RenderOptions) -> String {
    if stops.is_empty() {
        return "no snappoint hits: the snapshot holds no captured variables\n".to_string();
    }
    let mut out = String::new();
    for (i, stop) in stops.iter().enumerate() {
        let n = i + 1;
        if opts.only_stop.is_none_or(|o| o == n) {
            out.push_str(&render_stop(stop, n, opts));
        }
    }
    if out.is_empty() {
        out = format!(
            "no stop {} in this snapshot ({} stops)\n",
            opts.only_stop.unwrap_or(0),
            stops.len()
        );
    }
    out
}

#[cfg(test)]
mod tests;
