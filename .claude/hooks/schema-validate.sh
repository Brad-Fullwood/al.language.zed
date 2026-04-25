#!/usr/bin/env bash
# schema-validate.sh — validate an agentic artefact against its schema
#
# Usage: schema-validate.sh <kind> <path>
#   kind ∈ { manifest, finding, handoff, arch-handoff, arch-design,
#            handoff-progress, release-audit, cycle-log }
#
# Exits 0 on pass, 1 on fail (with message on stderr).
#
# Dependencies: jq (for JSON), grep/awk (for Markdown frontmatter).
#
# This script is intentionally shell + jq rather than a proper JSON Schema
# validator: it keeps dependencies zero and is fast enough for hook use.

set -uo pipefail

fail() {
  echo "schema-validate[$KIND]: $*" >&2
  exit 1
}

[[ $# -ge 2 ]] || { echo "usage: $0 <kind> <path>" >&2; exit 2; }

KIND=$1
PATH_ARG=$2

[[ -f "$PATH_ARG" ]] || fail "file does not exist: $PATH_ARG"

command -v jq >/dev/null || fail "jq is required but not on PATH"

validate_schema_version() {
  local path=$1 expected=$2
  local actual
  actual=$(jq -r '."$schema_version" // empty' "$path" 2>/dev/null)
  [[ -n "$actual" ]] || fail "missing \$schema_version in $path"
  [[ "$actual" == "$expected" ]] || fail "schema_version mismatch: expected $expected, got $actual (in $path)"
}

validate_jsonl() {
  local path=$1
  # Every line must be a valid JSON object on its own.
  local lineno=0
  while IFS= read -r line || [[ -n "$line" ]]; do
    lineno=$((lineno + 1))
    [[ -z "$line" ]] && continue
    echo "$line" | jq -e 'type == "object"' >/dev/null 2>&1 \
      || fail "line $lineno not a valid JSON object in $path"
  done < "$path"
}

validate_frontmatter_field() {
  local path=$1 field=$2 expected=$3
  local actual
  actual=$(awk -v f="$field" '
    /^---$/ { if (in_fm) exit; in_fm=1; next }
    in_fm && $1 == f ":" { $1=""; sub(/^ /, ""); print; exit }
  ' "$path")
  [[ -n "$actual" ]] || fail "missing frontmatter field '$field' in $path"
  [[ "$actual" == "$expected" ]] || fail "frontmatter '$field': expected '$expected', got '$actual' (in $path)"
}

case "$KIND" in
  manifest)
    jq -e 'type == "object"' "$PATH_ARG" >/dev/null || fail "not a JSON object"
    validate_schema_version "$PATH_ARG" "1"
    for required in run_id started_at started_by mode branch head_sha phases; do
      jq -e --arg k "$required" 'has($k)' "$PATH_ARG" >/dev/null \
        || fail "missing required field: $required"
    done
    ;;

  finding)
    # Each line must be an object with required fields.
    validate_jsonl "$PATH_ARG"
    while IFS= read -r line || [[ -n "$line" ]]; do
      [[ -z "$line" ]] && continue
      echo "$line" | jq -e '
        .["$schema_version"] == "1"
        and (.id | type == "string")
        and (.reviewer | type == "string")
        and (.kind | IN("bug","risk","refactor","gap","doc"))
        and (.severity | IN("critical","high","medium","low","nit","speculative"))
        and (.file | type == "string")
        and (.line | type == "number")
        and (.what | type == "string")
        and (.why | type == "string")
      ' >/dev/null 2>&1 || fail "finding missing required fields: $line"

      # Refactor-kind rule
      echo "$line" | jq -e '
        if .kind == "refactor"
        then (.what_we_know_now | type == "string" and length > 0)
          and (.scope_estimate  | type == "string" and length > 0)
        else true
        end
      ' >/dev/null 2>&1 || fail "refactor finding missing what_we_know_now or scope_estimate: $line"
    done < "$PATH_ARG"
    ;;

  handoff)
    jq -e 'type == "object"' "$PATH_ARG" >/dev/null || fail "not a JSON object"
    validate_schema_version "$PATH_ARG" "1"
    jq -e '.tasks | type == "array"' "$PATH_ARG" >/dev/null || fail "tasks is not an array"
    jq -e '.summary | type == "object"' "$PATH_ARG" >/dev/null || fail "summary is not an object"
    jq -e '
      .tasks
      | all(
          (.task_id | type == "string")
          and (.finding_ref == .task_id)
          and (.priority | IN("P0","P1","P2","P3"))
          and (.owner_crate | type == "array")
          and (.acceptance_criteria | type == "array")
          and (.reproduction | type == "object")
          and (.needs_design | type == "boolean")
        )
    ' "$PATH_ARG" >/dev/null || fail "task schema violation"
    ;;

  arch-handoff)
    jq -e 'type == "object"' "$PATH_ARG" >/dev/null || fail "not a JSON object"
    validate_schema_version "$PATH_ARG" "1"
    jq -e '
      .tasks
      | all(.design_status | IN("approved","blocked","not-needed"))
    ' "$PATH_ARG" >/dev/null || fail "task missing design_status"
    ;;

  arch-design)
    for required_heading in "^## Problem$" "^## Current state$" "^## Options$" "^### Option A" "^### Option B" "^## Recommended option$" "^## Execution plan$" "^## Roll-back plan$"; do
      grep -q -E "$required_heading" "$PATH_ARG" \
        || fail "missing required heading: $required_heading"
    done
    grep -q "^schema_version:" "$PATH_ARG" || fail "missing frontmatter schema_version"
    grep -q "^task_id:" "$PATH_ARG" || fail "missing frontmatter task_id"
    grep -q "^status:" "$PATH_ARG" || fail "missing frontmatter status"
    ;;

  handoff-progress)
    jq -e 'type == "object"' "$PATH_ARG" >/dev/null || fail "not a JSON object"
    validate_schema_version "$PATH_ARG" "1"
    jq -e '(.entries | type) == "array"' "$PATH_ARG" >/dev/null \
      || fail "entries is not an array"
    jq -e '
      .entries
      | all(
          (.task_id | type == "string")
          and (.status | IN("done","blocked","skipped","deferred","bounced-to-review"))
          and (if .status == "done"
               then (.commit_sha | type == "string") and (.commit_sha | length > 0)
               else true end)
        )
    ' "$PATH_ARG" >/dev/null || fail "entry schema violation"
    ;;

  release-audit)
    grep -q "^schema_version: 1$" "$PATH_ARG" || fail "missing/invalid schema_version frontmatter"
    grep -q "^overall: " "$PATH_ARG" || fail "missing overall frontmatter"
    for chk in compile clippy fmt tests scope_check dep_check commit_map_reachable; do
      grep -q "^  $chk: " "$PATH_ARG" || fail "missing check: $chk"
    done
    ;;

  cycle-log)
    validate_jsonl "$PATH_ARG"
    while IFS= read -r line || [[ -n "$line" ]]; do
      [[ -z "$line" ]] && continue
      echo "$line" | jq -e '
        .["$schema_version"] == "1"
        and (.cycle | type == "number")
        and (.outcome | IN("completed","halted-converged","halted-capped","halted-circuit-breaker","halted-drift","halted-red-audit","aborted-error"))
      ' >/dev/null 2>&1 || fail "cycle-log entry schema violation"
    done < "$PATH_ARG"
    ;;

  *)
    fail "unknown kind: $KIND"
    ;;
esac

echo "schema-validate[$KIND]: OK $PATH_ARG"
exit 0
