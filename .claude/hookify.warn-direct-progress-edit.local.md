---
name: warn-direct-task-completion
enabled: true
event: file
action: warn
conditions:
  - field: file_path
    operator: regex_match
    pattern: \.claude/data/tasks\.toml$
  - field: new_text
    operator: contains
    pattern: "completed = true"
---

**Are you using `/complete-task` to mark this task complete?**

Do not edit tasks.toml directly to set `completed = true`. Use `/complete-task <task_id> <wp_name>` instead — it runs tests, records proof, updates the task, and spawns the bug finder as one atomic action.

If `/complete-task` is currently running and this edit is part of its Step 4, ignore this warning.
