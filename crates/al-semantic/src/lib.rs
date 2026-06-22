//! `al-semantic`: the AL semantic subsystem — the .NET CodeAnalysis bridge,
//! its CLR host, the disk cache, and the in-memory `SemanticCache`.
//!
//! - `bridge` — `SemanticBridge`, request/response types, JSON-over-FFI calls
//! - `host` — netcorehost wrapper that loads the .NET runtime + AlBridge.dll
//! - `cache` — disk cache for builtins and error codes (keyed by toolchain version)
//! - `lifecycle` — the `SemanticCache` in-memory builtin index
//!
//! Bridge *lifecycle orchestration* (lazy startup, crash/restart, shutdown)
//! lives in the workspace layer, since it operates on the `Workspace` hub.

pub mod bridge;
pub mod cache;
pub(crate) mod host;
pub mod lifecycle;

pub use bridge::{
    AnalyzeRequest, BuiltinMethod, BuiltinType, CompileResult, CompletionItem, DiagnosticEntry,
    ErrorCodeInfo, MethodParameter, SemanticBridge, SemanticError, TypeInfo,
};
pub use lifecycle::SemanticCache;

/// Legacy bridge-compile error text retained for compatibility with older
/// callers. The native `.app` emitter no longer depends on the CodeAnalysis
/// bridge; this message now only describes semantic bridge availability.
#[deprecated(note = "native `.app` compilation no longer depends on the CodeAnalysis bridge")]
pub const NATIVE_COMPILER_UNAVAILABLE: &str =
    "Semantic CodeAnalysis bridge unavailable: the in-process .NET bridge could not be initialized \
     (binary built without the `semantic` feature, missing .NET runtime, or CLR init failed). \
     Native `.app` emission does not require this bridge; set `al.useOfficialCompiler: true` only \
     when you intentionally want Microsoft's `dotnet alc` subprocess.";
