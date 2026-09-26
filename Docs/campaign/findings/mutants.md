# Mutation testing pass, campaign/test-mutants

`cargo mutants` over the shortlist in `test-depth.md`. Each missed mutant is a line that
the crate's own tests execute but do not check. The verdict per mutant is `test added
<sha>`, `equivalent <why>`, or `deferred <why>`.

Branch: `campaign/test-mutants`, cut from `campaign/2026-09-21` at fa4fcf8f.

## Real bugs

None found so far.

## Setup

- cargo-mutants 27.1.0, installed with `cargo install cargo-mutants --locked`.
- Every run uses
  `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3`
  and `cargo mutants --in-place -p <crate> --file <path>`. cargo-mutants 27 rejects
  `--jobs` together with `--in-place`, and `--in-place` runs one mutant at a time. The repository
  `.cargo/mutants.toml` excludes `build.rs`, `impl Debug`, `impl Display` and `fn fmt`.
- `-p <crate>` runs only that crate's tests. A mutant that another crate's tests would
  catch still counts as missed here.

## Shortlist mapped to current paths

| Shortlist entry | Current path |
| --- | --- |
| `crates/al-syntax/src/formatting.rs` | `crates/al-syntax/src/formatting/{mod,casing,indent,options,passes,range,text}.rs`, split in c8f38561 |
| `crates/al-syntax/src/sort.rs` | unchanged |
| `crates/al-syntax/src/lint.rs` | unchanged |
| `crates/al-runtime/src/mock/record.rs` | unchanged |
| `crates/al-runtime/src/mock/filter.rs` | unchanged |
| `crates/al-source/src/documents.rs` | unchanged |
| `crates/al-symbols/src/composition.rs` | unchanged |
| `crates/al-emit/src/method_id.rs` | unchanged |
| `crates/al-test/src/output/cobertura.rs` | unchanged |
| `crates/al-bc/src/http_auth.rs` | unchanged |

## Summary

| File | Mutants | Caught | Missed | Unviable | Timeout |
| --- | ---: | ---: | ---: | ---: | ---: |
| `al-emit/src/method_id.rs` | 42 | 40 | 2 | 0 | 0 |
| `al-bc/src/http_auth.rs` | 26 | 21 | 3 | 2 | 0 |

## Runs

### al-emit: `crates/al-emit/src/method_id.rs`

```bash
cargo mutants --in-place -p al-emit --file crates/al-emit/src/method_id.rs
```

42 mutants in 5 minutes: 40 caught, 2 missed, 0 unviable, 0 timeout.

`missed.txt`:

```text
crates/al-emit/src/method_id.rs:60:30: replace % with / in adjust_for_system_codeunits
crates/al-emit/src/method_id.rs:60:30: replace % with + in adjust_for_system_codeunits
```

- `method_id.rs:60`, `%` to `/`: test added f5e9269a. Every alc vector came from a user
  codeunit (id 50100), so the fold for system codeunits ran in no test. The new test uses
  four method ids from the `SymbolReference.json` of Microsoft's BC 28 System symbols
  package, which confirms the fold against alc output, plus a boundary test at
  2_000_000_000.
- `method_id.rs:60`, `%` to `+`: test added f5e9269a, same test.

Re-run with `--re adjust_for_system_codeunits`: 6 mutants, 6 caught.

### al-bc: `crates/al-bc/src/http_auth.rs`

```bash
cargo mutants --in-place -p al-bc --file crates/al-bc/src/http_auth.rs
```

26 mutants in 2 minutes: 21 caught, 3 missed, 2 unviable, 0 timeout.

`missed.txt`:

```text
crates/al-bc/src/http_auth.rs:87:5: replace warn_insecure_tls with ()
crates/al-bc/src/http_auth.rs:104:5: replace build_http_client -> Result<reqwest::Client, reqwest::Error> with Ok(Default::default())
crates/al-bc/src/http_auth.rs:137:27: replace && with || in apply_snapshot_auth
```

- `http_auth.rs:87`, `warn_insecure_tls` emptied: test added c13d58b5. The tests only
  checked the message text. The new test captures the `tracing` output of
  `build_http_client(true, …)` and checks for a WARN line naming the surface, and checks
  that `build_http_client(false, …)` logs nothing.
- `http_auth.rs:104`, `build_http_client` returns a default client: test added c13d58b5.
  The tests only checked `is_ok()`. The new test points a client with a 1 second timeout
  at a local socket that accepts and never answers, and expects a reqwest timeout error
  within 10 seconds.
- `http_auth.rs:137`, `&&` to `||`: test added c13d58b5. With a username and no password
  the mutant sends no `Authorization` header. The new test expects the bearer override for
  a username without a password and for a password without a username.

Re-run of the whole file after c13d58b5: 26 mutants, 24 caught, 2 unviable.

Not covered by any mutant: `danger_accept_invalid_certs(accept_invalid_certs)`.
cargo-mutants does not mutate a boolean argument, and checking it needs a TLS server with
a self-signed certificate.
