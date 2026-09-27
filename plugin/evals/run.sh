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
# al-explorer resolves its own al-lsp the same way to start the daemon
# (crates/al-protocol/src/client/mod.rs, find_al_lsp_binary): beside
# al-explorer, then PATH, refusing a PATH al-lsp of a different version. A
# case whose fixture is missing fails; if no al-explorer is found, or a
# preflight call through al-explorer reports an error, every case is reported
# as skipped rather than failed, since that is a missing prerequisite, not a
# wrong answer.

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

run_with_timeout() {
	if command -v timeout >/dev/null 2>&1; then
		timeout 60 "$@"
	else
		"$@"
	fi
}

# Reads the .error field of a JSON object on stdout, or prints nothing when
# there is none.
stdout_error() {
	printf '%s' "$1" | jq -r 'if type == "object" and (.error != null) then .error else empty end' 2>/dev/null
}

# Every case needs the daemon, and al-explorer starts it from its own al-lsp
# resolution (crates/al-protocol/src/client/mod.rs, find_al_lsp_binary): beside
# its own binary, then PATH, refusing a PATH al-lsp of a different version.
# Asking al-explorer itself, once, before any case runs, catches a missing or
# mismatched al-lsp the same way al-explorer reports it to a case: as
# {"error": "..."} on stdout. Without this, each of the twelve cases answered
# that error as a failed check, which read as twelve wrong answers instead of
# one missing prerequisite.
preflight_error=""
if [ -n "$al_explorer" ]; then
	preflight_fixture="$root/crates/al-test-harness/data/test_al_project"
	if [ -f "$preflight_fixture/app.json" ]; then
		preflight_dir="$(mktemp -d "${TMPDIR:-/tmp}/al-eval-preflight.XXXXXX")"
		cp -r "$preflight_fixture/." "$preflight_dir/"
		rm -rf "${preflight_dir:?}/.vscode/.alcache"
		preflight_stdout="$(cd "$preflight_dir" && run_with_timeout "$al_explorer" --compact doctor 2>/dev/null)"
		preflight_error="$(stdout_error "$preflight_stdout")"
		(cd "$preflight_dir" && run_with_timeout "$al_explorer" daemon-shutdown >/dev/null 2>&1)
		rm -rf "$preflight_dir"
	fi
fi

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

	if [ -n "$preflight_error" ]; then
		printf 'SKIP  %s -- %s\n' "$id" "$preflight_error"
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
		# wrong. al-explorer reports a refusal as {"error": "..."} on stdout,
		# so check that first, then stderr: dropping both is what made a
		# missing or mismatched al-lsp read as twelve wrong answers instead of
		# one clear refusal.
		if [ "$check_failed" -eq 1 ]; then
			check_stdout_error="$(stdout_error "$stdout")"
			if [ -n "$check_stdout_error" ]; then
				reasons+=("check $i: error on stdout: $check_stdout_error")
			fi
			if [ -s "$stderr_file" ]; then
				reasons+=("check $i: stderr: $(cat "$stderr_file")")
			fi
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
