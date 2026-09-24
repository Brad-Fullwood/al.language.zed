//! Error and dialog builtins.
//!
//! Message, Confirm and their relatives need a handler; without one they fail
//! closed rather than silently continuing, which is what an unattended test
//! run needs.

use crate::interpreter::eval_error;
use crate::interpreter::scope::Eval;
use crate::interpreter::value::{ErrorInfo, Value};

use super::render::{render_value, substitute_placeholders};

/// `Error(msg[, arg1, …])` — raise a runtime error.
///
/// The first argument is the format string; subsequent arguments are
/// substituted as %1, %2, … (same semantics as StrSubstNo).
pub(super) fn builtin_error(args: &[Value]) -> Eval {
    let msg = match args.first() {
        Some(Value::Text(s)) | Some(Value::Code(s)) => s.clone(),
        Some(v) => render_value(v),
        None => return eval_error("Error() called with no arguments"),
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

pub(super) fn formatted_dialog_text(args: &[Value]) -> String {
    let message = match args.first() {
        Some(Value::Text(text)) | Some(Value::Code(text)) => text.clone(),
        Some(value) => render_value(value),
        None => String::new(),
    };
    if args.len() > 1 {
        substitute_placeholders(&message, &args[1..])
    } else {
        message
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::interpreter::dispatch::routing::dispatch_call;
    use crate::interpreter::dispatch::test_support::{ctx, error_of};

    #[test]
    fn error_builtin_produces_eval_error() {
        let mut ctx = ctx();
        let result = dispatch_call(None, "Error", vec![Value::Text("boom".into())], &mut ctx);
        let e = error_of(result);
        assert_eq!(e.message, "boom");
    }

    #[test]
    fn error_builtin_with_no_args_is_error() {
        let mut ctx = ctx();
        let result = dispatch_call(None, "Error", vec![], &mut ctx);
        assert!(result.is_error());
    }

    #[test]
    fn dialog_builtins_without_handlers_fail_closed() {
        let cases = [
            ("Message", vec![Value::Text("hello".into())]),
            ("Confirm", vec![Value::Text("continue?".into())]),
            ("StrMenu", vec![Value::Text("One,Two".into())]),
            (
                "Hyperlink",
                vec![Value::Text("https://example.test".into())],
            ),
        ];
        for (procedure, args) in cases {
            let mut ctx = ctx();
            let error = error_of(dispatch_call(None, procedure, args, &mut ctx));
            assert!(
                error.message.contains("configured"),
                "{procedure}: {}",
                error.message
            );
        }
    }
}
