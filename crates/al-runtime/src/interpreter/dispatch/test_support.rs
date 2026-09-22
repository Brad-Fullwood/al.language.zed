//! Fixtures the dispatch test modules share.

use std::sync::Arc;

use crate::interpreter::scope::Eval;
use crate::interpreter::value::{ErrorInfo, Value};
use crate::test_support::MockSource as Workspace;

use super::DispatchCtx;

pub(crate) fn ctx() -> DispatchCtx {
    DispatchCtx::new_pure(Arc::new(Workspace::new()))
}

pub(crate) fn ok(eval: Eval) -> Value {
    match eval {
        Eval::Normal(v) => v,
        Eval::Error(e) => panic!("unexpected error: {}", e.message),
        Eval::Exit(v) => v,
        Eval::Break | Eval::Continue => panic!("unexpected break/continue"),
    }
}

pub(crate) fn error_of(eval: Eval) -> ErrorInfo {
    match eval {
        Eval::Error(e) => e,
        Eval::Normal(v) => panic!("expected error, got Normal({})", v.type_name()),
        Eval::Exit(v) => panic!("expected error, got Exit({})", v.type_name()),
        Eval::Break | Eval::Continue => panic!("expected error, got break/continue"),
    }
}

pub(crate) fn workspace_with_helper() -> Arc<Workspace> {
    let ws = Arc::new(Workspace::new());
    let source = r#"codeunit 50999 "Helper"
{
procedure Add(a: Integer; b: Integer): Integer
begin
    exit(a + b);
end;

procedure Forever()
begin
    Forever();
end;
}
"#;
    ws.file_index.add_file(
        std::path::PathBuf::from("/test/Helper.al"),
        source.to_string(),
    );
    ws
}
