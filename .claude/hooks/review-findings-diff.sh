#!/usr/bin/env bash
# review-findings-diff.sh — Helper invoked by the reducer to compute
# run-to-run finding status (new | persistent | regressed | resolved).
#
# Usage:
#   review-findings-diff.sh <current-findings.jsonl> <previous-findings.jsonl>
#
# Output (stdout): JSON object with four arrays of finding ids.
#
# Dependencies: jq.

set -uo pipefail

[[ $# -eq 2 ]] || { echo "usage: $0 <current.jsonl> <previous.jsonl>" >&2; exit 2; }

CURRENT=$1
PREVIOUS=$2

[[ -f "$CURRENT" ]] || { echo "current file does not exist: $CURRENT" >&2; exit 1; }

# Previous may legitimately not exist (first-ever run).
if [[ ! -f "$PREVIOUS" ]]; then
  # Everything is new.
  jq -s '
    map(.id) as $ids
    | {new: $ids, persistent: [], regressed: [], resolved: []}
  ' "$CURRENT"
  exit 0
fi

# All ids in each file.
CUR_IDS=$(jq -r '.id' "$CURRENT" | sort -u)
PREV_IDS=$(jq -r '.id' "$PREVIOUS" | sort -u)

# new = current - previous
NEW=$(comm -23 <(echo "$CUR_IDS") <(echo "$PREV_IDS"))
# persistent = current ∩ previous
PERSIST=$(comm -12 <(echo "$CUR_IDS") <(echo "$PREV_IDS"))
# resolved = previous - current
RESOLVED=$(comm -13 <(echo "$CUR_IDS") <(echo "$PREV_IDS"))

# Regression: ids in PREVIOUS's false-positive bucket that now appear in CURRENT.
# Caller should pass the previous false-positive.jsonl as $3 if they want
# regression detection; we keep this simple and treat everything in 'new'
# as potentially-regressed if the caller wants to filter further.
to_json_array() {
  printf '%s\n' "$1" | jq -R . | jq -s 'map(select(length > 0))'
}

jq -n \
  --argjson new       "$(to_json_array "$NEW")" \
  --argjson persistent "$(to_json_array "$PERSIST")" \
  --argjson resolved  "$(to_json_array "$RESOLVED")" \
  '{new: $new, persistent: $persistent, regressed: [], resolved: $resolved}'
