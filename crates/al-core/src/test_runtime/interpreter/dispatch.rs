//! Procedure-call dispatch for the AL interpreter — Phase 2.
//!
//! `dispatch_call` is the single entry point for any procedure call that the
//! statement evaluator encounters. Priority order:
//!
//! 1. **Stub catalogs** — Library Assert and any other native-Rust ports of
//!    BC test libraries (via `crate::test_runtime::stubs`).
//! 2. **Inline builtins** — core AL global procedures implemented here in Rust
//!    (Error, Message, StrSubstNo, Format, StrLen, CopyStr, LowerCase,
//!    UpperCase, IndexOf).
//! 3. **Workspace procedures** — NOT yet wired; returns a descriptive
//!    `Eval::Error` for Phase 2. Phase 2b will tie this to
//!    `syntax::TypeResolver` + `symbols::SymbolIndex`.

use std::collections::HashMap;
use std::sync::Arc;

use crate::test_runtime::interpreter::scope::Eval;
use crate::test_runtime::interpreter::value::{ErrorInfo, Value};
use crate::test_runtime::mock::record::MockRecord;
use crate::test_runtime::stubs;
use crate::workspace::Workspace;

// ---------------------------------------------------------------------------
// DispatchMode and DispatchCtx
// ---------------------------------------------------------------------------

/// What record-level support the active run has access to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchMode {
    /// Pure-logic run — no record store.
    PureLogic,
    /// Record store wired in (Phase 3 territory, partial support).
    WithRecords,
}

/// Shared context threaded through `eval_stmt` and `dispatch_call`.
///
/// Owns the workspace handle plus any stateful runtime support (record
/// catalog for `InterpRecord` runs). Cheap to reborrow; pass `&mut DispatchCtx`
/// everywhere.
pub struct DispatchCtx {
    /// The owning workspace — used for workspace-procedure lookup (Phase 2b).
    pub workspace: Arc<Workspace>,
    /// In-memory record store keyed by table ID. Only populated when
    /// `mode == WithRecords`. Phase 2 does not populate this.
    pub records: HashMap<i32, MockRecord>,
    /// Which dispatch mode is active.
    pub mode: DispatchMode,
}

impl DispatchCtx {
    /// Create a pure-logic dispatch context (no records).
    pub fn new_pure(workspace: Arc<Workspace>) -> Self {
        Self {
            workspace,
            records: HashMap::new(),
            mode: DispatchMode::PureLogic,
        }
    }

    /// Create a dispatch context that includes a pre-populated record store.
    pub fn new_with_records(workspace: Arc<Workspace>, records: HashMap<i32, MockRecord>) -> Self {
        Self {
            workspace,
            records,
            mode: DispatchMode::WithRecords,
        }
    }
}

// ---------------------------------------------------------------------------
// Public dispatch entry point
// ---------------------------------------------------------------------------

/// Resolve and execute a procedure call.
///
/// `receiver` is the optional object qualifier (e.g. `"Assert"` for
/// `Assert.AreEqual(...)`). `procedure` is the bare procedure name.
/// `args` are already-evaluated positional arguments.
///
/// Returns `Eval::Normal` on success, `Eval::Error` on failure. Never panics.
pub fn dispatch_call(
    receiver: Option<&str>,
    procedure: &str,
    args: Vec<Value>,
    _ctx: &mut DispatchCtx,
) -> Eval {
    // ── 1. Stub catalogs (Library Assert etc.) ────────────────────────────────
    if let Some(recv) = receiver {
        if let Some(stub_fn) = stubs::resolve(recv, procedure) {
            return stub_fn(&args);
        }
    }
    // Also try bare procedure name against every catalog (no receiver).
    if receiver.is_none() {
        for cat in stubs::CATALOGS {
            if let Some(f) = (cat.resolve)(procedure) {
                return f(&args);
            }
        }
    }

    // ── 2. Inline builtins ────────────────────────────────────────────────────
    match procedure.to_ascii_lowercase().as_str() {
        "error" => builtin_error(&args),
        "message" => builtin_message(&args),
        "strsubstno" => builtin_strsubstno(&args),
        "format" => builtin_format(&args),
        "strlen" => builtin_strlen(&args),
        "copystr" => builtin_copystr(&args),
        "lowercase" => builtin_lowercase(&args),
        "uppercase" => builtin_uppercase(&args),
        "indexof" => builtin_indexof(&args),
        // ── 3. Workspace procedures (Phase 2b placeholder) ───────────────────
        _other => Eval::Error(ErrorInfo {
            message: format!(
                "workspace procedure dispatch not yet wired: {}{}",
                receiver.map(|r| format!("{r}.")).unwrap_or_default(),
                procedure
            ),
            error_type: None,
            source: None,
        }),
    }
}

// ---------------------------------------------------------------------------
// Inline built-in implementations
// ---------------------------------------------------------------------------

fn simple_error(msg: impl Into<String>) -> Eval {
    Eval::Error(ErrorInfo {
        message: msg.into(),
        error_type: None,
        source: None,
    })
}

/// `Error(msg[, arg1, …])` — raise a runtime error.
///
/// The first argument is the format string; subsequent arguments are
/// substituted as %1, %2, … (same semantics as StrSubstNo).
fn builtin_error(args: &[Value]) -> Eval {
    let msg = match args.first() {
        Some(Value::Text(s)) | Some(Value::Code(s)) => s.clone(),
        Some(v) => render_value(v),
        None => return simple_error("Error() called with no arguments"),
    };
    let formatted = if args.len() > 1 {
        substitute_placeholders(&msg, &args[1..])
    } else {
        msg
    };
    Eval::Error(ErrorInfo {
        message: formatted,
        error_type: None,
        source: None,
    })
}

/// `Message(msg[, arg1, …])` — display a message (no-op in the interpreter;
/// returns Normal so execution continues).
fn builtin_message(args: &[Value]) -> Eval {
    let msg = match args.first() {
        Some(Value::Text(s)) | Some(Value::Code(s)) => s.clone(),
        Some(v) => render_value(v),
        None => return Eval::Normal(Value::Empty),
    };
    let _formatted = if args.len() > 1 {
        substitute_placeholders(&msg, &args[1..])
    } else {
        msg
    };
    // In the interpreter, Message is a no-op (no UI). We still validate args.
    Eval::Normal(Value::Empty)
}

/// `StrSubstNo(fmt, arg1, …)` — substitute %1, %2, … in `fmt`.
fn builtin_strsubstno(args: &[Value]) -> Eval {
    let fmt = match args.first() {
        Some(Value::Text(s)) | Some(Value::Code(s)) => s.clone(),
        Some(v) => render_value(v),
        None => return simple_error("StrSubstNo requires at least 1 argument"),
    };
    let result = substitute_placeholders(&fmt, &args[1..]);
    Eval::Normal(Value::Text(result))
}

/// `Format(value[, length[, format_str]])` — convert a value to Text.
///
/// Phase 2 implements only the single-argument form.
fn builtin_format(args: &[Value]) -> Eval {
    match args.first() {
        Some(v) => Eval::Normal(Value::Text(render_value(v))),
        None => simple_error("Format() requires at least 1 argument"),
    }
}

/// `StrLen(s)` — length of a string in characters.
fn builtin_strlen(args: &[Value]) -> Eval {
    match args {
        [Value::Text(s)] | [Value::Code(s)] => {
            Eval::Normal(Value::Integer(s.chars().count() as i64))
        }
        [v] => simple_error(format!(
            "StrLen expects Text or Code, got {}",
            v.type_name()
        )),
        _ => simple_error("StrLen expects exactly 1 argument"),
    }
}

/// `CopyStr(s, pos[, len])` — extract a substring. 1-based position.
fn builtin_copystr(args: &[Value]) -> Eval {
    let (s, pos) = match args {
        [Value::Text(s), Value::Integer(pos)]
        | [Value::Code(s), Value::Integer(pos)]
        | [Value::Text(s), Value::Integer(pos), _]
        | [Value::Code(s), Value::Integer(pos), _] => (s.clone(), *pos as usize),
        _ => return simple_error("CopyStr expects (Text, Integer[, Integer])"),
    };
    let len = match args.get(2) {
        Some(Value::Integer(n)) => *n as usize,
        None => s.chars().count(),
        Some(v) => {
            return simple_error(format!(
                "CopyStr: len must be Integer, got {}",
                v.type_name()
            ))
        }
    };
    if pos == 0 {
        return simple_error("CopyStr: position must be >= 1");
    }
    let chars: Vec<char> = s.chars().collect();
    let start = (pos - 1).min(chars.len());
    let end = (start + len).min(chars.len());
    Eval::Normal(Value::Text(chars[start..end].iter().collect()))
}

/// `LowerCase(s)` — convert to lower case.
fn builtin_lowercase(args: &[Value]) -> Eval {
    match args {
        [Value::Text(s)] => Eval::Normal(Value::Text(s.to_lowercase())),
        [Value::Code(s)] => Eval::Normal(Value::Text(s.to_lowercase())),
        [v] => simple_error(format!(
            "LowerCase expects Text or Code, got {}",
            v.type_name()
        )),
        _ => simple_error("LowerCase expects exactly 1 argument"),
    }
}

/// `UpperCase(s)` — convert to upper case.
fn builtin_uppercase(args: &[Value]) -> Eval {
    match args {
        [Value::Text(s)] => Eval::Normal(Value::Text(s.to_uppercase())),
        [Value::Code(s)] => Eval::Normal(Value::Code(s.to_uppercase())),
        [v] => simple_error(format!(
            "UpperCase expects Text or Code, got {}",
            v.type_name()
        )),
        _ => simple_error("UpperCase expects exactly 1 argument"),
    }
}

/// `IndexOf(s, needle)` — return the 1-based index of `needle` in `s`,
/// or 0 if not found.
fn builtin_indexof(args: &[Value]) -> Eval {
    let (s, needle) = match args {
        [Value::Text(s), Value::Text(n)]
        | [Value::Code(s), Value::Text(n)]
        | [Value::Text(s), Value::Code(n)]
        | [Value::Code(s), Value::Code(n)] => (s.as_str(), n.as_str()),
        _ => return simple_error("IndexOf expects (Text, Text)"),
    };
    let result = s
        .find(needle)
        .map(|i| {
            // Convert byte offset to 1-based char index.
            s[..i].chars().count() as i64 + 1
        })
        .unwrap_or(0);
    Eval::Normal(Value::Integer(result))
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Render a `Value` as AL would show it in StrSubstNo / Format.
fn render_value(v: &Value) -> String {
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
        Value::Null => String::new(),
        Value::Empty => String::new(),
        other => format!("<{}>", other.type_name()),
    }
}

/// Substitute %1, %2, … placeholders in `fmt` with rendered arg values.
fn substitute_placeholders(fmt: &str, args: &[Value]) -> String {
    let mut result = fmt.to_string();
    for (i, arg) in args.iter().enumerate() {
        let placeholder = format!("%{}", i + 1);
        result = result.replace(&placeholder, &render_value(arg));
    }
    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;
    use std::sync::Arc;

    fn ctx() -> DispatchCtx {
        DispatchCtx::new_pure(Arc::new(Workspace::new()))
    }

    fn ok(eval: Eval) -> Value {
        match eval {
            Eval::Normal(v) => v,
            Eval::Error(e) => panic!("unexpected error: {}", e.message),
            Eval::Exit(v) => v,
        }
    }

    fn err(eval: Eval) -> ErrorInfo {
        match eval {
            Eval::Error(e) => e,
            Eval::Normal(v) => panic!("expected error, got Normal({})", v.type_name()),
            Eval::Exit(v) => panic!("expected error, got Exit({})", v.type_name()),
        }
    }

    // ── Stub routing ──────────────────────────────────────────────────────────

    #[test]
    fn stub_routed_library_assert_are_equal() {
        // Positive: Assert.AreEqual(1, 1) routes to the Library Assert stub.
        let mut ctx = ctx();
        let result = dispatch_call(
            Some("Library Assert"),
            "AreEqual",
            vec![Value::Integer(1), Value::Integer(1)],
            &mut ctx,
        );
        assert!(
            matches!(result, Eval::Normal(_)),
            "Expected Normal, got {:?}",
            result
        );
    }

    #[test]
    fn stub_routed_library_assert_are_equal_fail() {
        // Negative: AreEqual(1, 2) produces Eval::Error with "expected".
        let mut ctx = ctx();
        let result = dispatch_call(
            Some("Library Assert"),
            "AreEqual",
            vec![Value::Integer(1), Value::Integer(2)],
            &mut ctx,
        );
        let e = err(result);
        assert!(
            e.message.contains("expected"),
            "expected message containing 'expected', got: {}",
            e.message
        );
    }

    // ── Error builtin ─────────────────────────────────────────────────────────

    #[test]
    fn error_builtin_produces_eval_error() {
        // Positive: Error("boom") → Eval::Error with "boom".
        let mut ctx = ctx();
        let result = dispatch_call(None, "Error", vec![Value::Text("boom".into())], &mut ctx);
        let e = err(result);
        assert_eq!(e.message, "boom");
    }

    #[test]
    fn error_builtin_with_no_args_is_error() {
        // Negative: Error() with no arguments still produces Eval::Error.
        let mut ctx = ctx();
        let result = dispatch_call(None, "Error", vec![], &mut ctx);
        assert!(result.is_error());
    }

    // ── StrSubstNo ───────────────────────────────────────────────────────────

    #[test]
    fn strsubstno_formats_correctly() {
        // Positive: StrSubstNo("Hello %1, you are %2 years old", "Alice", 30).
        let mut ctx = ctx();
        let result = dispatch_call(
            None,
            "StrSubstNo",
            vec![
                Value::Text("Hello %1, you are %2 years old".into()),
                Value::Text("Alice".into()),
                Value::Integer(30),
            ],
            &mut ctx,
        );
        match ok(result) {
            Value::Text(s) => assert_eq!(s, "Hello Alice, you are 30 years old"),
            other => panic!("expected Text, got {:?}", other),
        }
    }

    #[test]
    fn strsubstno_no_args_is_error() {
        // Negative: StrSubstNo with no args is an error.
        let mut ctx = ctx();
        let result = dispatch_call(None, "StrSubstNo", vec![], &mut ctx);
        assert!(result.is_error());
    }

    // ── Format ───────────────────────────────────────────────────────────────

    #[test]
    fn format_integer_to_text() {
        let mut ctx = ctx();
        let result = dispatch_call(None, "Format", vec![Value::Integer(42)], &mut ctx);
        match ok(result) {
            Value::Text(s) => assert_eq!(s, "42"),
            other => panic!("expected Text, got {:?}", other),
        }
    }

    // ── Unknown procedure ─────────────────────────────────────────────────────

    #[test]
    fn unknown_procedure_returns_descriptive_error() {
        // Negative: an unknown procedure returns Eval::Error with a descriptive message.
        let mut ctx = ctx();
        let result = dispatch_call(None, "CompletelyUnknownProc", vec![], &mut ctx);
        let e = err(result);
        assert!(
            e.message.contains("CompletelyUnknownProc"),
            "expected procedure name in error, got: {}",
            e.message
        );
        assert!(
            e.message.contains("not yet wired"),
            "expected 'not yet wired' in error, got: {}",
            e.message
        );
    }

    // ── StrLen / CopyStr / IndexOf ────────────────────────────────────────────

    #[test]
    fn strlen_returns_char_count() {
        let mut ctx = ctx();
        let result = dispatch_call(None, "StrLen", vec![Value::Text("hello".into())], &mut ctx);
        assert_eq!(ok(result), Value::Integer(5));
    }

    #[test]
    fn strlen_non_text_is_error() {
        // Negative: StrLen on an Integer is an error.
        let mut ctx = ctx();
        let result = dispatch_call(None, "StrLen", vec![Value::Integer(123)], &mut ctx);
        assert!(result.is_error());
    }

    #[test]
    fn copystr_extracts_substring() {
        let mut ctx = ctx();
        let result = dispatch_call(
            None,
            "CopyStr",
            vec![
                Value::Text("Hello World".into()),
                Value::Integer(1),
                Value::Integer(5),
            ],
            &mut ctx,
        );
        assert_eq!(ok(result), Value::Text("Hello".into()));
    }

    #[test]
    fn indexof_finds_needle() {
        let mut ctx = ctx();
        let result = dispatch_call(
            None,
            "IndexOf",
            vec![
                Value::Text("Hello World".into()),
                Value::Text("World".into()),
            ],
            &mut ctx,
        );
        assert_eq!(ok(result), Value::Integer(7));
    }

    #[test]
    fn indexof_missing_returns_zero() {
        // Negative: needle not present → 0.
        let mut ctx = ctx();
        let result = dispatch_call(
            None,
            "IndexOf",
            vec![Value::Text("Hello".into()), Value::Text("xyz".into())],
            &mut ctx,
        );
        assert_eq!(ok(result), Value::Integer(0));
    }

    // ── LowerCase / UpperCase ─────────────────────────────────────────────────

    #[test]
    fn lowercase_works() {
        let mut ctx = ctx();
        let result = dispatch_call(
            None,
            "LowerCase",
            vec![Value::Text("HELLO".into())],
            &mut ctx,
        );
        assert_eq!(ok(result), Value::Text("hello".into()));
    }

    #[test]
    fn uppercase_works() {
        let mut ctx = ctx();
        let result = dispatch_call(
            None,
            "UpperCase",
            vec![Value::Text("hello".into())],
            &mut ctx,
        );
        assert_eq!(ok(result), Value::Text("HELLO".into()));
    }

    // ── Adversarial tests (adversarial-h) ─────────────────────────────────────

    #[test]
    fn strsubstno_percent10_placeholder_corrupted_adversarial_h_8() {
        // FINDING P1 wrong-result: substitute_placeholders iterates i=0..9
        // and replaces %1 first, consuming the %1 prefix inside %10. Format
        // "%1 and %10" with 10 args yields "FIRST and FIRST0" not "FIRST and TENTH".
        // Root cause: str::replace scans the original string left-to-right; the
        // %1 at position 0 AND the %1 inside %10 both get replaced on iteration 0.
        // Expected: "FIRST and TENTH"
        // Observed: "FIRST and FIRST0"
        let mut ctx = ctx();
        let result = dispatch_call(
            None,
            "StrSubstNo",
            vec![
                Value::Text("%1 and %10".into()),
                Value::Text("FIRST".into()),
                Value::Text("TWO".into()),
                Value::Text("THREE".into()),
                Value::Text("FOUR".into()),
                Value::Text("FIVE".into()),
                Value::Text("SIX".into()),
                Value::Text("SEVEN".into()),
                Value::Text("EIGHT".into()),
                Value::Text("NINE".into()),
                Value::Text("TENTH".into()),
            ],
            &mut ctx,
        );
        match ok(result) {
            Value::Text(s) => assert_eq!(
                s, "FIRST and TENTH",
                "%%10 must map to 10th arg; got: {s:?}"
            ),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn copystr_pos_beyond_string_length_adversarial_h_9() {
        // FINDING P2 spec-deviation: CopyStr("abc", 4, 1) silently returns ""
        // instead of raising an error. Position 4 is beyond the 3-char string.
        // AL/BC runtime raises "The value is too large" for out-of-bounds pos.
        // Expected: Eval::Error
        // Observed: Normal(Text(""))
        let mut ctx = ctx();
        let result = dispatch_call(
            None,
            "CopyStr",
            vec![
                Value::Text("abc".into()),
                Value::Integer(4),
                Value::Integer(1),
            ],
            &mut ctx,
        );
        assert!(
            result.is_error(),
            "CopyStr pos > string length must error, got: {:?}",
            result
        );
    }

    #[test]
    fn indexof_empty_needle_returns_one_not_zero_adversarial_h_10() {
        // FINDING P2 edge-case: IndexOf("ab", "") returns Integer(1) because
        // Rust str::find("") returns Some(0). AL convention: empty needle → 0.
        // Expected: Integer(0)
        // Observed: Integer(1)
        let mut ctx = ctx();
        let result = dispatch_call(
            None,
            "IndexOf",
            vec![Value::Text("ab".into()), Value::Text(String::new())],
            &mut ctx,
        );
        assert_eq!(
            ok(result),
            Value::Integer(0),
            "IndexOf with empty needle should return 0"
        );
    }
}
