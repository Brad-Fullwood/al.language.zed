# `needs_design` criteria

These are the rules the Review reducer uses to set `needs_design: true`
on tasks in `handoff.json`. Keep this in sync with
`review-reducer.md`'s prompt if the rules change.

Any task satisfying AT LEAST ONE of the rules below gets
`needs_design: true`:

1. `kind == "refactor"` AND `scope_estimate` is `L` or `XL`, OR the
   free-text `scope_estimate` mentions more than one crate.
2. `owner_crate` has more than one crate name.
3. `category == "architecture"` AND the finding touches a CLAUDE.md
   hard constraint (dependency direction; layering; WASM-native
   isolation; `al_core::server` business-logic ban; no-hardcoded-AL-values).
4. `fix` is null, empty, or literally "design needed" / "TBD" /
   "open question".
5. `kind == "gap"` AND the gap requires adding a new public API
   (method/type) on a crate boundary (e.g., a new `al-core` query
   function signature).

## Tasks that DO NOT need design

- Any task with a concrete single-file `fix` description.
- All `kind: doc` tasks.
- All `kind: bug` tasks where the reproduction is already clear and
  the cause is identified.
- Any task where `acceptance_criteria` already lists the expected
  behaviour precisely (the test-writer can derive the implementation
  from the criteria).

## Why these rules

- **Scope > 1 crate** → Dev's per-task implementer is scoped to one
  crate via the path-scope hook. Cross-crate work needs an explicit
  design.
- **CLAUDE.md hard constraint** → violating these is a restructuring
  job, not a patch.
- **Empty fix** → Review couldn't suggest a fix; the implementer
  shouldn't have to.
- **New cross-boundary API** → the design is the right place to pick
  the signature; ad-hoc invention during Dev is how leaky APIs are born.
