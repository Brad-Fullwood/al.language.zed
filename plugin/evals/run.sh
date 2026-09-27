#!/usr/bin/env bash
#
# Ground-truth runner for plugin/evals/cases/*.json.
#
# Each case names an al-explorer invocation against the bundled fixture and
# either a jq expression with the exact value it must equal, or a list of
# substrings its raw output must contain. This checks what the tool answers,
# not whether an agent reaches for the right skill: no LLM runs here. Scoring
# an agent's transcript against these questions is the plugin/TESTING.md
# protocol, run by hand against a live session.
#
# Usage:
#   plugin/evals/run.sh                # run every case under cases/
#   plugin/evals/run.sh cases/01-*.json # run one or more case files
#
# al-explorer is resolved in this order: $AL_EXPLORER_BIN, PATH, then
# target/release/al-explorer under the repository this script lives in.
# al-lsp is resolved the way a resolved al-explorer resolves it to start the
# daemon (crates/al-protocol/src/client/mod.rs): $AL_LSP_BIN, then beside
# al-explorer, then PATH. A case whose fixture is missing fails; if no
# al-explorer is found, or no al-lsp is found once al-explorer is, every case
# is reported as skipped rather than failed, since that is a missing
# prerequisite, not a wrong answer.

set -uo pipefail

root="$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)"
evals_dir="$root/plugin/evals"
cases_dir="$evals_dir/cases"
fixtures_dir="$evals_dir/fixtures"

if ! command -v jq >/dev/null 2>&1; then
	printf 'run.sh: jq is required to read the case files\n' >&2
	exit 2
fi

resolve_al_explorer() {
	if [ -n "${AL_EXPLORER_BIN-}" ] && [ -x "$AL_EXPLORER_BIN" ]; then
		printf '%s\n' "$AL_EXPLORER_BIN"
		return 0
	fi
	if command -v al-explorer >/dev/null 2>&1; then
		command -v al-explorer
		return 0
	fi
	if [ -x "$root/target/release/al-explorer" ]; then
		printf '%s\n' "$root/target/release/al-explorer"
		return 0
	fi
	return 1
}

al_explorer=""
if found="$(resolve_al_explorer)"; then
	al_explorer="$found"
fi

# Every case needs the daemon, and a resolved al-explorer starts it from
# al-lsp beside its own binary before it falls back to PATH. Resolving that
# same pair here, once, catches a missing or unrelated al-lsp before any case
# runs: without this, each of the twelve cases answered al-explorer's
# "Cannot find al-lsp" error on stdout, which read as twelve wrong answers
# instead of one missing prerequisite.
resolve_al_lsp() {
	if [ -n "${AL_LSP_BIN-}" ] && [ -x "$AL_LSP_BIN" ]; then
		printf '%s\n' "$AL_LSP_BIN"
		return 0
	fi
	if [ -n "$al_explorer" ]; then
		local sibling_dir
		sibling_dir="$(CDPATH='' cd -- "$(dirname -- "$al_explorer")" && pwd)"
		if [ -x "$sibling_dir/al-lsp" ]; then
			printf '%s\n' "$sibling_dir/al-lsp"
			return 0
		fi
	fi
	if command -v al-lsp >/dev/null 2>&1; then
		command -v al-lsp
		return 0
	fi
	return 1
}

al_lsp=""
lsp_skip_reason=""
if [ -n "$al_explorer" ]; then
	if found="$(resolve_al_lsp)"; then
		al_lsp="$found"
	else
		lsp_skip_reason="no al-lsp binary beside $al_explorer or on PATH"
	fi
fi

run_with_timeout() {
	if command -v timeout >/dev/null 2>&1; then
		timeout 60 "$@"
	else
		"$@"
	fi
}

case_files=("$@")
if [ "$#" -eq 0 ]; then
	case_files=()
	while IFS= read -r f; do
		case_files+=("$f")
	done < <(find "$cases_dir" -maxdepth 1 -name '*.json' | LC_ALL=C sort)
fi

pass=0
fail=0
skip=0

for case_file in "${case_files[@]}"; do
	if [ ! -f "$case_file" ]; then
		printf 'FAIL  %s -- case file not found\n' "$case_file"
		fail=$((fail + 1))
		continue
	fi

	id="$(jq -r '.id' "$case_file")"
	question="$(jq -r '.question' "$case_file")"

	if [ -z "$al_explorer" ]; then
		printf 'SKIP  %s -- no al-explorer binary on PATH or at target/release/al-explorer\n' "$id"
		skip=$((skip + 1))
		continue
	fi

	if [ -z "$al_lsp" ]; then
		printf 'SKIP  %s -- %s\n' "$id" "$lsp_skip_reason"
		skip=$((skip + 1))
		continue
	fi

	fixture_rel="$(jq -r '.fixture' "$case_file")"
	overlay="$(jq -r '.overlay // empty' "$case_file")"
	fixture_path="$root/$fixture_rel"

	if [ ! -f "$fixture_path/app.json" ]; then
		printf 'FAIL  %s -- fixture not found: %s\n' "$id" "$fixture_rel"
		fail=$((fail + 1))
		continue
	fi

	workdir="$(mktemp -d "${TMPDIR:-/tmp}/al-eval.XXXXXX")"
	cp -r "$fixture_path/." "$workdir/"
	rm -rf "${workdir:?}/.vscode/.alcache"
	if [ -n "$overlay" ]; then
		cp -r "${fixtures_dir:?}/$overlay/." "$workdir/"
	fi

	case_ok=1
	reasons=()

	check_count="$(jq '.checks | length' "$case_file")"
	i=0
	while [ "$i" -lt "$check_count" ]; do
		args=()
		while IFS= read -r arg; do
			args+=("$arg")
		done < <(jq -r ".checks[$i].args[]" "$case_file")

		stderr_file="$(mktemp "${TMPDIR:-/tmp}/al-eval-stderr.XXXXXX")"
		stdout="$(cd "$workdir" && run_with_timeout "$al_explorer" "${args[@]}" 2>"$stderr_file")"
		check_failed=0

		jq_expr="$(jq -r ".checks[$i].jq // empty" "$case_file")"
		if [ -n "$jq_expr" ]; then
			expect_exact="$(jq -r ".checks[$i].expect_exact" "$case_file")"
			actual="$(printf '%s' "$stdout" | jq -r "$jq_expr" 2>/dev/null)"
			if [ "$actual" != "$expect_exact" ]; then
				case_ok=0
				check_failed=1
				reasons+=("check $i: jq '$jq_expr' gave '$actual', wanted '$expect_exact'")
			fi
		fi

		contains_count="$(jq ".checks[$i].expect_contains // [] | length" "$case_file")"
		j=0
		while [ "$j" -lt "$contains_count" ]; do
			needle="$(jq -r ".checks[$i].expect_contains[$j]" "$case_file")"
			if ! printf '%s' "$stdout" | grep -qF -- "$needle"; then
				case_ok=0
				check_failed=1
				reasons+=("check $i: output of '${args[*]}' does not contain '$needle'")
			fi
			j=$((j + 1))
		done

		# A wrong answer and a refused answer look the same on stdout (both
		# fail their check), but only one is this tool getting the question
		# wrong. Dropping stderr here is what made a missing or mismatched
		# al-lsp read as twelve wrong answers instead of one clear refusal.
		if [ "$check_failed" -eq 1 ] && [ -s "$stderr_file" ]; then
			reasons+=("check $i: stderr: $(cat "$stderr_file")")
		fi
		rm -f "$stderr_file"

		i=$((i + 1))
	done

	(cd "$workdir" && run_with_timeout "$al_explorer" daemon-shutdown >/dev/null 2>&1)
	rm -rf "$workdir"

	if [ "$case_ok" -eq 1 ]; then
		printf 'PASS  %s -- %s\n' "$id" "$question"
		pass=$((pass + 1))
	else
		printf 'FAIL  %s -- %s\n' "$id" "$question"
		for reason in "${reasons[@]}"; do
			printf '        %s\n' "$reason"
		done
		fail=$((fail + 1))
	fi
done

printf '\n%d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skip"
[ "$fail" -eq 0 ]
