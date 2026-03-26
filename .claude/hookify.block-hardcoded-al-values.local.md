---
name: block-hardcoded-al-values
enabled: true
event: file
action: block
conditions:
  - field: file_path
    operator: regex_match
    pattern: \.rs$
  - field: new_text
    operator: regex_match
    pattern: (AL_BUILTIN_FUNCTIONS|AL_OBJECT_BODY_KEYWORDS|GLOBAL_BUILTINS|AL_KEYWORDS|BUILTIN_TYPES|AL_SYSTEM_FUNCTIONS|AL_DATA_TYPES|TRIGGER_NAMES|AL_RESERVED_WORDS|const\s+\w+\s*:\s*&\[&str\]\s*=\s*&\[[\s\S]*?(begin|procedure|codeunit|trigger|record|page|report|xmlport|query|enum|interface|entitlement|permissionset)\b)
---

**BLOCKED: Hardcoded AL language values detected**

You are introducing hardcoded AL language keywords, built-in functions, or type lists. This violates the project's fundamental architecture.

AL is a living language updated every BC release. Hardcoded lists become stale immediately.

**Instead use:**
- `tree-sitter-al/generator/tools/al-extract/` to dynamically extract values from Microsoft DLLs
- `al-symbols` for runtime symbol data from `.app` packages
- `al-semantic` bridge for built-in types and functions from .NET CLR
- If the extraction pipeline doesn't have what you need, **update the generator** — do NOT hardcode

**Violations include:** `const AL_BUILTIN_FUNCTIONS`, `const AL_OBJECT_BODY_KEYWORDS`, `const GLOBAL_BUILTINS`, or any `&[&str]` literal containing AL language tokens.
