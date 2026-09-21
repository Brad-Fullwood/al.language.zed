# desloppify triage

Tool: desloppify (https://github.com/peteromallet/desloppify), `--lang rust`, state in `.desloppify/`.
Branch: `campaign/fix-r1-extension-ci`. Review, triage and planning only. No source file was edited
in this pass: six fix agents hold `crates/`, `src/`, `tree-sitter-al/` and `scripts/` in other worktrees.

Repo at scan time: 315 files, 231K LOC, 60 dirs, 1243 open issues.

## Scores

(filled in below, see "All scores")

## 1. Security issues

Four open, all `hardcoded_secret_name`, all `[high]` by the detector's default severity.

| # | Path:line | Detector verdict | Real? | Why |
|---|---|---|---|---|
| 1 | `crates/al-symbols/src/oauth.rs:1011` | Hardcoded secret in variable `access_token` | False positive | Literal `"super-secret-bearer"` inside `#[cfg(test)] mod zeroize_tests` (module opens at line 1002). It is the input to `zeroize_wipes_token_response_secrets`, which asserts the field is wiped. |
| 2 | `crates/al-symbols/src/oauth.rs:1030` | Hardcoded secret in variable `access_token` | False positive | Literal `"secret-access"` in the same test module, feeding `zeroize_wipes_cached_token_secrets_and_keeps_metadata`. |
| 3 | `crates/al-symbols/src/oauth.rs:1051` | Hardcoded secret in variable `access_token` | False positive | Literal `"only-access"` in the same test module, covering the `refresh_token: None` path of `zeroize`. |
| 4 | `crates/al-dap/src/dap/bc_debug/wire.rs:524` | Hardcoded secret in variable `token` | False positive | `let token = "token=value&other=x";` in `#[cfg(test)] mod tests` (opens at line 236). It is a query-string sample for `percent_encode_url`, not a credential. The variable name `token` is what tripped the name-based detector. |

No real credential is committed. The three oauth.rs hits are, if anything, evidence of the opposite:
the code carries a `Zeroize` implementation on `TokenResponse` and `CachedToken` with
`#[zeroize(skip)]` on the non-secret fields, and those tests exist to prove the wipe happens.

### Fix

No code change. The detector is name-based and does not respect `#[cfg(test)]` module boundaries
inside a production-zone file. Recorded as suppressions with reasons (section 3).

If someone wants a code-side fix instead of a suppression, renaming the wire.rs local from `token`
to `raw_query` would silence hit 4 and read better, but that file is held by another agent and the
rename is cosmetic.

## 2. Subjective review

## 3. Detector false-positive classes and suppressions

## 4. Fix batches

## All scores
