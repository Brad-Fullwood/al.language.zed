---
name: show-progress
description: Show project progress — completed tasks, current status, what's next, blockers
user_invocable: true
---

# Show Progress

Generate a concise progress report.

## Steps

### 1. Read Index (lightweight)

Read `.claude/data/task-index.toml` (84 lines). Count completed vs total per WP from the header comments and task entries.

### 2. Count Proof Entries

```bash
python3 -c "
import tomllib
with open('.claude/data/proof.toml','rb') as f: d=tomllib.load(f)
print(f'PoF entries: {len(d.get(\"entries\",[]))}')
"
```

### 3. Count Open Issues

```bash
python3 -c "
import tomllib
with open('.claude/data/issues.toml','rb') as f: d=tomllib.load(f)
open_issues = [i for i in d['issues'] if i['status']=='open']
print(f'Open issues: {len(open_issues)} ({sum(1 for i in open_issues if i.get(\"triage\")==\"STOP\")} STOP)')
"
```

### 4. Report

```
## Progress — [DATE]

### Summary
- **Completed**: X/Y tasks (Z%)
- **Current WP**: WPX — [NAME] (A/B tasks done)
- **Next task**: TXXX — [NAME]
- **PoF entries**: N
- **Open issues**: N (M STOP)

### WP Status
WP0: DONE | WP1: DONE | ... | WP9: 4/8 | WP10: 0/3 | WP11: 0/5

### Next Steps
1. TXXX — [DESCRIPTION]
2. TXXX — [DESCRIPTION]
```
