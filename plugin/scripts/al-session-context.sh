#!/usr/bin/env bash
#
# SessionStart hook. In an AL project, tell the session that symbol, event,
# caller and impact questions have a tool, and that grep is the wrong answer.
# Anywhere else, emit nothing.
#
# Reads the hook payload on stdin and writes the SessionStart JSON contract to
# stdout. An empty stdout adds no context.

set -uo pipefail

payload="$(cat)"

cwd=""
if command -v jq >/dev/null 2>&1; then
	cwd="$(printf '%s' "$payload" | jq -r '.cwd // empty' 2>/dev/null || true)"
fi
[ -n "$cwd" ] || cwd="${CLAUDE_PROJECT_DIR:-$PWD}"

# An AL project is a directory with an app.json that declares an id and a
# publisher. Anything else, including a Node project with an app.json, is not.
app_json="$cwd/app.json"
[ -f "$app_json" ] || exit 0
if command -v jq >/dev/null 2>&1; then
	jq -e 'has("id") and has("publisher")' "$app_json" >/dev/null 2>&1 || exit 0
else
	grep -q '"publisher"' "$app_json" 2>/dev/null || exit 0
fi

context=$(
	cat <<'EOF'
This is a Business Central AL project. The al-bc plugin indexes every object in
the workspace and in the .app packages under .alpackages, including Base
Application.

Answer these from the plugin, not from find, grep, ripgrep or reading .al files:

- Where an object, table, field, codeunit, enum or procedure is defined, what
  fields a table has, which app defines object N -> skill al-bc:bc-symbol-lookup
- The source or body of a procedure, including base-app code -> skill al-bc:bc-base-app-source
- Who subscribes to or publishes an event, which event to subscribe to -> skill al-bc:bc-event-map
- Who calls or uses a symbol, what a change breaks -> skill al-bc:bc-impact-check
- The next free object ID or field number -> skill al-bc:bc-object-id-allocator
- Running AL tests or test coverage -> skill al-bc:bc-test-locally
- What a dependency upgrade breaks -> skill al-bc:bc-upgrade-impact
- Pre-build and pre-deploy audit, lint and cop warnings -> skill al-bc:bc-workspace-health

Run the plugin's commands from this project directory. Do not cd first: the
al-lsp daemon binds to the directory the command runs in, and a cd into the
toolchain checkout starts a daemon on the wrong project.

For a lookup that needs several calls, delegate to the al-bc:bc-symbol-scout
agent rather than the Explore agent. Grep finds text; these tools read the
symbol index and the call and event graphs, so they also see .app package code
that no grep can reach.
EOF
)

if command -v jq >/dev/null 2>&1; then
	jq -n --arg c "$context" \
		'{hookSpecificOutput: {hookEventName: "SessionStart", additionalContext: $c}}'
else
	printf '%s\n' "$context"
fi
