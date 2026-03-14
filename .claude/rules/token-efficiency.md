# Token Efficiency Rules (Always Loaded)

## Command Output
- **Always** pipe cargo/test output through `| tail -N` (use 20-30 lines).
- For failures, show only the first 3 failing tests with 3 lines of context each.

## Agent Spawning
- Use **sonnet** for all subagents. Opus is reserved for the main session only.
- Do NOT spawn agents when a shell command suffices.
- For parallel work: use `superpowers:dispatching-parallel-agents`.
- For code review: use `superpowers:requesting-code-review`.

## Communication
- Skip preamble. "Fixed." is better than "I've identified and resolved the compilation error."
- Don't repeat what hooks already told you. If a hook said "COMPILE ERROR", just fix it.

## Continuity
- **Never stop between tasks.** After task completion, immediately start the next one.
- Adversarial agents run in the background — zero blocking cost.
