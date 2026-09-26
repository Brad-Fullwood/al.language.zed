//! The single entry point for a procedure call, and the order it tries.
//!
//! Stub catalogs first, then the inline builtins, then workspace procedures.
//! An explicit receiver skips the builtin step, so a workspace procedure named
//! like a builtin is still the one that runs.

use crate::interpreter::eval_error;
use crate::interpreter::scope::{Eval, ScopeStack};
use crate::interpreter::value::Value;
use crate::stubs;

use super::datetime::{
    builtin_calcdate, builtin_createdatetime, builtin_date2dmy, builtin_date2dwy, builtin_dmy2date,
    builtin_dt2date, builtin_dt2time, builtin_workdate, clock_current_datetime, clock_time,
    clock_today,
};
use super::dialog::{builtin_error, formatted_dialog_text};
use super::numeric::{
    builtin_abs, builtin_arraylen, builtin_extreme, builtin_power, builtin_round,
};
use super::random::{builtin_random, builtin_randomize};
use super::render::{builtin_format, builtin_strsubstno, render_value};
use super::text::{
    builtin_convertstr, builtin_copystr, builtin_delchr, builtin_delstr, builtin_evaluate,
    builtin_incstr, builtin_indexof, builtin_lowercase, builtin_maxstrlen, builtin_padstr,
    builtin_selectstr, builtin_strlen, builtin_strpos, builtin_uppercase,
};
use super::workspace_procedure::dispatch_workspace_procedure;
use super::DispatchCtx;

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
    ctx: &mut DispatchCtx,
) -> Eval {
    let mut stack = ScopeStack::new();
    dispatch_call_scoped(receiver, procedure, args, &mut stack, ctx)
}

/// Resolve a call against the active interpreter scope.
///
/// Preserving the stack is required for same-codeunit procedure calls and test
/// handlers to observe the codeunit's object-level globals. The public
/// [`dispatch_call`] wrapper intentionally starts an empty scope for isolated
/// builtin/stub tests.
pub(crate) fn dispatch_call_scoped(
    receiver: Option<&str>,
    procedure: &str,
    args: Vec<Value>,
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    // Clear any var-parameter write-backs left over from a previous call so
    // builtins and non-workspace calls (which never populate it) leave the
    // channel empty for the caller to observe.
    ctx.var_writebacks.clear();
    // Statement position matters only to builtins that fail differently as a
    // statement (Evaluate); take it so it never leaks into a callee's body.
    let statement = std::mem::take(&mut ctx.stmt_position);
    // An enum variable never assigned carries only its ordinal; name it
    // before any builtin or stub shows it.
    let args = if args
        .iter()
        .any(|arg| matches!(arg, Value::Option { member, .. } if member.is_empty()))
    {
        crate::interpreter::enums::with_member_names(args, ctx)
    } else {
        args
    };
    if let Some(recv) = receiver {
        if stubs::is_context_member(recv, procedure) {
            return dispatch_stub_with_context(recv, procedure, &args, ctx);
        }
        if let Some(stub_fn) = stubs::resolve(recv, procedure) {
            return stub_fn(&args);
        }
    }
    if receiver.is_none() {
        for cat in stubs::CATALOGS {
            if let Some(f) = (cat.resolve)(procedure) {
                return f(&args);
            }
        }
    }

    // Global builtins must never hijack an explicitly-qualified workspace
    // method with the same name (for example `Helper.Format(...)`).
    if receiver.is_none() {
        match procedure.to_ascii_lowercase().as_str() {
            "error" => return builtin_error(&args),
            "message" => {
                if let Some((object, handler)) = ctx.test_handlers.message.clone() {
                    if args.is_empty() {
                        return eval_error("Message requires a message argument");
                    }
                    let message = formatted_dialog_text(&args);
                    let result = dispatch_workspace_procedure(
                        Some(&object),
                        &handler,
                        vec![Value::Text(message)],
                        stack,
                        ctx,
                    );
                    ctx.var_writebacks.clear();
                    return result;
                }
                return eval_error(
                    "Message requires a configured [MessageHandler] in the local test runtime",
                );
            }
            "confirm" => {
                if let Some((object, handler)) = ctx.test_handlers.confirm.clone() {
                    if args.is_empty() {
                        return eval_error("Confirm requires a question argument");
                    }
                    let question = formatted_dialog_text(&args);
                    let result = dispatch_workspace_procedure(
                        Some(&object),
                        &handler,
                        vec![Value::Text(question), Value::Boolean(false)],
                        stack,
                        ctx,
                    );
                    if result.is_error() {
                        return result;
                    }
                    let reply = ctx
                        .var_writebacks
                        .iter()
                        .find(|(index, _)| *index == 1)
                        .and_then(|(_, value)| match value {
                            Value::Boolean(reply) => Some(*reply),
                            _ => None,
                        })
                        .unwrap_or(false);
                    ctx.var_writebacks.clear();
                    return Eval::Normal(Value::Boolean(reply));
                }
                return eval_error(
                    "Confirm requires a configured [ConfirmHandler] in the local test runtime",
                );
            }
            "strmenu" => {
                if args.is_empty() {
                    return eval_error("StrMenu requires a menu-options argument");
                }
                let default_choice = args
                    .get(1)
                    .and_then(|value| match value {
                        Value::Integer(choice) => Some(*choice),
                        _ => None,
                    })
                    .unwrap_or(0);
                if let Some((object, handler)) = ctx.test_handlers.str_menu.clone() {
                    let options = args.first().map(render_value).unwrap_or_default();
                    let instruction = args.get(2).map(render_value).unwrap_or_default();
                    let result = dispatch_workspace_procedure(
                        Some(&object),
                        &handler,
                        vec![
                            Value::Text(options),
                            Value::Integer(default_choice),
                            Value::Text(instruction),
                        ],
                        stack,
                        ctx,
                    );
                    if result.is_error() {
                        return result;
                    }
                    let choice = ctx
                        .var_writebacks
                        .iter()
                        .find(|(index, _)| *index == 1)
                        .and_then(|(_, value)| match value {
                            Value::Integer(choice) => Some(*choice),
                            _ => None,
                        })
                        .unwrap_or(default_choice);
                    ctx.var_writebacks.clear();
                    return Eval::Normal(Value::Integer(choice));
                }
                return eval_error(
                    "StrMenu requires a configured [StrMenuHandler] in the local test runtime",
                );
            }
            "hyperlink" => {
                if let Some((object, handler)) = ctx.test_handlers.hyperlink.clone() {
                    if args.is_empty() {
                        return eval_error("Hyperlink requires a link argument");
                    }
                    let link = args.first().map(render_value).unwrap_or_default();
                    let result = dispatch_workspace_procedure(
                        Some(&object),
                        &handler,
                        vec![Value::Text(link)],
                        stack,
                        ctx,
                    );
                    ctx.var_writebacks.clear();
                    return result;
                }
                return eval_error(
                    "Hyperlink requires a configured [HyperlinkHandler] in the local test runtime",
                );
            }
            "strsubstno" => return builtin_strsubstno(&args),
            "format" => return builtin_format(&args),
            "strlen" => return builtin_strlen(&args),
            "copystr" => return builtin_copystr(&args),
            "lowercase" => return builtin_lowercase(&args),
            "uppercase" => return builtin_uppercase(&args),
            "indexof" => return builtin_indexof(&args),
            "maxstrlen" => return builtin_maxstrlen(&args),
            "createdatetime" => return builtin_createdatetime(&args),
            "currentdatetime" => return Eval::Normal(Value::DateTime(clock_current_datetime())),
            "today" => return Eval::Normal(Value::Date(clock_today())),
            "time" => return Eval::Normal(Value::Time(clock_time())),
            "abs" => return builtin_abs(&args),
            "round" => return builtin_round(&args),
            "power" => return builtin_power(&args),
            "strpos" => return builtin_strpos(&args),
            "delchr" => return builtin_delchr(&args),
            "convertstr" => return builtin_convertstr(&args),
            "padstr" => return builtin_padstr(&args),
            "selectstr" => return builtin_selectstr(&args),
            "incstr" => return builtin_incstr(&args),
            "date2dmy" => return builtin_date2dmy(&args),
            "date2dwy" => return builtin_date2dwy(&args),
            "calcdate" => return builtin_calcdate(&args),
            "delstr" => return builtin_delstr(&args),
            "maximum" => return builtin_extreme(&args, true),
            "minimum" => return builtin_extreme(&args, false),
            "arraylen" => return builtin_arraylen(&args),
            "createguid" if args.is_empty() => return crate::stubs::any::guid_value(&args),
            "isnullguid" => {
                return match args.as_slice() {
                    [Value::Guid(guid)] => Eval::Normal(Value::Boolean(
                        guid.chars().all(|c| matches!(c, '0' | '-' | '{' | '}')),
                    )),
                    _ => eval_error("IsNullGuid expects a Guid"),
                }
            }
            "evaluate" => {
                // A failed Evaluate as a statement is a runtime error in BC;
                // in an expression (`if Evaluate(...)`) it is `false`.
                return match builtin_evaluate(&args) {
                    Ok((true, Some(value))) => {
                        ctx.var_writebacks.clear();
                        ctx.var_writebacks.push((0, value));
                        Eval::Normal(Value::Boolean(true))
                    }
                    Ok(_) if statement => eval_error(format!(
                        "Evaluate: '{}' is not a valid {}",
                        match &args[1] {
                            Value::Text(t) | Value::Code(t) => t.as_str(),
                            _ => "",
                        },
                        args[0].type_name()
                    )),
                    Ok(_) => Eval::Normal(Value::Boolean(false)),
                    Err(error) => eval_error(error),
                };
            }
            "dmy2date" => return builtin_dmy2date(&args, ctx),
            "dt2date" => return builtin_dt2date(&args),
            "dt2time" => return builtin_dt2time(&args),
            "workdate" => return builtin_workdate(&args, ctx),
            "random" => return builtin_random(&args, ctx),
            "randomize" => return builtin_randomize(&args, ctx),
            "getlasterrortext" => {
                if !args.is_empty() {
                    return eval_error("GetLastErrorText expects no arguments");
                }
                let text = ctx
                    .last_error
                    .as_ref()
                    .map(|error| error.message.clone())
                    .unwrap_or_default();
                return Eval::Normal(Value::Text(text));
            }
            "clearlasterror" => {
                if !args.is_empty() {
                    return eval_error("ClearLastError expects no arguments");
                }
                ctx.last_error = None;
                return Eval::Normal(Value::Empty);
            }
            _ => {}
        }
    }

    let args =
        match super::table_code::dispatch_table_procedure(receiver, procedure, args, stack, ctx) {
            Ok(result) => return result,
            Err(args) => args,
        };
    dispatch_workspace_procedure(receiver, procedure, args, stack, ctx)
}

/// Run a stub member that reads the interpreter context, which a context-free
/// [`stubs::StubFn`] cannot. `stubs::is_context_member` decides membership, so
/// an unmatched name here means the two lists drifted apart.
fn dispatch_stub_with_context(
    receiver: &str,
    procedure: &str,
    args: &[Value],
    ctx: &mut DispatchCtx,
) -> Eval {
    if procedure.eq_ignore_ascii_case("ExpectedError") {
        return crate::stubs::library_assert::expected_error(args, ctx.last_error.as_ref());
    }
    eval_error(format!(
        "stub member '{receiver}.{procedure}' is listed as context-aware but has no implementation"
    ))
}

/// True when `name` is a global (receiver-less) builtin the interpreter
/// implements natively — the single source of truth shared with the al-test
/// router: bare global calls to any *other* name have no local implementation
/// and must route to live BC. Every name listed here has a matching arm in
/// `dispatch_call_scoped` (or the niladic identifier fallback in
/// `eval_expr`); a unit test pins the agreement.
pub fn supports_global_builtin(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "error"
            | "message"
            | "confirm"
            | "strmenu"
            | "hyperlink"
            | "strsubstno"
            | "format"
            | "strlen"
            | "copystr"
            | "lowercase"
            | "uppercase"
            | "indexof"
            | "maxstrlen"
            | "createdatetime"
            | "currentdatetime"
            | "today"
            | "time"
            | "abs"
            | "round"
            | "power"
            | "strpos"
            | "delchr"
            | "convertstr"
            | "padstr"
            | "selectstr"
            | "incstr"
            | "date2dmy"
            | "date2dwy"
            | "calcdate"
            | "delstr"
            | "maximum"
            | "minimum"
            | "arraylen"
            | "createguid"
            | "isnullguid"
            | "evaluate"
            | "dmy2date"
            | "dt2date"
            | "dt2time"
            | "workdate"
            | "random"
            | "randomize"
            | "getlasterrortext"
            | "clearlasterror"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::interpreter::value::ErrorInfo;
    use crate::test_support::MockSource as Workspace;

    use crate::interpreter::dispatch::test_support::{ctx, error_of, ok};

    #[test]
    fn stub_routed_library_assert_are_equal() {
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
        let mut ctx = ctx();
        let result = dispatch_call(
            Some("Library Assert"),
            "AreEqual",
            vec![Value::Integer(1), Value::Integer(2)],
            &mut ctx,
        );
        let e = error_of(result);
        assert!(
            e.message.contains("expected"),
            "expected message containing 'expected', got: {}",
            e.message
        );
    }

    #[test]
    fn unknown_procedure_returns_descriptive_error() {
        let mut ctx = ctx();
        let result = dispatch_call(None, "CompletelyUnknownProc", vec![], &mut ctx);
        let e = error_of(result);
        assert!(
            e.message.contains("CompletelyUnknownProc"),
            "expected procedure name in error, got: {}",
            e.message
        );
        assert!(
            e.message.contains("not found"),
            "expected 'not found' in error, got: {}",
            e.message
        );
    }

    #[test]
    fn explicit_workspace_receiver_is_not_hijacked_by_builtin_name() {
        let ws = Arc::new(Workspace::new());
        ws.file_index.add_file(
            std::path::PathBuf::from("/test/BuiltinCollision.al"),
            r#"codeunit 50998 "Builtin Collision"
{
    procedure Format(): Text
    begin
        exit('workspace method');
    end;
}"#
            .to_string(),
        );
        let mut ctx = DispatchCtx::new_pure(ws);

        let result = dispatch_call(Some("Builtin Collision"), "Format", vec![], &mut ctx);
        assert_eq!(ok(result), Value::Text("workspace method".to_string()));
    }

    #[test]
    fn every_supported_global_builtin_dispatches_without_procedure_not_found() {
        // The router's safe-list and the dispatch catalog share
        // `supports_global_builtin`; this pins that every listed name actually
        // has a dispatch arm (a zero-argument call may fail its own argument
        // validation, but must never fall through to "procedure not found").
        let names = [
            "Error",
            "Message",
            "Confirm",
            "StrMenu",
            "Hyperlink",
            "StrSubstNo",
            "Format",
            "StrLen",
            "CopyStr",
            "LowerCase",
            "UpperCase",
            "IndexOf",
            "MaxStrLen",
            "CreateDateTime",
            "CurrentDateTime",
            "Today",
            "Time",
            "Abs",
            "Round",
            "Power",
            "StrPos",
            "DelChr",
            "ConvertStr",
            "PadStr",
            "SelectStr",
            "IncStr",
            "Date2DMY",
            "Date2DWY",
            "CalcDate",
            "DelStr",
            "Maximum",
            "Minimum",
            "ArrayLen",
            "CreateGuid",
            "IsNullGuid",
            "Evaluate",
            "DMY2Date",
            "DT2Date",
            "DT2Time",
            "WorkDate",
            "Random",
            "Randomize",
            "GetLastErrorText",
            "ClearLastError",
        ];
        for name in names {
            assert!(
                supports_global_builtin(name),
                "{name} must be in the shared safe-list"
            );
            let mut ctx = ctx();
            let result = dispatch_call(None, name, vec![], &mut ctx);
            if let Eval::Error(e) = &result {
                assert!(
                    !e.message.contains("procedure not found"),
                    "{name} must dispatch to a builtin, got: {}",
                    e.message
                );
            }
        }
        assert!(
            !supports_global_builtin("GlobalLanguage"),
            "unimplemented globals must stay off the safe-list"
        );
        assert!(!supports_global_builtin("ApplicationPath"));
    }

    #[test]
    fn get_last_error_text_reads_and_clears() {
        let mut ctx = ctx();
        ctx.last_error = Some(ErrorInfo {
            message: "boom".into(),
            error_type: None,
            source: None,
        });
        assert_eq!(
            ok(dispatch_call(None, "GetLastErrorText", vec![], &mut ctx)),
            Value::Text("boom".into())
        );
        assert!(matches!(
            dispatch_call(None, "ClearLastError", vec![], &mut ctx),
            Eval::Normal(_)
        ));
        assert_eq!(
            ok(dispatch_call(None, "GetLastErrorText", vec![], &mut ctx)),
            Value::Text(String::new())
        );
    }
}
