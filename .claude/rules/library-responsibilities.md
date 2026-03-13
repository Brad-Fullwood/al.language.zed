# Library Responsibilities & Code Reuse
**Status: Non-Negotiable**

Every crate in the workspace has a strictly defined set of responsibilities as documented in `crates-map.md`.

### Rules:
1. **Strict Boundary Adherence**: A library must never implement logic that falls under the jurisdiction of another crate. 
2. **Reuse over Duplication**: If functionality is needed that already exists in another library, that library must be imported and reused. Duplicating logic across library boundaries is a critical architectural failure.
3. **Single Source of Truth**: Shared data types and business logic (e.g., `AppDependency`, `SymbolEntry`) must live in their designated owner crate only.

### Verification:
During every **Surgical Audit**, code must be checked for logic leakage or "shadow" implementations of another library's responsibility. Any duplicated code must be deleted and replaced with a dependency on the authoritative crate.
