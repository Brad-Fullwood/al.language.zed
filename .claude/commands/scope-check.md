Verify that the current changes are within scope of the task.

1. Run `git diff --stat` to list all changed files
2. For each changed file, verify it's relevant to the current task
3. Flag any files that appear to be out-of-scope changes:
   - Files in unrelated crates
   - CI/CD configuration changes
   - Dependency changes in Cargo.toml
   - Tree-sitter grammar modifications
   - README or documentation changes (unless the task is documentation)
   - Formatting-only changes to files not otherwise modified

Report:
- IN SCOPE: files that are clearly related to the task
- OUT OF SCOPE: files that appear unrelated (these should be reverted)
- UNCLEAR: files that might be related but need human judgment

If out-of-scope changes are found, suggest `git checkout -- <file>` commands to revert them.
