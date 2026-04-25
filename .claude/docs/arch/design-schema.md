# Design Document Schema

Mirror of [`.claude/docs/agentic/schemas/arch-design.md`](../agentic/schemas/arch-design.md).
This copy lives under `docs/arch/` for discoverability from within the
Architecture Department. The authoritative version is in `schemas/`;
if they ever disagree, `schemas/` wins.

See that file for:
- Required frontmatter (schema_version, task_id, run_id, status,
  iterations, reviewed_by, owner_crate, recommended_option).
- Required body sections in order (Problem, Current state, optional
  "What we know now", Options with ≥2 options, Recommended option,
  Execution plan, Roll-back plan).
- arch-critic verdicts.
- Validator command.

## How to write a good design (guidance, not schema)

- Lead with the **problem**, not the solution. One paragraph.
- Make options genuinely different. If Option A and Option B are
  one-line-diff variants, you have one option, not two.
- Every option declares its **boundary impact** — which crates' public
  API changes. The Dev Department uses this to decide whether the
  implementer gets multi-crate scope.
- The Execution plan is a sequence of commits. Each step should be
  small enough that it's meaningful to git-bisect.
- The roll-back plan matters. Most of these designs are done for
  working code; you're proposing change. Know the retreat path.
