//! Semantic analysis subsystem — .NET CodeAnalysis bridge + lifecycle + cache.
//!
//! Folded into al-core in stage 6 of the crate consolidation. Splits across:
//! - `bridge` — `SemanticBridge`, request/response types, JSON-over-FFI calls (was `crate::semantic` lib)
//! - `host` — netcorehost wrapper that loads the .NET runtime + AlBridge.dll
//! - `cache` — disk cache for builtins and error codes (keyed by toolchain version)
//! - `lifecycle` — bridge initialisation, lazy startup, crash detection + restart,
//!   `SemanticCache` (in-memory builtin index)

pub mod bridge;
pub mod cache;
pub(crate) mod host;
pub mod lifecycle;

pub use bridge::{
    AnalyzeRequest, BuiltinMethod, BuiltinType, CompileResult, CompletionItem, DiagnosticEntry,
    ErrorCodeInfo, MethodParameter, SemanticBridge, SemanticError, TypeInfo,
};
pub use lifecycle::{
    get_or_init_bridge, restart_bridge, set_builtins, shutdown_bridge, SemanticCache,
};
