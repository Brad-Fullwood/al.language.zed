//! The AL semantic subsystem: the .NET CodeAnalysis bridge,
//! its CLR host, the disk cache, and the in-memory `SemanticCache`.
//!
//! - `bridge` — `SemanticBridge`, request/response types, JSON-over-FFI calls
//! - `host` — netcorehost wrapper that loads the .NET runtime + AlBridge.dll
//! - `cache` — disk cache for builtins and error codes (keyed by toolchain version)
//! - `lifecycle` — the `SemanticCache` in-memory builtin index

pub mod bridge;
pub mod cache;
pub(crate) mod host;
pub mod lifecycle;

pub use bridge::{
    AnalyzeRequest, BuiltinMethod, BuiltinType, CompletionItem, DiagnosticEntry, ErrorCodeInfo,
    MethodParameter, SemanticBridge, SemanticError, TypeInfo,
};
pub use lifecycle::SemanticCache;
