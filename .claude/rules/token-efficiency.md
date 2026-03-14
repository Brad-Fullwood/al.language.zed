# Token Efficiency Rules (Always Loaded)

## Command Output
- **Always** pipe cargo/test output through `| tail -N` (use 20-30 lines). Full build output wastes thousands of tokens.
- When all tests pass, report one line: "X passed, Y ignored". Don't echo the full output.
- For failures, show only the first 3 failing tests with 3 lines of context each.

## Agent Spawning
- **Do NOT spawn agents for mechanical tasks** that a shell command can do (running tests, checking compilation, counting files).
- Agents are for tasks requiring **reasoning**: adversarial testing, supervision triage, architecture review.
- Use **sonnet** for all subagents. Opus is reserved for the main session only.
- Limit agent turns: adversarial 15, supervisor 15, guardian 12, auditor 15.

## Proof of Functionality
- Pipe real command output directly into PoF entries: `cargo test 2>&1 | tail -20`
- Do NOT have Claude rewrite or summarize test output — paste the raw tail.

## Communication
- Supervision reports: 5 lines max unless there are STOP items.
- Don't repeat what hooks already told you. If a hook said "COMPILE ERROR in al-core", just fix it — don't echo the message back.
- Skip preamble. "Fixed." is better than "I've identified and resolved the compilation error."

## WPX: Adversarial Enforcement
- The adversarial agent MUST be spawned in background after every supervision cycle.
- The track-edits hook reminds every 5 .rs edits. Obey it.
- Record adversarial runs: `cat /tmp/al-edit-count > /tmp/al-adversarial-last-run`
