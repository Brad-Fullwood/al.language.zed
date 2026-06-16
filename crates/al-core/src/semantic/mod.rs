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

/// User-facing error when native-first compilation cannot run because the
/// in-process CodeAnalysis bridge is unavailable and the user has not opted
/// into Microsoft's `dotnet alc` subprocess. Shared by every compile path so
/// the "fail loud, no silent fallback" message is identical everywhere.
pub const NATIVE_COMPILER_UNAVAILABLE: &str =
    "Native compiler unavailable: the in-process CodeAnalysis bridge could not be initialized \
     (binary built without the `semantic` feature, missing .NET runtime, or CLR init failed). \
     Native-first compilation is enabled with no silent fallback. Fix the bridge, or set \
     `al.useOfficialCompiler: true` to compile via Microsoft's `dotnet alc` subprocess.";
