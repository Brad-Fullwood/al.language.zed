# al-symbols Audit Report

**Files reviewed:** 16 | **Lines:** ~6K | **Issues:** 4 CRITICAL, 6 HIGH, 8 MEDIUM, 7 LOW

## Architecture: Clean
No imports from al-core, al-syntax, or al-semantic.

## CRITICAL

| ID | File | Line | Description |
|----|------|------|-------------|
| C1 | oauth.rs | 511 | `getrandom::expect()` panics in library code |
| C2 | language_data.rs | 33, 41 | `LazyLock::expect` panics on malformed JSON |
| C3 | nuget.rs | 386-396 | Truncated .app file left on disk after size bomb check |
| C4 | virtual_file.rs | 32-40 | TOCTOU race on cache file creation |

## HIGH

| ID | File | Line | Category | Description |
|----|------|------|----------|-------------|
| H1 | nuget.rs | 163 | bug | `reqwest::Client` has no timeout |
| H2 | nuget.rs | 220 | bug | HTTP status not checked before JSON parse |
| H3 | nuget.rs, bc_server.rs | 281-293, 118-130 | security | Full response buffered; Content-Length bypass |
| H4 | oauth.rs | 408-450 | bug | No HTTP 429 / Retry-After handling |
| H5 | nuget.rs | 382-396 | bug | Partial .app not cleaned up on extraction failure |
| H6 | nuget.rs | 229-265 | bug | Version prefix sort uses lexicographic not semver |

## IMPORTANT

| ID | File | Line | Description |
|----|------|------|-------------|
| I1 | oauth.rs | 713 | Refresh token loss not surfaced to user |
| I2 | nuget.rs | 50-56 | `NuGetFeed::default()` is public gallery, not BC feed |
| I3 | index.rs | 114-119 | Silent warn on failed .app load; no error surfacing |
| I4 | app_reader.rs | 113-118 | Linear ZIP-sig scan has no depth cap (200MB scan) |
| I5 | source_index.rs | 22 | Global static cache never evicts entries |
| I6 | source_index.rs | 113-127 | Double-build race on concurrent get_or_build |
| I7 | manifest.rs | 57-69 | XML attributes not unescaped (`&amp;` not decoded) |

## MEDIUM

| ID | File | Line | Description |
|----|------|------|-------------|
| M1 | language_data.rs | - | No test for embedded JSON parsing |
| M2 | virtual_file.rs | - | No tests for virtual_file module |
| M3 | bc_server.rs | 228-254 | Trivially thin tests; no HTTP error path coverage |
| M4 | app_reader.rs | 151-159 | Second full JSON parse in fallback path |
| M5 | model.rs | 99 | Hardcoded variant list in ObjectKind::from_str error |
| M6 | index.rs | 349-365 | `search_in_package` O(n) with no per-package index |
| M7 | index.rs | 512 | Raw pointer set lacks safety comment |
| M8 | model.rs | 715-746 | Non-deterministic synthetic enum values |
