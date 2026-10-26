# Virtual workspace contract

Source lives in vwork, not the original checkout or the current shell directory.

- Use `vwork_workspace_info` to get the current revision before editing.
- Read with `vwork_files_read`; discover paths with `vwork_files_list` / `vwork_files_glob`.
- Search with `vwork_files_search`. Use regex, glob filters, context and pagination.
- Edit with `vwork_files_replace`, `vwork_files_write`, or `vwork_files_apply_patch`.
- Every edit supplies `expected_revision`. On conflict, reread and reconcile.
- The patch tool accepts full-file replacement/delete objects, not unified diffs.
- Rename/copy/delete using the corresponding virtual tools. File modes are preserved.
- Use virtual status/diff/history/restore/merge instead of shell Git.
- Fork before independent work. Never mutate another agent's workspace without assignment.
- Native tools see a different filesystem. Do not use shell, grep, rg, find, cat, native file editors, or native Git to operate on virtual source.
- Builds/tests use `vwork_workspace_run_task` and a human-configured task name. Report tested revision, success, exit status and any truncated output accurately.
- Use `vwork_workspace_export` only when physical files are requested or required. Exports are separate from the original checkout.
- Import physical edits only using explicit paths and the matching exported revision.
- Export a Git commit using `vwork_workspace_commit`. This creates a private ref; it does not push or move the original branch.
- Conflicted merges make no changes. Resolve explicitly with revision-checked edits.
- Never fabricate successful operations. Missing tools/task definitions require human configuration.

This file governs consumers of virtual workspaces. The repository-root AGENTS.md governs development of vwork itself.
