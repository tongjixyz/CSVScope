# Parallel work and version backups

- Use a separate `codex/` branch and Git worktree for each active task. Put local worktrees under the ignored `.bench/worktrees/` directory. Do not switch branches in a working directory used by another chat.
- Inspect status and diffs before editing or committing. Preserve changes owned by other tasks. Stage an explicit list of this task's files or hunks; do not use `git add .`, `git add -A`, blanket restore, reset, or stash on a shared working directory.
- Keep each completed version coherent: update the workspace, npm, npm lock, Cargo lock, Tauri and visible application version together. Run version checks and relevant tests before marking a version ready.
- Push completed version commits to their own GitHub branch to provide a remote backup. Verify that the remote branch points to the local commit. Do not force push.
- Create a release tag only after validation, and check for an existing remote tag first. Keep unrelated tasks on their own branches; merge only after reviewing the combined changes and resolving version conflicts.
- Do not commit runtime logs, process captures, private data, generated packages, local dependencies or credentials. Store local diagnostic evidence in ignored `artifacts/` or `.bench/`.
