Diagnose a runtime issue with the AL language server.

Usage: /diagnose <description of the problem>

Steps:
1. Check if al-lsp is running: `pgrep -a al-lsp`
2. Check recent logs: `tail -100 ~/.local/share/al-lsp/logs/al-lsp.log`
3. Check for daemon socket: `ls -la ${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/al-lsp/`
4. Based on the symptoms described in $ARGUMENTS, investigate:
   - **No completions**: Check if workspace initialized (look for "workspace ready" in logs)
   - **No symbols**: Check if .app packages downloaded (look in `~/.cache/al-lsp/packages/`)
   - **Crash on open**: Check for panic in logs, look for malformed .al files
   - **Slow performance**: Check RUST_LOG=al_core=debug for slow query times
   - **Daemon won't start**: Check socket permissions and whether another instance is running

Report findings with specific log excerpts and suggested fixes.
