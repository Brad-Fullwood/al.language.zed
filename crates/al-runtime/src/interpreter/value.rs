//! AL runtime values for the pure-Rust interpreter.
//!
//! **Stability contract.** This enum is the boundary type between the
//! interpreter, mock BC runtime, and consumers such as DAP variable inspection
//! and test execution. Variants here are considered stable for parallel work;
//! new variants may be appended, but existing variants must not be renamed or
//! reordered.
//!
//! Variant set tracks AL's primitive + structured value space:
//!  * scalars: Integer, Decimal, Boolean, Char, Date, Time, DateTime,
//!    Duration, Guid, Text, Code
//!  * sentinels: Null, Empty (uninitialised vs. cleared)
//!  * structured: Option, Record, RecordRef, Variant, Array, List, Dict,
//!    Blob, Stream, ErrorInfo

use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

pub use rust_decimal::Decimal;

/// The entries of an AL `Dictionary`, in insertion order: the normalised
/// key text maps to the key as given and its value.
pub type DictEntries = indexmap::IndexMap<String, (Value, Value)>;

/// The contents of an AL reference type (`List`, `Dictionary`,
/// `TextBuilder`): cloning the handle shares the contents, as assigning the
/// AL variable does.
///
/// Values cross to the test runner's thread, hence `Arc<Mutex>`. Hold one
/// guard at a time: locking the same contents twice deadlocks.
///
/// The contents count toward the test's [`MAX_HELD_BYTES`]: the bytes they
/// held when made, and what the methods that add to them charge, until the
/// last handle drops.
#[derive(Debug, Default)]
pub struct Shared<T: Contents>(Arc<SharedContents<T>>);

#[derive(Debug, Default)]
struct SharedContents<T> {
    contents: Mutex<T>,
    /// The bytes these contents have added to the test's total.
    held: AtomicUsize,
}

/// What a [`Shared`] handle holds: a List's values, a Dictionary's entries
/// or a TextBuilder's text.
pub trait Contents {
    /// Move the values these contents hold into `into`.
    fn take_values(&mut self, into: &mut Vec<Value>);

    /// The bytes the contents take, as [`MAX_HELD_BYTES`] counts them.
    fn held_bytes(&self) -> usize;
}

impl Contents for String {
    fn take_values(&mut self, _into: &mut Vec<Value>) {}

    fn held_bytes(&self) -> usize {
        self.len()
    }
}

impl Contents for Vec<Value> {
    fn take_values(&mut self, into: &mut Vec<Value>) {
        into.append(self);
    }

    fn held_bytes(&self) -> usize {
        self.iter().map(element_bytes).sum()
    }
}

impl Contents for DictEntries {
    fn take_values(&mut self, into: &mut Vec<Value>) {
        for (_, (key, value)) in self.drain(..) {
            into.push(key);
            into.push(value);
        }
    }

    fn held_bytes(&self) -> usize {
        self.iter()
            .map(|(key_text, (key, value))| entry_bytes(key_text, key, value))
            .sum()
    }
}

impl<T: Contents> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

/// Dropping the last handle to a list drops the values it holds, and each
/// of those drops the values it holds. The values are moved into a work list
/// and dropped in a loop, so a chain of lists nested a hundred thousand deep
/// is freed without one native frame per level.
impl<T: Contents> Drop for Shared<T> {
    fn drop(&mut self) {
        let mut orphans = Vec::new();
        self.take_values_if_last(&mut orphans);
        while let Some(mut value) = orphans.pop() {
            value.take_held_values(&mut orphans);
        }
    }
}

impl<T: Contents> Shared<T> {
    /// Move the values the contents hold into `into`, and take their bytes
    /// off the test's total, when this is the last handle to them.
    fn take_values_if_last(&mut self, into: &mut Vec<Value>) {
        if let Some(shared) = Arc::get_mut(&mut self.0) {
            release_held_bytes(std::mem::take(shared.held.get_mut()));
            shared
                .contents
                .get_mut()
                .unwrap_or_else(PoisonError::into_inner)
                .take_values(into);
        }
    }
}

impl<T: Contents> Shared<T> {
    /// New contents, whose bytes count toward the test's total. The total
    /// is checked at the next operation that adds to it.
    pub fn new(contents: T) -> Self {
        let held = contents.held_bytes();
        add_held_bytes(held);
        Self(Arc::new(SharedContents {
            contents: Mutex::new(contents),
            held: AtomicUsize::new(held),
        }))
    }

    /// The contents, locked until the guard drops.
    pub fn lock(&self) -> MutexGuard<'_, T> {
        // A panic while locked leaves the contents as they were. Use them.
        self.0
            .contents
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Count `bytes` more for these contents, or refuse when the test's
    /// total would pass [`MAX_HELD_BYTES`]. `operation` names what adds
    /// them, for the message.
    pub(crate) fn charge(&self, operation: &str, bytes: usize) -> Result<(), String> {
        hold_bytes(operation, bytes)?;
        self.0.held.fetch_add(bytes, AtomicOrdering::Relaxed);
        Ok(())
    }

    /// Count `bytes` fewer for these contents, at most what they count.
    pub(crate) fn release(&self, bytes: usize) {
        // A compare-exchange loop instead of `fetch_update`, which Rust 1.99 deprecates in favour
        // of `try_update`, a method older toolchains do not have.
        let held = &self.0.held;
        let mut before = held.load(AtomicOrdering::Relaxed);
        while let Err(current) = held.compare_exchange_weak(
            before,
            before.saturating_sub(bytes),
            AtomicOrdering::Relaxed,
            AtomicOrdering::Relaxed,
        ) {
            before = current;
        }
        release_held_bytes(before.min(bytes));
    }

    /// Count the contents as `bytes`, what a TextBuilder's text takes after
    /// a method changed it, and refuse when the test's total is past
    /// [`MAX_HELD_BYTES`].
    pub(crate) fn recount(&self, operation: &str, bytes: usize) -> Result<(), String> {
        let before = self.0.held.swap(bytes, AtomicOrdering::Relaxed);
        if bytes <= before {
            release_held_bytes(before - bytes);
            return Ok(());
        }
        add_held_bytes(bytes - before);
        check_held_bytes(operation)
    }

    /// Whether both handles name the same contents.
    pub fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// The address of the contents, the same for every handle to them.
    fn address(&self) -> usize {
        Arc::as_ptr(&self.0).cast::<()>() as usize
    }
}

impl<T: Contents + Clone> Shared<T> {
    /// A copy of the contents, detached from the handle.
    pub fn snapshot(&self) -> T {
        self.lock().clone()
    }
}

/// A `List` or `Dictionary` value: its shared contents and the types its
/// declaration gives it, written as in source (`Code[20]`). The member type
/// is the element type of a List and the key type of a Dictionary, and the
/// value type is the value type of a Dictionary. `None` when no declaration
/// made the value.
#[derive(Debug)]
pub struct Collection<T: Contents> {
    contents: Shared<T>,
    member_type: Option<Arc<str>>,
    value_type: Option<Arc<str>>,
}

impl<T: Contents> Clone for Collection<T> {
    fn clone(&self) -> Self {
        Self {
            contents: self.contents.clone(),
            member_type: self.member_type.clone(),
            value_type: self.value_type.clone(),
        }
    }
}

impl<T: Contents> Collection<T> {
    pub fn new(contents: T, member_type: Option<&str>) -> Self {
        Self {
            contents: Shared::new(contents),
            member_type: member_type.map(Arc::from),
            value_type: None,
        }
    }

    /// The same collection with `value_type` as its Dictionary value type.
    pub fn with_value_type(mut self, value_type: Option<&str>) -> Self {
        self.value_type = value_type.map(Arc::from);
        self
    }

    /// The contents, locked until the guard drops.
    pub fn lock(&self) -> MutexGuard<'_, T> {
        self.contents.lock()
    }

    /// Whether both values name the same contents.
    pub fn same(&self, other: &Self) -> bool {
        self.contents.same(&other.contents)
    }

    /// Count `bytes` more for the contents, or refuse when the test's total
    /// would pass [`MAX_HELD_BYTES`].
    pub(crate) fn charge(&self, operation: &str, bytes: usize) -> Result<(), String> {
        self.contents.charge(operation, bytes)
    }

    /// Count `bytes` fewer for the contents.
    pub(crate) fn release(&self, bytes: usize) {
        self.contents.release(bytes);
    }

    /// The address of the contents, the same for every copy of the value.
    fn address(&self) -> usize {
        self.contents.address()
    }

    /// The declared element type of a List, or key type of a Dictionary.
    pub fn member_type(&self) -> Option<&str> {
        self.member_type.as_deref()
    }

    /// The declared value type of a Dictionary.
    pub fn value_type(&self) -> Option<&str> {
        self.value_type.as_deref()
    }

    /// New empty contents with the same declared types, as `Clear` leaves.
    pub fn emptied(&self) -> Self
    where
        T: Default,
    {
        Self {
            contents: Shared::new(T::default()),
            member_type: self.member_type.clone(),
            value_type: self.value_type.clone(),
        }
    }
}

impl<T: Contents + Clone> Collection<T> {
    /// A copy of the contents, detached from the value.
    pub fn snapshot(&self) -> T {
        self.contents.snapshot()
    }
}

/// AL `Date` carrier: days since 0001-01-01.
pub type AlDate = i64;
/// AL `Time` carrier: milliseconds since midnight, in `[0, 86_400_000)`.
pub type AlTime = i64;
/// AL `DateTime` carrier: milliseconds since the AL epoch (0001-01-01).
pub type AlDateTime = i64;

/// Milliseconds in one day — the conversion factor between the `Date` (day)
/// and `DateTime` (millisecond) carriers.
pub const MS_PER_DAY: i64 = 86_400_000;

/// Days from the AL epoch (0001-01-01) to the Unix epoch (1970-01-01).
/// Used to bridge the system clock (Unix-based) and the AL `Date`/`DateTime`
/// carriers (0001-01-01-based). The value is the standard proleptic-Gregorian
/// offset (`days_from_civil(1,1,1) == -719162`).
pub const AL_EPOCH_TO_UNIX_DAYS: i64 = 719_162;

/// Days since the Unix epoch (1970-01-01) for a proleptic-Gregorian
/// year/month/day. Howard Hinnant's `days_from_civil` algorithm — exact for
/// all `i64` years, no leap-year edge cases. `month` is 1..=12, `day` 1..=31.
pub fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Days since the AL epoch (0001-01-01) for a year/month/day. `0001-01-01`
/// maps to day 0; `1970-01-01` maps to [`AL_EPOCH_TO_UNIX_DAYS`].
pub fn al_days_from_ymd(year: i64, month: i64, day: i64) -> i64 {
    days_from_civil(year, month, day) + AL_EPOCH_TO_UNIX_DAYS
}

/// Proleptic-Gregorian `(year, month, day)` for a days-since-Unix-epoch count.
/// The exact inverse of [`days_from_civil`] (Howard Hinnant's `civil_from_days`).
pub fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if month <= 2 { y + 1 } else { y }, month, day)
}

/// `(year, month, day)` for an AL `Date` day carrier (days since 0001-01-01).
pub fn ymd_from_al_days(days: AlDate) -> (i64, i64, i64) {
    civil_from_days(days - AL_EPOCH_TO_UNIX_DAYS)
}

/// One AL runtime value.
///
/// `PartialEq` is implemented manually (below) to mirror the `Ord` total
/// ordering. `Decimal` is an exact 96-bit decimal (`rust_decimal`) with a
/// total `Ord` and no NaN, so equality is exact and the three impls
/// (PartialEq / Eq / Ord) stay consistent, letting `Value` be a valid
/// `BTreeMap` key.
#[derive(Debug, Clone)]
pub enum Value {
    /// Uninitialised slot (before assignment).
    Null,
    /// Cleared / default-initialised value (`Clear(x)` semantics).
    Empty,
    /// AL `Integer` — 32-bit signed. Stored in an i64 carrier, but arithmetic
    /// traps at the i32 range to match BC (see `apply_binary`). Distinct from
    /// [`Value::BigInteger`] so the interpreter can apply the right overflow
    /// width; the two compare equal by numeric value.
    Integer(i64),
    /// AL `Decimal` — exact 96-bit decimal (`rust_decimal::Decimal`), matching
    /// BC's `System.Decimal`: no binary-float drift and no NaN or infinity.
    Decimal(Decimal),
    Boolean(bool),
    /// AL `Char` — single Unicode code point.
    Char(char),
    /// AL `Text[N]` — variable-length string, length cap not enforced here.
    Text(String),
    /// AL `Code[N]` — uppercase string, length cap not enforced here.
    Code(String),
    /// AL `TextBuilder`, a string its methods change in place. A reference
    /// type like `List`.
    TextBuilder(Shared<String>),
    /// `JsonObject`, `JsonArray`, `JsonToken` or `JsonValue`: a reference
    /// into the dispatch context's JSON arena.
    Json(crate::interpreter::json::JsonRef),
    /// AL `Date` — days since AL epoch (0001-01-01).
    Date(AlDate),
    /// AL `Time` — milliseconds since midnight.
    Time(AlTime),
    /// AL `DateTime` — milliseconds since AL epoch.
    DateTime(AlDateTime),
    /// AL `Duration` — milliseconds.
    Duration(i64),
    /// AL `Guid` — opaque identifier stored in string form.
    Guid(String),
    /// AL `Option` — name + ordinal pair.
    Option {
        /// The option-set name (e.g. `Status`).
        type_name: String,
        /// The selected member name (e.g. `Open`).
        member: String,
        ordinal: i64,
    },
    /// AL `Record` — handle into the in-memory table store.
    Record(RecordValue),
    RecordRef(RecordValue),
    /// AL `Variant` — tagged any-value.
    Variant(Box<Value>),
    /// AL `array[N]` of homogeneous values.
    Array(Vec<Value>),
    /// AL `List of [T]`. A reference type: copies of the value, and a
    /// parameter passed without `var`, share one list.
    List(Collection<Vec<Value>>),
    /// AL `Dictionary of [K, V]`, keyed by the serialised K after it is
    /// converted to the declared key type. A reference type like `List`.
    Dict(Collection<DictEntries>),
    /// AL `Blob` / `InStream` / `OutStream` — raw bytes.
    Blob(Vec<u8>),
    /// AL `ErrorInfo` — structured error captured by `asserterror` / `Error`.
    ErrorInfo(Box<ErrorInfo>),
    /// AL `Codeunit <Subtype>` instance handle. The interpreter is BC-free, so
    /// a codeunit variable carries only the declared subtype's object name; a
    /// method call on it (`MyCu.DoStuff(...)`) is dispatched to that workspace
    /// object's procedure. Added at the end of the enum to preserve the
    /// variant-ordering stability contract documented above.
    Codeunit {
        /// The declared subtype object name (e.g. `"Library - Sales"`).
        object_name: String,
        /// The instance whose globals this variable's calls see, given on
        /// its first call. Copies of the variable share it.
        instance: Option<u64>,
    },
    /// AL `BigInteger` — 64-bit signed. Appended to the enum to preserve the
    /// variant-ordering stability contract; it is treated as the same numeric
    /// class as [`Value::Integer`] for comparison (see `variant_index`), and
    /// arithmetic on it traps only at the i64 range.
    BigInteger(i64),
    /// Inclusive range expression (`low .. high`). This is an expression
    /// carrier used by CASE labels and `in` set members, not a user-declarable
    /// AL variable type.
    Range {
        start: Box<Value>,
        end: Box<Value>,
    },
}

/// In-memory record handle backed by `mock::record::MockRecord`.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordValue {
    pub table_name: String,
    pub table_id: i32,
    /// Opaque handle into the mock table store. `None` means "no current
    /// record" (e.g. after `Reset` and before `FindFirst`).
    pub handle: Option<u64>,
    /// Declared `Record "X" temporary`. The rows then live in this variable
    /// rather than in the table, so the runtime keys its store per variable.
    pub temporary: bool,
}

// Variants are ordered by their declaration index, then within each variant
// by an obvious natural order. `Decimal` has an exact total `Ord`.
// `Variant`/`Array`/`List`/`Dict`/`Blob`/`ErrorInfo` are never used as
// primary-key components in BC, so their orderings are implementation-defined
// (length-then-content) — adequate for BTreeMap stability without committing
// to an external contract.

/// Manual `PartialEq` mirroring the `Ord` impl so the three trait impls
/// stay consistent.
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for Value {}

impl Ord for Value {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        compare(self, other)
    }
}

/// The pairs of List or Dictionary contents one comparison has started on,
/// by address. A list can hold a list that holds it (`A.Add(B); B.Add(A)`),
/// so a pair met again counts as equal, and the walk ends. A pair that
/// differs ends the whole comparison, so every pair in the set is still being
/// compared or was equal, and a pair is walked at most once.
type MetPairs = std::collections::HashSet<(usize, usize)>;

/// What comparing one pair of values gives: an order, or two sequences of
/// values to compare element by element, a shorter sequence that matches so
/// far first.
enum Step {
    Done(std::cmp::Ordering),
    Descend(Vec<Value>, Vec<Value>),
}

/// Compare two values. Nested Arrays, Lists and Dictionaries are walked with
/// a stack of the sequences being compared, so a chain of lists nested a
/// hundred thousand deep takes heap and no native stack.
fn compare(left: &Value, right: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let mut met = MetPairs::new();
    let (left, right) = match compare_one(left, right, &mut met) {
        Step::Done(ordering) => return ordering,
        Step::Descend(left, right) => (left, right),
    };
    let mut pending = vec![(left.into_iter(), right.into_iter())];
    while let Some((left, right)) = pending.last_mut() {
        // A cancelled test stops here, and the statement that compared ends
        // it. The order returned then does not matter.
        if crate::interpreter::dispatch::thread_cancelled() {
            return Ordering::Equal;
        }
        let step = match (left.next(), right.next()) {
            (None, None) => {
                pending.pop();
                continue;
            }
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(a), Some(b)) => compare_one(&a, &b, &mut met),
        };
        match step {
            Step::Done(Ordering::Equal) => {}
            Step::Done(different) => return different,
            Step::Descend(left, right) => pending.push((left.into_iter(), right.into_iter())),
        }
    }
    Ordering::Equal
}

/// Compare two values of which neither holds others, or give the sequences
/// the comparison continues with.
fn compare_one(left: &Value, right: &Value, met: &mut MetPairs) -> Step {
    use std::cmp::Ordering;
    use Value::*;
    fn variant_index(v: &Value) -> u8 {
        match v {
            Null => 0,
            Empty => 1,
            // Integer and BigInteger are one numeric class: same index so
            // they order by value, and `5 = 5L` holds.
            Integer(_) | BigInteger(_) => 2,
            Decimal(_) => 3,
            Boolean(_) => 4,
            Char(_) => 5,
            Text(_) => 6,
            Code(_) => 7,
            Date(_) => 8,
            Time(_) => 9,
            DateTime(_) => 10,
            Duration(_) => 11,
            Guid(_) => 12,
            Option { .. } => 13,
            Record(_) => 14,
            RecordRef(_) => 15,
            Variant(_) => 16,
            Array(_) => 17,
            List(_) => 18,
            Dict(_) => 19,
            Blob(_) => 20,
            ErrorInfo(_) => 21,
            Codeunit { .. } => 22,
            Range { .. } => 23,
            TextBuilder(_) => 24,
            Json(_) => 25,
        }
    }
    /// The keys and values of a Dictionary's entries, entry by entry.
    fn entries_in_order(dict: &Collection<DictEntries>) -> Vec<Value> {
        dict.lock()
            .values()
            .flat_map(|(key, value)| [key.clone(), value.clone()])
            .collect()
    }
    let mine = variant_index(left);
    let theirs = variant_index(right);
    if mine != theirs {
        return Step::Done(mine.cmp(&theirs));
    }
    Step::Done(match (left, right) {
        (Null, Null) | (Empty, Empty) => Ordering::Equal,
        (Integer(a) | BigInteger(a), Integer(b) | BigInteger(b)) => a.cmp(b),
        (Decimal(a), Decimal(b)) => a.cmp(b),
        (Boolean(a), Boolean(b)) => a.cmp(b),
        (Char(a), Char(b)) => a.cmp(b),
        (Text(a), Text(b)) | (Code(a), Code(b)) => a.cmp(b),
        (TextBuilder(a), TextBuilder(b)) if a.same(b) => Ordering::Equal,
        (TextBuilder(a), TextBuilder(b)) => a.snapshot().cmp(&b.snapshot()),
        (Date(a), Date(b)) | (Time(a), Time(b)) | (DateTime(a), DateTime(b)) => a.cmp(b),
        (Duration(a), Duration(b)) => a.cmp(b),
        (Guid(a), Guid(b)) => a.cmp(b),
        (Json(a), Json(b)) => a.cmp(b),
        (
            Option {
                type_name: at,
                member: am,
                ordinal: ao,
            },
            Option {
                type_name: bt,
                member: bm,
                ordinal: bo,
            },
        ) => (ao, at, am).cmp(&(bo, bt, bm)),
        (Record(a), Record(b)) | (RecordRef(a), RecordRef(b)) => {
            (a.table_id, &a.table_name, a.handle).cmp(&(b.table_id, &b.table_name, b.handle))
        }
        (Variant(a), Variant(b)) => {
            return Step::Descend(vec![(**a).clone()], vec![(**b).clone()]);
        }
        (Array(a), Array(b)) => return Step::Descend(a.clone(), b.clone()),
        (List(a), List(b)) if a.same(b) => Ordering::Equal,
        (List(a), List(b)) => {
            if !met.insert((a.address(), b.address())) {
                return Step::Done(Ordering::Equal);
            }
            return Step::Descend(a.snapshot(), b.snapshot());
        }
        (Dict(a), Dict(b)) if a.same(b) => Ordering::Equal,
        (Dict(a), Dict(b)) => {
            if !met.insert((a.address(), b.address())) {
                return Step::Done(Ordering::Equal);
            }
            return Step::Descend(entries_in_order(a), entries_in_order(b));
        }
        (Blob(a), Blob(b)) => a.cmp(b),
        (ErrorInfo(a), ErrorInfo(b)) => a.message.cmp(&b.message),
        (
            Codeunit {
                object_name: a,
                instance: a_instance,
            },
            Codeunit {
                object_name: b,
                instance: b_instance,
            },
        ) => (a, a_instance).cmp(&(b, b_instance)),
        (
            Range {
                start: a_start,
                end: a_end,
            },
            Range {
                start: b_start,
                end: b_end,
            },
        ) => {
            return Step::Descend(
                vec![(**a_start).clone(), (**a_end).clone()],
                vec![(**b_start).clone(), (**b_end).clone()],
            );
        }
        // Different variants handled by the index check above.
        _ => Ordering::Equal,
    })
}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// The longest Text, Code or TextBuilder value the local test runtime builds,
/// in bytes of UTF-8.
///
/// Business Central's Text is a .NET string, which holds about a billion
/// characters. The local runtime runs tests inside the language server's
/// daemon, and the deadline is checked only between loop iterations, so a
/// test that doubles a text used to exhaust memory before the deadline
/// stopped it. 64 MiB is far more text than a test fixture needs, and a
/// doubling loop reaches it in 26 steps.
pub const MAX_TEXT_BYTES: usize = 64 * 1024 * 1024;

/// The most elements a List, Dictionary or array holds in the local test
/// runtime. Business Central refuses an array of more than 1,000,000
/// elements when it compiles it (AL0146), and does not cap a List or
/// Dictionary. The runtime uses the array limit for all three: a value takes
/// 56 bytes before any text it holds, so a full list of numbers takes 53 MiB.
pub const MAX_COLLECTION_LEN: usize = 1_000_000;

/// The most bytes the variables, Lists, Dictionaries, arrays, TextBuilders
/// and JSON values of one test hold together in the local test runtime.
///
/// [`MAX_TEXT_BYTES`] and [`MAX_COLLECTION_LEN`] bound one value, and a
/// test that copied a large text into many elements, variables or frames
/// took memory at a gigabyte a second. The runtime counts each List or
/// Dictionary element as it is added, the size of a `Value` and the bytes of
/// the text it holds, and takes it off when the element is removed or the
/// last handle to its List or Dictionary drops. A TextBuilder counts its
/// text. A variable counts the text it holds, and an array variable the size
/// of a `Value` for each element and the text each element holds, until the
/// variable takes another value or its frame drops: when its procedure
/// returns, or for a global when its codeunit instance goes. A JSON node
/// counts from when it is made until the test ends. An addition past this
/// total is an AL error.
///
/// The total is kept per thread, and the test runner runs each test on a
/// thread of its own.
pub const MAX_HELD_BYTES: usize = 256 * 1024 * 1024;

thread_local! {
    /// The bytes the test running on this thread holds, as
    /// [`MAX_HELD_BYTES`] counts them.
    static HELD_BYTES: Cell<usize> = const { Cell::new(0) };
}

/// The bytes the test running on this thread holds, as [`MAX_HELD_BYTES`]
/// counts them.
pub(crate) fn held_bytes() -> usize {
    HELD_BYTES.with(Cell::get)
}

/// Add `bytes` to the test's total, or refuse when the total would pass
/// [`MAX_HELD_BYTES`]. `operation` names what adds them, for the message.
pub(crate) fn hold_bytes(operation: &str, bytes: usize) -> Result<(), String> {
    let total = held_bytes().saturating_add(bytes);
    if bytes > 0 && total > MAX_HELD_BYTES {
        return Err(over_held_budget(operation, total));
    }
    HELD_BYTES.with(|held| held.set(total));
    Ok(())
}

/// Add `bytes` to the test's total without checking it.
pub(crate) fn add_held_bytes(bytes: usize) {
    HELD_BYTES.with(|held| held.set(held.get().saturating_add(bytes)));
}

/// Refuse when the test's total is past [`MAX_HELD_BYTES`].
pub(crate) fn check_held_bytes(operation: &str) -> Result<(), String> {
    let total = held_bytes();
    if total > MAX_HELD_BYTES {
        return Err(over_held_budget(operation, total));
    }
    Ok(())
}

/// Take `bytes` off the test's total.
pub(crate) fn release_held_bytes(bytes: usize) {
    HELD_BYTES.with(|held| held.set(held.get().saturating_sub(bytes)));
}

fn over_held_budget(operation: &str, total: usize) -> String {
    format!(
        "{operation} would make the variables, Lists, Dictionaries, arrays, TextBuilders and \
         JSON values of this test hold {total} bytes, over the local test runtime's limit of {} \
         MiB for one test",
        MAX_HELD_BYTES / (1024 * 1024)
    )
}

/// The bytes of text or data `value` owns, as [`MAX_HELD_BYTES`] counts a
/// variable or an array element. An array counts its elements and the text
/// they hold.
pub(crate) fn owned_bytes(value: &Value) -> usize {
    fn text_bytes(value: &Value) -> usize {
        match value {
            Value::Text(text) | Value::Code(text) | Value::Guid(text) => text.len(),
            Value::Blob(bytes) => bytes.len(),
            _ => 0,
        }
    }
    let mut value = value;
    while let Value::Variant(inner) = value {
        value = inner;
    }
    match value {
        Value::Array(items) => items
            .iter()
            .map(|item| std::mem::size_of::<Value>().saturating_add(text_bytes(item)))
            .sum(),
        other => text_bytes(other),
    }
}

/// The bytes `value` takes as an element of a List or Dictionary: the value
/// itself and what it owns. A List, Dictionary or TextBuilder element is a
/// handle, and its contents count on their own.
pub(crate) fn element_bytes(value: &Value) -> usize {
    std::mem::size_of::<Value>().saturating_add(owned_bytes(value))
}

/// The bytes one Dictionary entry takes: its normalised key text, key and
/// value.
pub(crate) fn entry_bytes(key_text: &str, key: &Value, value: &Value) -> usize {
    key_text
        .len()
        .saturating_add(element_bytes(key))
        .saturating_add(element_bytes(value))
}

/// Refuse a text of `bytes` bytes that is longer than [`MAX_TEXT_BYTES`].
/// `operation` names what would build it, for the message.
pub(crate) fn check_text_size(operation: &str, bytes: usize) -> Result<(), String> {
    if bytes <= MAX_TEXT_BYTES {
        return Ok(());
    }
    Err(format!(
        "{operation} would make a text of {bytes} bytes, over the local test runtime's \
         limit of {} MiB for one text",
        MAX_TEXT_BYTES / (1024 * 1024)
    ))
}

/// Refuse a List, Dictionary or array of `len` elements that is longer than
/// [`MAX_COLLECTION_LEN`]. `operation` names what would build it.
pub(crate) fn check_collection_len(operation: &str, len: usize) -> Result<(), String> {
    if len <= MAX_COLLECTION_LEN {
        return Ok(());
    }
    Err(format!(
        "{operation} would make {len} elements, over the local test runtime's limit of \
         {MAX_COLLECTION_LEN} elements for one List, Dictionary or array"
    ))
}

/// Reject a string that does not fit a declared `Text[N]`/`Code[N]` capacity.
/// BC traps this at the assignment rather than truncating, and counts
/// characters, not bytes. The message mirrors the server's.
pub(crate) fn check_string_capacity(value: &str, capacity: Option<usize>) -> Result<(), String> {
    let Some(capacity) = capacity else {
        return Ok(());
    };
    let length = value.chars().count();
    if length <= capacity {
        return Ok(());
    }
    Err(format!(
        "The length of the string is {length}, but it must be less than or equal to {capacity} \
         characters. Value: {value}"
    ))
}

/// Captured `Error()` / `asserterror` payload.
#[derive(Debug, Clone, PartialEq)]
pub struct ErrorInfo {
    pub message: String,
    /// Optional `ErrorType` enum member name (e.g. `Internal`).
    pub error_type: Option<String>,
    /// Optional source codeunit/procedure for diagnostic purposes.
    pub source: Option<String>,
}

impl Value {
    /// Move the values this value holds into `into`: an Array's elements, a
    /// Variant's value, and the contents of a List or Dictionary when this
    /// is the last handle to them. Drop uses it to free nested values in a
    /// loop.
    fn take_held_values(&mut self, into: &mut Vec<Value>) {
        match self {
            Value::List(list) => list.contents.take_values_if_last(into),
            Value::Dict(dict) => dict.contents.take_values_if_last(into),
            Value::Array(values) => into.append(values),
            Value::Variant(value) => into.push(std::mem::replace(&mut **value, Value::Null)),
            _ => {}
        }
    }

    /// Truthiness for AL `IF`/`WHILE` semantics: only `Boolean(true)` is
    /// truthy. Numbers, strings, etc. are NOT auto-coerced (AL is strict).
    pub fn is_truthy(&self) -> bool {
        matches!(self, Value::Boolean(true))
    }

    /// The i64 payload of an `Integer` or `BigInteger`, else `None`. Use this
    /// wherever a consumer treats the two integer types interchangeably
    /// (filters, field compares, coercions) rather than matching each variant.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Integer(n) | Value::BigInteger(n) => Some(*n),
            _ => None,
        }
    }

    /// Numeric view as an exact `Decimal`: `Integer`/`BigInteger` promote
    /// losslessly (i64 fits the 96-bit range), `Decimal` passes through, and
    /// everything else is `None`. The single conversion used by numeric
    /// comparison, FlowField aggregation, and the assert helpers.
    pub fn as_decimal(&self) -> Option<Decimal> {
        match self {
            Value::Integer(n) | Value::BigInteger(n) => Some(Decimal::from(*n)),
            Value::Decimal(d) => Some(*d),
            _ => None,
        }
    }

    /// Coerce `incoming` to the declared type of the `slot` it is being assigned
    /// into. AL variables have a fixed type, so an assignment preserves the
    /// slot's type rather than adopting the RHS's:
    ///
    /// * a `Code` slot uppercases a string RHS and stays caseless `Code`;
    /// * an `Integer`/`BigInteger` slot keeps its width so later arithmetic uses
    ///   the right overflow trap — narrowing an out-of-range value into an
    ///   `Integer` slot is a runtime error, matching BC's overflow trap;
    /// * a `Decimal` slot promotes an integer RHS to `Decimal`, so later
    ///   `div`/`mod` correctly reject the slot as Decimal.
    ///
    /// `capacity` is the slot's declared `Text[N]`/`Code[N]` length. A string
    /// longer than it is a runtime error, the way BC traps an assignment whose
    /// converted value overflows the target.
    ///
    /// Any other combination overwrites as-is.
    pub(crate) fn coerce_into_slot(
        slot: &Value,
        incoming: Value,
        capacity: Option<usize>,
    ) -> Result<Value, String> {
        Ok(match (slot, &incoming) {
            // A Code value is uppercased and carries no leading or trailing
            // spaces, so its length is measured after trimming.
            (Value::Code(_), Value::Text(s) | Value::Code(s)) => {
                let trimmed = s.trim();
                check_string_capacity(trimmed, capacity)?;
                Value::Code(trimmed.to_uppercase())
            }
            (Value::Text(_), Value::Text(s) | Value::Code(s)) => {
                check_string_capacity(s, capacity)?;
                Value::Text(s.clone())
            }
            (Value::BigInteger(_), Value::Integer(n) | Value::BigInteger(n)) => {
                Value::BigInteger(*n)
            }
            (Value::Integer(_), Value::Integer(n) | Value::BigInteger(n)) => {
                if !(i32::MIN as i64..=i32::MAX as i64).contains(n) {
                    return Err(format!("value {n} is outside the Integer range"));
                }
                Value::Integer(*n)
            }
            (Value::Decimal(_), Value::Integer(n) | Value::BigInteger(n)) => {
                Value::Decimal(Decimal::from(*n))
            }
            _ => incoming,
        })
    }

    /// Default value for the named AL type. Returns `None` if the type
    /// name is unknown to the interpreter.
    pub fn default_for(type_name: &str) -> Option<Value> {
        match type_name.to_ascii_lowercase().as_str() {
            "integer" => Some(Value::Integer(0)),
            "biginteger" => Some(Value::BigInteger(0)),
            "decimal" => Some(Value::Decimal(Decimal::ZERO)),
            "boolean" => Some(Value::Boolean(false)),
            "text" => Some(Value::Text(String::new())),
            "code" => Some(Value::Code(String::new())),
            "date" => Some(Value::Date(0)),
            "time" => Some(Value::Time(0)),
            "datetime" => Some(Value::DateTime(0)),
            "duration" => Some(Value::Duration(0)),
            // Braced, as Format shows a Guid and CreateGuid returns one.
            "guid" => Some(Value::Guid("{00000000-0000-0000-0000-000000000000}".into())),
            "char" => Some(Value::Char('\0')),
            "textbuilder" => Some(Value::text_builder(String::new())),
            other => crate::interpreter::json::default_for(other),
        }
    }

    /// A new `List` holding `items`, with no declared element type.
    pub fn list(items: Vec<Value>) -> Value {
        Value::List(Collection::new(items, None))
    }

    /// A new `List of [member_type]` holding `items`, for a method whose
    /// return type is a typed list, so the list converts what it is given.
    pub fn typed_list(member_type: &str, items: Vec<Value>) -> Value {
        Value::List(Collection::new(items, Some(member_type)))
    }

    /// A new `List of [Text]` holding `items`, as `Text.Split`,
    /// `Enum.Names()` and `JsonObject.Keys()` return.
    pub fn text_list(items: Vec<Value>) -> Value {
        Value::typed_list("Text", items)
    }

    /// A new `List of [JsonToken]` holding `items`, as `JsonObject.Values()`
    /// returns.
    pub fn json_token_list(items: Vec<Value>) -> Value {
        Value::typed_list("JsonToken", items)
    }

    /// A new `TextBuilder` holding `text`.
    pub fn text_builder(text: String) -> Value {
        Value::TextBuilder(Shared::new(text))
    }

    /// A new `Dictionary` holding `entries`, with no declared key type.
    pub fn dict(entries: DictEntries) -> Value {
        Value::Dict(Collection::new(entries, None))
    }

    /// Short type-name for diagnostic output. Stable identifiers; do not
    /// depend on these for parsing.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "Null",
            Value::Empty => "Empty",
            Value::Integer(_) => "Integer",
            Value::BigInteger(_) => "BigInteger",
            Value::Decimal(_) => "Decimal",
            Value::Boolean(_) => "Boolean",
            Value::Char(_) => "Char",
            Value::Text(_) => "Text",
            Value::Code(_) => "Code",
            Value::TextBuilder(_) => "TextBuilder",
            Value::Json(json) => match json.kind {
                crate::interpreter::json::JsonKind::Object => "JsonObject",
                crate::interpreter::json::JsonKind::Array => "JsonArray",
                crate::interpreter::json::JsonKind::Token => "JsonToken",
                crate::interpreter::json::JsonKind::Value => "JsonValue",
            },
            Value::Date(_) => "Date",
            Value::Time(_) => "Time",
            Value::DateTime(_) => "DateTime",
            Value::Duration(_) => "Duration",
            Value::Guid(_) => "Guid",
            Value::Option { .. } => "Option",
            Value::Record(_) => "Record",
            Value::RecordRef(_) => "RecordRef",
            Value::Variant(_) => "Variant",
            Value::Array(_) => "Array",
            Value::List(_) => "List",
            Value::Dict(_) => "Dict",
            Value::Blob(_) => "Blob",
            Value::ErrorInfo(_) => "ErrorInfo",
            Value::Codeunit { .. } => "Codeunit",
            Value::Range { .. } => "Range",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use std::collections::BTreeMap;

    #[test]
    fn truthiness_is_strict() {
        assert!(Value::Boolean(true).is_truthy());
        assert!(!Value::Boolean(false).is_truthy());
        assert!(!Value::Integer(1).is_truthy());
        assert!(!Value::Text("anything".into()).is_truthy());
        assert!(!Value::Null.is_truthy());
    }

    #[test]
    fn default_for_returns_zero_value() {
        assert_eq!(Value::default_for("Integer"), Some(Value::Integer(0)));
        assert_eq!(Value::default_for("integer"), Some(Value::Integer(0)));
        assert_eq!(Value::default_for("Boolean"), Some(Value::Boolean(false)));
        assert!(matches!(Value::default_for("Text"), Some(Value::Text(s)) if s.is_empty()));
    }

    #[test]
    fn default_for_unknown_returns_none() {
        assert!(Value::default_for("DefinitelyNotAnALType").is_none());
        assert!(Value::default_for("").is_none());
    }

    #[test]
    fn type_name_is_stable() {
        assert_eq!(Value::Integer(0).type_name(), "Integer");
        assert_eq!(Value::Boolean(true).type_name(), "Boolean");
        assert_eq!(
            Value::Option {
                type_name: "S".into(),
                member: "X".into(),
                ordinal: 0
            }
            .type_name(),
            "Option"
        );
    }

    #[test]
    fn default_for_code_returns_code_variant() {
        assert!(
            matches!(Value::default_for("Code"), Some(Value::Code(s)) if s.is_empty()),
            "default_for(\"Code\") must return Value::Code, got: {:?}",
            Value::default_for("Code")
        );
    }

    #[test]
    fn civil_days_round_trip_over_wide_range() {
        // Property-style round trip: civil_from_days is the exact inverse of
        // days_from_civil. Step through a wide day range (covering leap
        // centuries, negative years, and both era branches) plus a few named
        // anchors, and require days -> (y, m, d) -> days to be the identity
        // with in-range month/day parts.
        let anchors = [
            days_from_civil(2024, 2, 29), // leap day
            -AL_EPOCH_TO_UNIX_DAYS,       // AL epoch 0001-01-01
            0,                            // Unix epoch 1970-01-01
        ];
        let stepped = (-1_000_000..=1_000_000).step_by(1_237);
        for days in stepped.chain(anchors) {
            let (year, month, day) = civil_from_days(days);
            assert!((1..=12).contains(&month), "bad month {month} for {days}");
            assert!((1..=31).contains(&day), "bad day {day} for {days}");
            assert_eq!(
                days_from_civil(year, month, day),
                days,
                "round trip failed for day {days} ({year:04}-{month:02}-{day:02})"
            );
        }
        // The named anchors also decode to the expected civil dates.
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2, 29));
        assert_eq!(civil_from_days(-AL_EPOCH_TO_UNIX_DAYS), (1, 1, 1));
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    #[test]
    fn decimal_is_exact_no_binary_float_drift() {
        assert_eq!(
            Value::Decimal(dec!(0.1) + dec!(0.2)),
            Value::Decimal(dec!(0.3)),
            "0.1 + 0.2 must equal 0.3 exactly"
        );
        let one = Value::Decimal(dec!(1.0));
        let two = Value::Decimal(dec!(2.0));
        assert!(one < two);
        assert_eq!(
            one.cmp(&Value::Decimal(dec!(1.0))),
            std::cmp::Ordering::Equal
        );
    }

    #[test]
    fn default_for_covers_every_supported_type() {
        use std::cmp::Ordering;
        assert_eq!(Value::default_for("biginteger"), Some(Value::BigInteger(0)));
        assert_eq!(Value::BigInteger(0), Value::Integer(0));
        assert_eq!(
            Value::default_for("decimal"),
            Some(Value::Decimal(Decimal::ZERO))
        );
        assert_eq!(Value::default_for("date"), Some(Value::Date(0)));
        assert_eq!(Value::default_for("time"), Some(Value::Time(0)));
        assert_eq!(Value::default_for("datetime"), Some(Value::DateTime(0)));
        assert_eq!(Value::default_for("duration"), Some(Value::Duration(0)));
        assert_eq!(Value::default_for("char"), Some(Value::Char('\0')));
        assert!(matches!(Value::default_for("code"), Some(Value::Code(s)) if s.is_empty()));
        match Value::default_for("guid") {
            Some(Value::Guid(g)) => {
                assert_eq!(g, "{00000000-0000-0000-0000-000000000000}");
            }
            other => panic!("guid default wrong: {other:?}"),
        }
        assert_eq!(
            Value::default_for("DateTime").cmp(&Value::default_for("datetime")),
            Ordering::Equal
        );
    }

    #[test]
    fn cross_variant_order_follows_declaration_index() {
        let huge_int = Value::Integer(i64::MAX);
        let tiny_dec = Value::Decimal(dec!(-1000000));
        assert!(
            huge_int < tiny_dec,
            "Integer variant precedes Decimal variant"
        );

        let ascending = vec![
            Value::Null,
            Value::Empty,
            Value::Integer(0),
            Value::Decimal(Decimal::ZERO),
            Value::Boolean(false),
            Value::Char('\0'),
            Value::Text(String::new()),
            Value::Code(String::new()),
            Value::Date(0),
            Value::Time(0),
            Value::DateTime(0),
            Value::Duration(0),
            Value::Guid(String::new()),
            Value::Option {
                type_name: String::new(),
                member: String::new(),
                ordinal: 0,
            },
            Value::Record(RecordValue {
                table_name: String::new(),
                table_id: 0,
                handle: None,
                temporary: false,
            }),
            Value::RecordRef(RecordValue {
                table_name: String::new(),
                table_id: 0,
                handle: None,
                temporary: false,
            }),
            Value::Variant(Box::new(Value::Null)),
            Value::Array(vec![]),
            Value::list(vec![]),
            Value::dict(DictEntries::new()),
            Value::Blob(vec![]),
            Value::ErrorInfo(Box::new(ErrorInfo {
                message: String::new(),
                error_type: None,
                source: None,
            })),
        ];
        for win in ascending.windows(2) {
            assert!(
                win[0] < win[1],
                "{} must sort before {}",
                win[0].type_name(),
                win[1].type_name()
            );
        }
    }

    #[test]
    fn within_variant_scalar_ordering() {
        assert!(Value::Integer(-5) < Value::Integer(5));
        assert!(Value::Decimal(dec!(1.5)) < Value::Decimal(dec!(2.5)));
        assert!(Value::Boolean(false) < Value::Boolean(true));
        assert!(Value::Char('a') < Value::Char('z'));
        assert!(Value::Text("apple".into()) < Value::Text("banana".into()));
        assert!(Value::Code("AAA".into()) < Value::Code("AAB".into()));
        assert!(Value::Date(1) < Value::Date(2));
        assert!(Value::Time(100) < Value::Time(200));
        assert!(Value::DateTime(10) < Value::DateTime(20));
        assert!(Value::Duration(-1) < Value::Duration(1));
        assert!(Value::Guid("a".into()) < Value::Guid("b".into()));
    }

    #[test]
    fn option_orders_by_ordinal_first() {
        let opt = |t: &str, m: &str, o: i64| Value::Option {
            type_name: t.into(),
            member: m.into(),
            ordinal: o,
        };
        assert!(opt("Status", "Zzz", 0) < opt("Status", "Aaa", 1));
        assert!(opt("AStatus", "X", 5) < opt("BStatus", "X", 5));
        assert!(opt("Status", "Aaa", 5) < opt("Status", "Bbb", 5));
        assert_eq!(opt("S", "M", 3), opt("S", "M", 3));
    }

    #[test]
    fn record_orders_by_table_id_then_name_then_handle() {
        let rec = |id: i32, name: &str, handle: Option<u64>| {
            Value::Record(RecordValue {
                table_name: name.into(),
                table_id: id,
                handle,
                temporary: false,
            })
        };
        assert!(rec(18, "Zebra", Some(99)) < rec(27, "Aardvark", Some(1)));
        assert!(rec(18, "Apple", None) < rec(18, "Banana", None));
        assert!(rec(18, "Customer", None) < rec(18, "Customer", Some(0)));
        assert!(rec(18, "Customer", Some(1)) < rec(18, "Customer", Some(2)));
        let rref = |id: i32| {
            Value::RecordRef(RecordValue {
                table_name: "T".into(),
                table_id: id,
                handle: None,
                temporary: false,
            })
        };
        assert!(rref(1) < rref(2));
    }

    #[test]
    fn structured_collection_ordering() {
        assert!(
            Value::Variant(Box::new(Value::Integer(1)))
                < Value::Variant(Box::new(Value::Integer(2)))
        );
        assert!(
            Value::Array(vec![Value::Integer(1)])
                < Value::Array(vec![Value::Integer(1), Value::Integer(0)])
        );
        assert!(Value::list(vec![Value::Integer(1)]) < Value::list(vec![Value::Integer(2)]));
        assert!(Value::Blob(vec![1, 2]) < Value::Blob(vec![1, 3]));
        assert!(Value::Blob(vec![1]) < Value::Blob(vec![1, 0]));
    }

    #[test]
    fn dict_ordering_compares_entry_sequences() {
        let key = |text: &str| Value::Text(text.to_string());
        let mut a = DictEntries::new();
        a.insert("k1".to_string(), (key("k1"), Value::Integer(1)));
        let mut b = DictEntries::new();
        b.insert("k1".to_string(), (key("k1"), Value::Integer(2)));
        assert!(Value::dict(a.clone()) < Value::dict(b));
        let mut c = a.clone();
        c.insert("k2".to_string(), (key("k2"), Value::Integer(0)));
        assert!(Value::dict(a) < Value::dict(c));
    }

    #[test]
    fn error_info_orders_by_message() {
        let err = |msg: &str| {
            Value::ErrorInfo(Box::new(ErrorInfo {
                message: msg.into(),
                error_type: Some("Internal".into()),
                source: Some("CU 50000".into()),
            }))
        };
        assert!(err("aaa") < err("bbb"));
        assert_eq!(err("same"), err("same"));
    }

    #[test]
    fn eq_mirrors_cmp_across_variants() {
        let d = Value::Decimal(dec!(1.25));
        assert_eq!(d, d.clone(), "a decimal must equal itself");
        assert_ne!(Value::Integer(0), Value::Decimal(Decimal::ZERO));
        assert_eq!(
            Value::Integer(1).partial_cmp(&Value::Integer(2)),
            Some(std::cmp::Ordering::Less)
        );
    }

    #[test]
    fn type_name_covers_structured_variants() {
        assert_eq!(Value::Null.type_name(), "Null");
        assert_eq!(Value::Empty.type_name(), "Empty");
        assert_eq!(Value::Decimal(Decimal::ZERO).type_name(), "Decimal");
        assert_eq!(Value::Char('x').type_name(), "Char");
        assert_eq!(Value::Text(String::new()).type_name(), "Text");
        assert_eq!(Value::Code(String::new()).type_name(), "Code");
        assert_eq!(Value::Date(0).type_name(), "Date");
        assert_eq!(Value::Time(0).type_name(), "Time");
        assert_eq!(Value::DateTime(0).type_name(), "DateTime");
        assert_eq!(Value::Duration(0).type_name(), "Duration");
        assert_eq!(Value::Guid(String::new()).type_name(), "Guid");
        assert_eq!(Value::Variant(Box::new(Value::Null)).type_name(), "Variant");
        assert_eq!(Value::Array(vec![]).type_name(), "Array");
        assert_eq!(Value::list(vec![]).type_name(), "List");
        assert_eq!(Value::dict(DictEntries::new()).type_name(), "Dict");
        assert_eq!(Value::Blob(vec![]).type_name(), "Blob");
        assert_eq!(
            Value::Record(RecordValue {
                table_name: String::new(),
                table_id: 0,
                handle: None,
                temporary: false,
            })
            .type_name(),
            "Record"
        );
        assert_eq!(
            Value::RecordRef(RecordValue {
                table_name: String::new(),
                table_id: 0,
                handle: None,
                temporary: false,
            })
            .type_name(),
            "RecordRef"
        );
        assert_eq!(
            Value::ErrorInfo(Box::new(ErrorInfo {
                message: String::new(),
                error_type: None,
                source: None,
            }))
            .type_name(),
            "ErrorInfo"
        );
    }

    #[test]
    fn al_date_math_known_anchors() {
        assert_eq!(al_days_from_ymd(1, 1, 1), 0);
        assert_eq!(al_days_from_ymd(1970, 1, 1), AL_EPOCH_TO_UNIX_DAYS);
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1, 1, 1), -AL_EPOCH_TO_UNIX_DAYS);
    }

    #[test]
    fn al_date_math_month_lengths_and_leap_years() {
        assert_eq!(
            al_days_from_ymd(2024, 7, 1) - al_days_from_ymd(2024, 6, 1),
            30
        );
        assert_eq!(
            al_days_from_ymd(2024, 3, 1) - al_days_from_ymd(2024, 2, 1),
            29
        );
        assert_eq!(
            al_days_from_ymd(2023, 3, 1) - al_days_from_ymd(2023, 2, 1),
            28
        );
        assert_eq!(
            al_days_from_ymd(2024, 1, 1) - al_days_from_ymd(2023, 1, 1),
            365
        );
    }

    #[test]
    fn value_is_usable_as_btreemap_key() {
        let mut map: BTreeMap<Value, &str> = BTreeMap::new();
        map.insert(Value::Integer(2), "two");
        map.insert(Value::Integer(1), "one");
        map.insert(Value::Decimal(dec!(3.5)), "dec");
        assert_eq!(map.get(&Value::Integer(1)), Some(&"one"));
        assert_eq!(map.get(&Value::Decimal(dec!(3.5))), Some(&"dec"));
        let keys: Vec<_> = map.keys().cloned().collect();
        assert!(keys[0] < keys[1]);
    }

    /// A chain `depth` levels deep around `Integer(end)`: each level a List,
    /// or with `dictionaries` every other level a Dictionary.
    fn nested(depth: usize, end: i64, dictionaries: bool) -> Value {
        let mut value = Value::list(vec![Value::Integer(end)]);
        for level in 0..depth {
            value = if dictionaries && level % 2 == 0 {
                let mut entries = DictEntries::new();
                entries.insert("1".to_string(), (Value::Integer(1), value));
                Value::dict(entries)
            } else {
                Value::list(vec![value])
            };
        }
        value
    }

    /// Run `body` on a thread with the interpreter's stack, as the test
    /// backend runs an AL body.
    fn on_interpreter_stack<T: Send + 'static>(body: impl FnOnce() -> T + Send + 'static) -> T {
        std::thread::Builder::new()
            .stack_size(crate::interpreter::dispatch::INTERP_STACK_BYTES)
            .spawn(body)
            .expect("spawn the interpreter thread")
            .join()
            .expect("the interpreter thread returns")
    }

    /// `compare` recursed once per level, and two chains 40,000 deep
    /// overflowed the interpreter's stack in a debug build and aborted the
    /// process.
    #[test]
    fn chains_of_collections_100000_deep_compare() {
        on_interpreter_stack(|| {
            for dictionaries in [false, true] {
                let same = nested(100_000, 1, dictionaries);
                assert_eq!(same, nested(100_000, 1, dictionaries));
                assert!(same < nested(100_000, 2, dictionaries));
            }
        });
    }

    /// Dropping the last handle to a chain dropped one level per native
    /// frame, and a chain 210,000 deep overflowed the interpreter's stack in
    /// a debug build and aborted the process.
    #[test]
    fn chains_of_collections_300000_deep_drop() {
        on_interpreter_stack(|| {
            drop(nested(300_000, 1, false));
            drop(nested(300_000, 1, true));
        });
    }
}
