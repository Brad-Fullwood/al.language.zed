---
name: block-bare-ok
enabled: false
event: file
action: warn
conditions:
  - field: file_path
    operator: regex_match
    pattern: \.rs$
  - field: new_text
    operator: contains
    pattern: Ok(())
---

**Bare `Ok(())` detected**

Functions should return meaningful results, not bare `Ok(())`.

Consider:
- Return a value that callers can use
- Use a more descriptive return type
- If this is truly a side-effect-only function, document why `Ok(())` is appropriate

Exception: test functions and setup/teardown code may use `Ok(())`.
