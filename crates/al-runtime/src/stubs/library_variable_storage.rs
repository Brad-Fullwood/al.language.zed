//! Library Variable Storage (codeunit 131004) — native Rust port.
//!
//! AL source for the real codeunit lives in BCApps at:
//!   `src/Tools/Test Framework/Test Libraries/Variable Storage/src/LibraryVariableStorage.Codeunit.al`
//!
//! This stub uses the same fast-path approach as `library_assert`: the dispatch
//! layer recognises calls to Library Variable Storage procedure names and invokes
//! equivalent Rust code here, without interpreting the AL source.
//!
//! ## State model
//!
//! AL's `Library - Variable Storage` is a stateful codeunit — it owns a
//! circular queue of up to 25 `Variant` values plus three integer counters
//! (`StartIndex`, `EndIndex`, `TotalCount`). Because `StubFn` is a plain
//! function pointer (`fn(&[Value]) -> Eval`) that carries no instance
//! context, the queue is stored in a **thread-local** `RefCell<LvsQueue>`.
//!
//! Consequences:
//! - One queue per thread per test run — correct for the typical BC test
//!   pattern where a single procedure enqueues values and modal handlers
//!   dequeue them (all on one OS thread in the interpreter).
//! - Tests must not rely on queue state persisting across threads.
//! - Call `Clear` (or `AssertEmpty`) at the start of each test to ensure a
//!   clean slate, matching BC best-practice.
//!
//! ## Procedure coverage
//!
//! All public procedures from the BC source are covered:
//! `AssertEmpty`, `AssertFull`, `AssertNotOverflow`, `AssertNotUnderflow`,
//! `AssertPeekAvailable`, `Clear`, `Dequeue`, `Peek`, `Enqueue`, `Length`,
//! `MaxLength`, `DequeueText`, `DequeueDecimal`, `DequeueInteger`,
//! `DequeueDate`, `DequeueDateTime`, `DequeueTime`, `DequeueBoolean`,
//! `PeekText`, `PeekDecimal`, `PeekInteger`, `PeekDate`, `PeekTime`,
//! `PeekBoolean`.

use std::cell::RefCell;
use std::collections::VecDeque;

use crate::interpreter::scope::Eval;
use crate::interpreter::value::{ErrorInfo, Value};

/// Maximum items the queue can hold (mirrors `array[25] of Variant`).
const MAX_QUEUE_SIZE: usize = 25;

struct LvsQueue {
    items: VecDeque<Value>,
}

impl LvsQueue {
    fn new() -> Self {
        Self {
            items: VecDeque::with_capacity(MAX_QUEUE_SIZE),
        }
    }

    fn len(&self) -> usize {
        self.items.len()
    }

    fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    fn is_full(&self) -> bool {
        self.items.len() >= MAX_QUEUE_SIZE
    }

    fn enqueue(&mut self, v: Value) -> Result<(), &'static str> {
        if self.items.len() >= MAX_QUEUE_SIZE {
            return Err("Queue overflow.");
        }
        self.items.push_back(v);
        Ok(())
    }

    fn dequeue(&mut self) -> Result<Value, &'static str> {
        self.items.pop_front().ok_or("Queue underflow.")
    }

    /// 1-based peek (mirrors AL `Peek(var V; Index: Integer)`).
    fn peek(&self, index: usize) -> Result<&Value, &'static str> {
        if index == 0 || index > self.items.len() {
            return Err("Index out of bounds.");
        }
        // Safety: bounds checked above.
        Ok(&self.items[index - 1])
    }

    fn clear(&mut self) {
        self.items.clear();
    }
}

thread_local! {
    static QUEUE: RefCell<LvsQueue> = RefCell::new(LvsQueue::new());
}

/// Reset the thread-local queue to empty. Exposed for test isolation.
pub fn reset_queue() {
    QUEUE.with(|q| q.borrow_mut().clear());
}

fn err(message: impl Into<String>) -> Eval {
    Eval::Error(ErrorInfo {
        message: message.into(),
        error_type: None,
        source: Some("Library Variable Storage".to_string()),
    })
}

fn ok(v: Value) -> Eval {
    Eval::Normal(v)
}

fn ok_empty() -> Eval {
    Eval::Normal(Value::Empty)
}

/// `AssertEmpty()` — error if the queue is not empty; also clears the queue
/// (matches BC behaviour: counts items, clears, then asserts count was 0).
pub fn assert_empty(_args: &[Value]) -> Eval {
    QUEUE.with(|q| {
        let mut queue = q.borrow_mut();
        let count = queue.len();
        queue.clear();
        if count != 0 {
            err(format!(
                "Library Variable Storage: AssertEmpty failed — queue had {count} item(s). Queue is not empty."
            ))
        } else {
            ok_empty()
        }
    })
}

pub fn assert_full(_args: &[Value]) -> Eval {
    QUEUE.with(|q| {
        if q.borrow().is_full() {
            ok_empty()
        } else {
            err(format!(
                "Library Variable Storage: AssertFull failed — queue has {}/{} items. Queue is empty.",
                q.borrow().len(),
                MAX_QUEUE_SIZE
            ))
        }
    })
}

pub fn assert_not_overflow(_args: &[Value]) -> Eval {
    QUEUE.with(|q| {
        let len = q.borrow().len();
        if len + 1 > MAX_QUEUE_SIZE {
            err("Library Variable Storage: Queue overflow.")
        } else {
            ok_empty()
        }
    })
}

pub fn assert_not_underflow(_args: &[Value]) -> Eval {
    QUEUE.with(|q| {
        if q.borrow().is_empty() {
            err("Library Variable Storage: Queue underflow.")
        } else {
            ok_empty()
        }
    })
}

pub fn assert_peek_available(args: &[Value]) -> Eval {
    let index = match args {
        [Value::Integer(i)] => *i,
        _ => return err("Library Variable Storage: AssertPeekAvailable expects (Integer)"),
    };
    QUEUE.with(|q| {
        let len = q.borrow().len() as i64;
        if index <= 0 || index > len {
            err(format!(
                "Library Variable Storage: Index out of bounds (index={index}, count={len})."
            ))
        } else {
            ok_empty()
        }
    })
}

pub fn clear(_args: &[Value]) -> Eval {
    QUEUE.with(|q| {
        q.borrow_mut().clear();
        ok_empty()
    })
}

pub fn enqueue(args: &[Value]) -> Eval {
    let value = match args {
        [v] => v.clone(),
        _ => return err("Library Variable Storage: Enqueue expects (Variant)"),
    };
    QUEUE.with(|q| match q.borrow_mut().enqueue(value) {
        Ok(()) => ok_empty(),
        Err(msg) => err(format!("Library Variable Storage: {msg}")),
    })
}

/// `Dequeue(var Variant: Variant)` — remove the front value and return it.
///
/// Note: in AL this is an `out`/`var` parameter. In the stub, we return the
/// dequeued value as the `Eval::Normal` payload so the interpreter can store
/// it in the caller's variable.
pub fn dequeue(_args: &[Value]) -> Eval {
    QUEUE.with(|q| match q.borrow_mut().dequeue() {
        Ok(v) => ok(v),
        Err(msg) => err(format!("Library Variable Storage: {msg}")),
    })
}

/// `Peek(var Variant: Variant; Index: Integer)` — read without removing.
///
/// Returns the value at the 1-based `Index` position.
pub fn peek(args: &[Value]) -> Eval {
    let index = match args {
        [Value::Integer(i)] => *i as usize,
        // Allow (_, index) for callers that pass a placeholder variant + index.
        [_, Value::Integer(i)] => *i as usize,
        _ => return err("Library Variable Storage: Peek expects (Variant, Integer)"),
    };
    QUEUE.with(|q| match q.borrow().peek(index) {
        Ok(v) => ok(v.clone()),
        Err(msg) => err(format!("Library Variable Storage: {msg}")),
    })
}

pub fn length(_args: &[Value]) -> Eval {
    QUEUE.with(|q| ok(Value::Integer(q.borrow().len() as i64)))
}

pub fn max_length(_args: &[Value]) -> Eval {
    ok(Value::Integer(MAX_QUEUE_SIZE as i64))
}

/// `DequeueText(): Text` — dequeue and coerce to Text via Format().
pub fn dequeue_text(_args: &[Value]) -> Eval {
    QUEUE.with(|q| match q.borrow_mut().dequeue() {
        Ok(v) => ok(Value::Text(format_value(&v))),
        Err(msg) => err(format!("Library Variable Storage: {msg}")),
    })
}

pub fn dequeue_decimal(_args: &[Value]) -> Eval {
    QUEUE.with(|q| match q.borrow_mut().dequeue() {
        Ok(Value::Decimal(d)) => ok(Value::Decimal(d)),
        Ok(Value::Integer(i)) => ok(Value::Decimal(i as f64)),
        Ok(v) => err(format!(
            "Library Variable Storage: DequeueDecimal type mismatch — got {}",
            v.type_name()
        )),
        Err(msg) => err(format!("Library Variable Storage: {msg}")),
    })
}

pub fn dequeue_integer(_args: &[Value]) -> Eval {
    QUEUE.with(|q| match q.borrow_mut().dequeue() {
        Ok(Value::Integer(i)) => ok(Value::Integer(i)),
        Ok(v) => err(format!(
            "Library Variable Storage: DequeueInteger type mismatch — got {}",
            v.type_name()
        )),
        Err(msg) => err(format!("Library Variable Storage: {msg}")),
    })
}

pub fn dequeue_date(_args: &[Value]) -> Eval {
    QUEUE.with(|q| match q.borrow_mut().dequeue() {
        Ok(Value::Date(d)) => ok(Value::Date(d)),
        Ok(v) => err(format!(
            "Library Variable Storage: DequeueDate type mismatch — got {}",
            v.type_name()
        )),
        Err(msg) => err(format!("Library Variable Storage: {msg}")),
    })
}

pub fn dequeue_date_time(_args: &[Value]) -> Eval {
    QUEUE.with(|q| match q.borrow_mut().dequeue() {
        Ok(Value::DateTime(dt)) => ok(Value::DateTime(dt)),
        Ok(v) => err(format!(
            "Library Variable Storage: DequeueDateTime type mismatch — got {}",
            v.type_name()
        )),
        Err(msg) => err(format!("Library Variable Storage: {msg}")),
    })
}

pub fn dequeue_time(_args: &[Value]) -> Eval {
    QUEUE.with(|q| match q.borrow_mut().dequeue() {
        Ok(Value::Time(t)) => ok(Value::Time(t)),
        Ok(v) => err(format!(
            "Library Variable Storage: DequeueTime type mismatch — got {}",
            v.type_name()
        )),
        Err(msg) => err(format!("Library Variable Storage: {msg}")),
    })
}

pub fn dequeue_boolean(_args: &[Value]) -> Eval {
    QUEUE.with(|q| match q.borrow_mut().dequeue() {
        Ok(Value::Boolean(b)) => ok(Value::Boolean(b)),
        Ok(v) => err(format!(
            "Library Variable Storage: DequeueBoolean type mismatch — got {}",
            v.type_name()
        )),
        Err(msg) => err(format!("Library Variable Storage: {msg}")),
    })
}

pub fn peek_text(args: &[Value]) -> Eval {
    let index = match args {
        [Value::Integer(i)] => *i as usize,
        _ => return err("Library Variable Storage: PeekText expects (Integer)"),
    };
    QUEUE.with(|q| match q.borrow().peek(index) {
        Ok(v) => ok(Value::Text(format_value(v))),
        Err(msg) => err(format!("Library Variable Storage: {msg}")),
    })
}

pub fn peek_decimal(args: &[Value]) -> Eval {
    let index = match args {
        [Value::Integer(i)] => *i as usize,
        _ => return err("Library Variable Storage: PeekDecimal expects (Integer)"),
    };
    QUEUE.with(|q| match q.borrow().peek(index) {
        Ok(Value::Decimal(d)) => ok(Value::Decimal(*d)),
        Ok(Value::Integer(i)) => ok(Value::Decimal(*i as f64)),
        Ok(v) => err(format!(
            "Library Variable Storage: PeekDecimal type mismatch — got {}",
            v.type_name()
        )),
        Err(msg) => err(format!("Library Variable Storage: {msg}")),
    })
}

pub fn peek_integer(args: &[Value]) -> Eval {
    let index = match args {
        [Value::Integer(i)] => *i as usize,
        _ => return err("Library Variable Storage: PeekInteger expects (Integer)"),
    };
    QUEUE.with(|q| match q.borrow().peek(index) {
        Ok(Value::Integer(i)) => ok(Value::Integer(*i)),
        Ok(v) => err(format!(
            "Library Variable Storage: PeekInteger type mismatch — got {}",
            v.type_name()
        )),
        Err(msg) => err(format!("Library Variable Storage: {msg}")),
    })
}

pub fn peek_date(args: &[Value]) -> Eval {
    let index = match args {
        [Value::Integer(i)] => *i as usize,
        _ => return err("Library Variable Storage: PeekDate expects (Integer)"),
    };
    QUEUE.with(|q| match q.borrow().peek(index) {
        Ok(Value::Date(d)) => ok(Value::Date(*d)),
        Ok(v) => err(format!(
            "Library Variable Storage: PeekDate type mismatch — got {}",
            v.type_name()
        )),
        Err(msg) => err(format!("Library Variable Storage: {msg}")),
    })
}

pub fn peek_time(args: &[Value]) -> Eval {
    let index = match args {
        [Value::Integer(i)] => *i as usize,
        _ => return err("Library Variable Storage: PeekTime expects (Integer)"),
    };
    QUEUE.with(|q| match q.borrow().peek(index) {
        Ok(Value::Time(t)) => ok(Value::Time(*t)),
        Ok(v) => err(format!(
            "Library Variable Storage: PeekTime type mismatch — got {}",
            v.type_name()
        )),
        Err(msg) => err(format!("Library Variable Storage: {msg}")),
    })
}

pub fn peek_boolean(args: &[Value]) -> Eval {
    let index = match args {
        [Value::Integer(i)] => *i as usize,
        _ => return err("Library Variable Storage: PeekBoolean expects (Integer)"),
    };
    QUEUE.with(|q| match q.borrow().peek(index) {
        Ok(Value::Boolean(b)) => ok(Value::Boolean(*b)),
        Ok(v) => err(format!(
            "Library Variable Storage: PeekBoolean type mismatch — got {}",
            v.type_name()
        )),
        Err(msg) => err(format!("Library Variable Storage: {msg}")),
    })
}

fn format_value(v: &Value) -> String {
    match v {
        Value::Integer(n) => n.to_string(),
        Value::Decimal(n) => n.to_string(),
        Value::Boolean(true) => "Yes".to_string(),
        Value::Boolean(false) => "No".to_string(),
        Value::Text(s) | Value::Code(s) => s.clone(),
        Value::Date(d) => d.to_string(),
        Value::Time(t) => t.to_string(),
        Value::DateTime(dt) => dt.to_string(),
        Value::Duration(d) => d.to_string(),
        Value::Guid(g) => g.clone(),
        Value::Char(c) => c.to_string(),
        Value::Null | Value::Empty => String::new(),
        other => format!("<{}>", other.type_name()),
    }
}

/// Resolve a procedure name (case-insensitive) to its Rust implementation.
///
/// Returns `None` if the name is unknown to this catalog.
pub fn resolve(procedure: &str) -> Option<fn(&[Value]) -> Eval> {
    match procedure.to_ascii_lowercase().as_str() {
        "assertempty" => Some(assert_empty),
        "assertfull" => Some(assert_full),
        "assertnotoverflow" => Some(assert_not_overflow),
        "assertnotunderflow" => Some(assert_not_underflow),
        "assertpeekavailable" => Some(assert_peek_available),
        "clear" => Some(clear),
        "enqueue" => Some(enqueue),
        "dequeue" => Some(dequeue),
        "peek" => Some(peek),
        "length" => Some(length),
        "maxlength" => Some(max_length),
        "dequeuetext" => Some(dequeue_text),
        "dequeuedecimal" => Some(dequeue_decimal),
        "dequeueinteger" => Some(dequeue_integer),
        "dequeuedate" => Some(dequeue_date),
        "dequeuedatetime" => Some(dequeue_date_time),
        "dequeuetime" => Some(dequeue_time),
        "dequeueboolean" => Some(dequeue_boolean),
        "peektext" => Some(peek_text),
        "peekdecimal" => Some(peek_decimal),
        "peekinteger" => Some(peek_integer),
        "peekdate" => Some(peek_date),
        "peektime" => Some(peek_time),
        "peekboolean" => Some(peek_boolean),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() {
        reset_queue();
    }

    fn assert_pass(eval: Eval) {
        match eval {
            Eval::Normal(_) => {}
            Eval::Error(e) => panic!("expected pass, got error: {}", e.message),
            Eval::Exit(v) => panic!("expected pass, got exit({v:?})"),
        }
    }

    fn assert_fail_contains(eval: Eval, needle: &str) {
        match eval {
            Eval::Error(e) => assert!(
                e.message.contains(needle),
                "expected message containing `{needle}`, got: {}",
                e.message
            ),
            other => panic!("expected Eval::Error, got {other:?}"),
        }
    }

    fn assert_value(eval: Eval) -> Value {
        match eval {
            Eval::Normal(v) => v,
            Eval::Error(e) => panic!("expected Normal, got error: {}", e.message),
            Eval::Exit(v) => v,
        }
    }

    #[test]
    fn enqueue_dequeue_single_integer() {
        setup();
        assert_pass(enqueue(&[Value::Integer(42)]));
        let got = assert_value(dequeue(&[]));
        assert_eq!(got, Value::Integer(42));
    }

    #[test]
    fn enqueue_dequeue_fifo_order() {
        setup();
        enqueue(&[Value::Integer(1)]);
        enqueue(&[Value::Integer(2)]);
        enqueue(&[Value::Integer(3)]);
        assert_eq!(assert_value(dequeue(&[])), Value::Integer(1));
        assert_eq!(assert_value(dequeue(&[])), Value::Integer(2));
        assert_eq!(assert_value(dequeue(&[])), Value::Integer(3));
    }

    #[test]
    fn length_tracks_queue_size() {
        setup();
        assert_eq!(assert_value(length(&[])), Value::Integer(0));
        enqueue(&[Value::Text("a".into())]);
        assert_eq!(assert_value(length(&[])), Value::Integer(1));
        enqueue(&[Value::Text("b".into())]);
        assert_eq!(assert_value(length(&[])), Value::Integer(2));
        dequeue(&[]);
        assert_eq!(assert_value(length(&[])), Value::Integer(1));
    }

    #[test]
    fn max_length_is_25() {
        assert_eq!(assert_value(max_length(&[])), Value::Integer(25));
    }

    #[test]
    fn peek_does_not_remove() {
        setup();
        enqueue(&[Value::Integer(99)]);
        let peeked = assert_value(peek(&[Value::Integer(1)]));
        assert_eq!(peeked, Value::Integer(99));
        assert_eq!(assert_value(length(&[])), Value::Integer(1));
    }

    #[test]
    fn peek_index_2_returns_second_item() {
        setup();
        enqueue(&[Value::Integer(10)]);
        enqueue(&[Value::Integer(20)]);
        enqueue(&[Value::Integer(30)]);
        assert_eq!(assert_value(peek(&[Value::Integer(2)])), Value::Integer(20));
        assert_eq!(assert_value(peek(&[Value::Integer(3)])), Value::Integer(30));
    }

    #[test]
    fn clear_empties_queue() {
        setup();
        enqueue(&[Value::Integer(1)]);
        enqueue(&[Value::Integer(2)]);
        assert_pass(clear(&[]));
        assert_eq!(assert_value(length(&[])), Value::Integer(0));
    }

    #[test]
    fn assert_empty_passes_on_empty_queue() {
        setup();
        assert_pass(assert_empty(&[]));
    }

    #[test]
    fn assert_empty_fails_and_clears_non_empty_queue() {
        setup();
        enqueue(&[Value::Integer(1)]);
        assert_fail_contains(assert_empty(&[]), "not empty");
        // Queue must be cleared even after failure (matches BC semantics).
        assert_eq!(assert_value(length(&[])), Value::Integer(0));
    }

    #[test]
    fn dequeue_empty_is_underflow_error() {
        setup();
        assert_fail_contains(dequeue(&[]), "underflow");
    }

    #[test]
    fn enqueue_beyond_capacity_is_overflow_error() {
        setup();
        for i in 0..MAX_QUEUE_SIZE {
            assert_pass(enqueue(&[Value::Integer(i as i64)]));
        }
        assert_fail_contains(enqueue(&[Value::Integer(99)]), "overflow");
    }

    #[test]
    fn assert_not_overflow_passes_when_space_available() {
        setup();
        assert_pass(assert_not_overflow(&[]));
    }

    #[test]
    fn assert_not_overflow_fails_when_full() {
        setup();
        for i in 0..MAX_QUEUE_SIZE {
            enqueue(&[Value::Integer(i as i64)]);
        }
        assert_fail_contains(assert_not_overflow(&[]), "overflow");
    }

    #[test]
    fn assert_not_underflow_passes_when_non_empty() {
        setup();
        enqueue(&[Value::Integer(1)]);
        assert_pass(assert_not_underflow(&[]));
    }

    #[test]
    fn assert_not_underflow_fails_when_empty() {
        setup();
        assert_fail_contains(assert_not_underflow(&[]), "underflow");
    }

    #[test]
    fn assert_full_passes_when_at_capacity() {
        setup();
        for i in 0..MAX_QUEUE_SIZE {
            enqueue(&[Value::Integer(i as i64)]);
        }
        assert_pass(assert_full(&[]));
    }

    #[test]
    fn assert_full_fails_when_not_full() {
        setup();
        assert_fail_contains(assert_full(&[]), "empty");
    }

    #[test]
    fn assert_peek_available_valid_index() {
        setup();
        enqueue(&[Value::Integer(1)]);
        enqueue(&[Value::Integer(2)]);
        assert_pass(assert_peek_available(&[Value::Integer(1)]));
        assert_pass(assert_peek_available(&[Value::Integer(2)]));
    }

    #[test]
    fn assert_peek_available_out_of_bounds() {
        setup();
        enqueue(&[Value::Integer(1)]);
        assert_fail_contains(assert_peek_available(&[Value::Integer(0)]), "bounds");
        assert_fail_contains(assert_peek_available(&[Value::Integer(2)]), "bounds");
        assert_fail_contains(assert_peek_available(&[Value::Integer(-1)]), "bounds");
    }

    #[test]
    fn dequeue_text_formats_integer() {
        setup();
        enqueue(&[Value::Integer(7)]);
        assert_eq!(
            assert_value(dequeue_text(&[])),
            Value::Text("7".to_string())
        );
    }

    #[test]
    fn dequeue_decimal_returns_decimal() {
        setup();
        enqueue(&[Value::Decimal(3.5)]);
        match assert_value(dequeue_decimal(&[])) {
            Value::Decimal(d) => assert!((d - 3.5).abs() < 1e-9),
            other => panic!("expected Decimal, got {other:?}"),
        }
    }

    #[test]
    fn dequeue_decimal_type_mismatch_is_error() {
        setup();
        enqueue(&[Value::Text("nope".into())]);
        assert_fail_contains(dequeue_decimal(&[]), "type mismatch");
    }

    #[test]
    fn dequeue_integer_returns_integer() {
        setup();
        enqueue(&[Value::Integer(99)]);
        assert_eq!(assert_value(dequeue_integer(&[])), Value::Integer(99));
    }

    #[test]
    fn dequeue_integer_type_mismatch_is_error() {
        setup();
        enqueue(&[Value::Text("x".into())]);
        assert_fail_contains(dequeue_integer(&[]), "type mismatch");
    }

    #[test]
    fn dequeue_boolean_returns_boolean() {
        setup();
        enqueue(&[Value::Boolean(true)]);
        assert_eq!(assert_value(dequeue_boolean(&[])), Value::Boolean(true));
    }

    #[test]
    fn dequeue_boolean_type_mismatch_is_error() {
        setup();
        enqueue(&[Value::Integer(1)]);
        assert_fail_contains(dequeue_boolean(&[]), "type mismatch");
    }

    #[test]
    fn peek_text_reads_without_removing() {
        setup();
        enqueue(&[Value::Integer(5)]);
        assert_eq!(
            assert_value(peek_text(&[Value::Integer(1)])),
            Value::Text("5".to_string())
        );
        assert_eq!(assert_value(length(&[])), Value::Integer(1));
    }

    #[test]
    fn peek_integer_returns_integer() {
        setup();
        enqueue(&[Value::Integer(42)]);
        assert_eq!(
            assert_value(peek_integer(&[Value::Integer(1)])),
            Value::Integer(42)
        );
    }

    #[test]
    fn peek_integer_type_mismatch_is_error() {
        setup();
        enqueue(&[Value::Text("abc".into())]);
        assert_fail_contains(peek_integer(&[Value::Integer(1)]), "type mismatch");
    }

    #[test]
    fn peek_out_of_bounds_is_error() {
        setup();
        assert_fail_contains(peek(&[Value::Integer(1)]), "bounds");
    }

    #[test]
    fn resolve_is_case_insensitive() {
        assert!(resolve("Enqueue").is_some());
        assert!(resolve("ENQUEUE").is_some());
        assert!(resolve("enqueue").is_some());
        assert!(resolve("Dequeue").is_some());
        assert!(resolve("DequeueText").is_some());
        assert!(resolve("DEQUEUETEXT").is_some());
        assert!(resolve("MaxLength").is_some());
        assert!(resolve("MAXLENGTH").is_some());
    }

    #[test]
    fn resolve_unknown_returns_none() {
        assert!(resolve("DoesNotExist").is_none());
        assert!(resolve("").is_none());
        assert!(resolve("EnqueueAll").is_none());
    }

    #[test]
    fn mixed_type_round_trip() {
        setup();
        enqueue(&[Value::Integer(1)]);
        enqueue(&[Value::Text("hello".into())]);
        enqueue(&[Value::Boolean(false)]);
        assert_eq!(assert_value(dequeue(&[])), Value::Integer(1));
        assert_eq!(assert_value(dequeue(&[])), Value::Text("hello".into()));
        assert_eq!(assert_value(dequeue(&[])), Value::Boolean(false));
        assert_pass(assert_empty(&[]));
    }
}
