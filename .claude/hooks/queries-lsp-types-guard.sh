#!/usr/bin/env bash
# PostToolUse hook: blocks `lsp_types::*` from appearing in public function
# signatures inside crates/al-core/src/queries/.
#
# CLAUDE.md: query functions in al_core::queries::* must return
# transport-agnostic types. The al_core::server module converts to LSP
# types at the boundary. This rule is no longer compiler-enforced (al-lsp
# folded into al-core), so it's a coding discipline — enforce here.

set -euo pipefail

FILE="${CLAUDE_FILE_PATH:-}"
[ -z "$FILE" ] && exit 0

# Only act on Rust files inside crates/al-core/src/queries/.
case "$FILE" in
  */crates/al-core/src/queries/*.rs) ;;
  *) exit 0 ;;
esac

[ -f "$FILE" ] || exit 0

# Look for `pub fn`/`pub async fn`/`pub(crate) fn` whose signature line
# (or the next two continuation lines) mentions lsp_types::.
# Use grep -A 2 to capture multi-line signatures.
OFFENDERS="$(grep -nE '^[[:space:]]*pub(\([^)]+\))?[[:space:]]+(async[[:space:]]+)?fn[[:space:]]+' "$FILE" \
  | awk -F: '{print $1}' \
  | while read -r LN; do
      # Read up to 6 lines from $LN to capture multi-line sigs.
      SIG="$(sed -n "${LN},$((LN+5))p" "$FILE")"
      if echo "$SIG" | grep -q 'lsp_types::'; then
        echo "$FILE:$LN"
      fi
    done)"

if [ -n "$OFFENDERS" ]; then
  cat >&2 <<EOF
BLOCKED: lsp_types::* found in a public signature inside al-core::queries::*.

Query functions must return transport-agnostic native types (Option<T>,
Result<T, E>, plain structs). LSP-specific types (lsp_types::Hover,
lsp_types::CompletionItem, lsp_types::Location, etc.) belong only in
al-core::server, which converts at the transport boundary.

Offending location(s):
$OFFENDERS

Fix: move the lsp_types::* construction to the corresponding handler in
crates/al-core/src/server/, and have the query return a plain struct or
enum that the server maps to lsp_types.

See CLAUDE.md "The One Rule of Architecture".
EOF
  exit 2
fi

exit 0
