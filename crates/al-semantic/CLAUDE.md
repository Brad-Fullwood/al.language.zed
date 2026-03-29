# al-semantic — .NET CLR Bridge (~1K lines)

**Leaf crate** — must NOT depend on al-syntax, al-symbols, or al-core.

Hosts .NET CLR in-process via `netcorehost` for Microsoft.Dynamics.Nav.CodeAnalysis.

## Key Constraint

All CLR calls are **Mutex-serialized on a blocking thread with 30s timeout**. Never call DotNetHost methods from multiple threads directly.

## Modules

| File | Purpose |
|------|---------|
| lib.rs | `SemanticBridge` — public API, initialization, managed method calls |
| host.rs | .NET CLR hosting via netcorehost |
| cache.rs | Semantic analysis result caching |

## Degraded Mode

When ALTool/.NET SDK is absent, the extension gracefully degrades:
- **Works:** highlighting, folding, navigation, completions from symbol index
- **Unavailable:** semantic analysis, compilation, debugging
- al-core handles None/error from SemanticBridge gracefully — never crashes
