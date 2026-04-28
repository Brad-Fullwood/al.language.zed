#!/usr/bin/env bash
# PreToolUse hook: blocks adding native dependencies to the root Cargo.toml.
#
# The root crate is `zed-al`, the WASM extension. It must compile to
# wasm32-wasip1 and therefore cannot depend on native crates (tree-sitter,
# tower-lsp, netcorehost, reqwest, dashmap, etc.).
#
# Allowlist for `[dependencies]` in root Cargo.toml:
#   - zed_extension_api
#   - serde
#   - serde_json
#
# Workspace deps under `[workspace.dependencies]` are fine — they only
# affect crates that opt in.
#
# Triggered on Write|Edit of the root Cargo.toml. Crate-level Cargo.tomls
# (crates/*/Cargo.toml) are unaffected.

set -euo pipefail

FILE="${CLAUDE_FILE_PATH:-}"
[ -z "$FILE" ] && exit 0

# Only act on the *root* Cargo.toml.
ROOT_CARGO="$(realpath "$CLAUDE_PROJECT_DIR/Cargo.toml" 2>/dev/null || echo "")"
TARGET="$(realpath "$FILE" 2>/dev/null || echo "")"
[ "$TARGET" = "$ROOT_CARGO" ] || exit 0

# The new content is in CLAUDE_TOOL_INPUT for Write, or we re-read the file
# after the proposed edit. Simplest portable approach: read what's currently
# on disk *plus* check that the in-flight tool's parameters don't introduce
# disallowed deps. For Edit/MultiEdit the file hasn't been written yet, so
# we look at the raw new_string content.

# Pull candidate added lines from the tool input. Anthropic exposes the raw
# JSON via CLAUDE_TOOL_INPUT_JSON when available; fall back to scanning the
# tool input env vars individually.
CANDIDATE="${CLAUDE_TOOL_INPUT_JSON:-}"
if [ -z "$CANDIDATE" ]; then
  CANDIDATE="${CLAUDE_TOOL_NEW_STRING:-}${CLAUDE_TOOL_CONTENT:-}"
fi

# If we couldn't determine candidate text, fall back to scanning the file
# after the operation completes. PreToolUse runs *before* the write, so a
# fallback here won't catch in-flight edits — we just exit clean.
if [ -z "$CANDIDATE" ]; then
  exit 0
fi

# Restrict the search to dependency lines: `name = ...` or `name = { ... }`
# but only if they appear under a [dependencies] section. We approximate by
# scanning for known disallowed crate names directly.
DISALLOWED_PATTERN='^[[:space:]]*(tokio|tower-lsp|tower|tree-sitter|tree-sitter-[a-zA-Z0-9_-]+|dashmap|ropey|reqwest|zip|quick-xml|memmap2|petgraph|netcorehost|hyper|axum|sqlx|rusqlite|notify|walkdir|crossbeam[a-zA-Z_-]*|rayon|tonic|prost|hyper-tls|native-tls|openssl|rustls|tokio-[a-zA-Z_-]+|async-[a-zA-Z_-]+)[[:space:]]*='

if printf '%s' "$CANDIDATE" | grep -qE "$DISALLOWED_PATTERN"; then
  cat >&2 <<EOF
BLOCKED: Adding a native dependency to the root Cargo.toml.

The root crate (zed-al) compiles to wasm32-wasip1 and cannot depend on
native crates. Common offenders detected: tokio, tower-lsp, tree-sitter,
dashmap, reqwest, etc.

If this dep is meant for a specific native crate (al-core, al-explorer,
etc.), put it in that crate's Cargo.toml instead. If it's shared, add it
under [workspace.dependencies] (not [dependencies]) and have the consuming
crates opt in.

See CLAUDE.md "Common Agent Mistakes" #3.
EOF
  exit 2
fi

exit 0
