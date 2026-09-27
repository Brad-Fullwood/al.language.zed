#!/usr/bin/env bash
#
# Validate the Claude Code plugin under plugin/ and the marketplace manifest at
# .claude-plugin/marketplace.json. Checks that the manifests parse, that the
# scripts are clean under shellcheck, and that every skill and agent carries a
# name and a description.
#
# Run from the repository root, or through `make plugin-validate`.

set -uo pipefail

root="$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)"
plugin="$root/plugin"
fail=0

problem() {
	printf 'plugin-validate: %s\n' "$1" >&2
	fail=1
}

# ── Manifests ────────────────────────────────────────────────────
for manifest in \
	"$plugin/.claude-plugin/plugin.json" \
	"$plugin/.mcp.json" \
	"$plugin/hooks/hooks.json" \
	"$root/.claude-plugin/marketplace.json"; do
	if [ ! -f "$manifest" ]; then
		problem "missing $manifest"
		continue
	fi
	if ! jq -e . "$manifest" >/dev/null 2>&1; then
		problem "$manifest is not valid JSON"
	fi
done

jq -e '.name and .description' "$plugin/.claude-plugin/plugin.json" >/dev/null 2>&1 ||
	problem "plugin.json needs a name and a description"

jq -e '.name and .owner.name and (.plugins | length > 0)' \
	"$root/.claude-plugin/marketplace.json" >/dev/null 2>&1 ||
	problem "marketplace.json needs a name, an owner.name and at least one plugin"

# The marketplace entry and the plugin manifest have to agree, or
# `claude plugin tag` and the install both go wrong.
mp_name="$(jq -r '.plugins[0].name // empty' "$root/.claude-plugin/marketplace.json" 2>/dev/null)"
pl_name="$(jq -r '.name // empty' "$plugin/.claude-plugin/plugin.json" 2>/dev/null)"
[ "$mp_name" = "$pl_name" ] ||
	problem "marketplace entry '$mp_name' does not match plugin name '$pl_name'"

mp_version="$(jq -r '.plugins[0].version // empty' "$root/.claude-plugin/marketplace.json" 2>/dev/null)"
pl_version="$(jq -r '.version // empty' "$plugin/.claude-plugin/plugin.json" 2>/dev/null)"
[ "$mp_version" = "$pl_version" ] ||
	problem "marketplace version '$mp_version' does not match plugin version '$pl_version'"

# ── Frontmatter ──────────────────────────────────────────────────
# Every skill and agent needs a name and a description: the description is the
# only thing deciding whether the model reaches for it.
check_frontmatter() {
	local file="$1" label="$2" key
	if [ "$(head -n1 "$file")" != "---" ]; then
		problem "$label has no YAML frontmatter"
		return
	fi
	for key in name description; do
		if ! awk 'NR>1 && /^---[[:space:]]*$/ {exit} NR>1' "$file" |
			grep -qE "^${key}:[[:space:]]*[^[:space:]]"; then
			problem "$label has no '$key' in its frontmatter"
		fi
	done
}

skill_count=0
for skill in "$plugin"/skills/*/SKILL.md; do
	[ -f "$skill" ] || continue
	skill_count=$((skill_count + 1))
	check_frontmatter "$skill" "skills/$(basename "$(dirname "$skill")")/SKILL.md"
done
[ "$skill_count" -gt 0 ] || problem "no skills found under plugin/skills/"

agent_count=0
for agent in "$plugin"/agents/*.md; do
	[ -f "$agent" ] || continue
	agent_count=$((agent_count + 1))
	check_frontmatter "$agent" "agents/$(basename "$agent")"
done
[ "$agent_count" -gt 0 ] || problem "no agents found under plugin/agents/"

# ── Scripts ──────────────────────────────────────────────────────
# plugin/evals/run.sh and plugin/tests/*.sh are dev-facing harnesses rather
# than something the plugin runtime invokes, but they ship in the same
# repository and CI holds them to the same bar, so they get the same
# executable, syntax and shellcheck checks as everything under
# plugin/scripts/.
for script in "$plugin"/scripts/*.sh "$plugin"/evals/*.sh "$plugin"/tests/*.sh; do
	[ -f "$script" ] || continue
	[ -x "$script" ] || problem "$(basename "$script") is not executable"
	bash -n "$script" || problem "$(basename "$script") has a syntax error"
done

if command -v shellcheck >/dev/null 2>&1; then
	shellcheck "$plugin"/scripts/*.sh "$plugin"/evals/*.sh "$plugin"/tests/*.sh ||
		problem "shellcheck reported findings"
else
	printf 'plugin-validate: shellcheck not installed, skipping (install it to run this check)\n' >&2
fi

# ── Regression tests ─────────────────────────────────────────────
# al-fetch-release.sh's member and install-step checks, and run.sh's al-lsp
# resolution. Needs python3 to serve the test archives over a local HTTP
# server.
plugin_test="$plugin/tests/al-fetch-release-test.sh"
if [ ! -x "$plugin_test" ]; then
	problem "missing or non-executable $plugin_test"
elif command -v python3 >/dev/null 2>&1; then
	"$plugin_test" || problem "$plugin_test failed"
else
	printf 'plugin-validate: python3 not installed, skipping %s\n' "$plugin_test" >&2
fi

# al-bin.sh walks upward looking for a target/ directory. A relative
# CLAUDE_PLUGIN_ROOT used to walk down to "." and stay there, because
# `dirname .` is `.`. .mcp.json runs this script, so the MCP server never
# started and never said why. Run it from a directory with no target/ above it,
# which is what makes the walk reach the top.
if command -v timeout >/dev/null 2>&1; then
	walk_dir="$(mktemp -d)"
	(
		cd "$walk_dir" &&
			env -u AL_BIN_DIR -u CLAUDE_PLUGIN_DATA CLAUDE_PLUGIN_ROOT=. \
				timeout 20 bash "$plugin/scripts/al-bin.sh" al-explorer >/dev/null 2>&1
		[ "$?" -ne 124 ]
	) || problem "al-bin.sh does not terminate with a relative CLAUDE_PLUGIN_ROOT"
	rmdir "$walk_dir" 2>/dev/null || true
else
	printf 'plugin-validate: timeout not installed, skipping the al-bin.sh walk check\n' >&2
fi

# ── Claude Code's own validator ──────────────────────────────────
if command -v claude >/dev/null 2>&1; then
	claude plugin validate "$plugin" || problem "claude plugin validate failed"
	claude plugin validate "$root" || problem "claude plugin validate failed on the marketplace"
else
	printf 'plugin-validate: claude not on PATH, skipping its validator\n' >&2
fi

if [ "$fail" -eq 0 ]; then
	printf 'plugin-validate: OK (%d skills, %d agents)\n' "$skill_count" "$agent_count"
fi
exit "$fail"
