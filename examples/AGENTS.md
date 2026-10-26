# Virtual workspace contract

Source lives in vwork, not the original checkout or the current shell directory.

- Reuse `vwork_repo_list` registrations. For requested setup, use `vwork_repo_clone` or `vwork_repo_register`; objects, refs and committed contents are imported into redb. No persistent source checkout is needed.
- Use `vwork_workspace_info` to get the current revision before editing.
- Read with `vwork_files_read`; discover paths with `vwork_files_list` / `vwork_files_glob`.
- Search with `vwork_files_search`. Use regex, glob filters, context and pagination.
- Edit with `vwork_files_replace`, `vwork_files_write`, or `vwork_files_apply_patch`.
- Every edit supplies `expected_revision`. On conflict, reread and reconcile.
- The patch tool accepts full-file replacement/delete objects, not unified diffs.
- Rename/copy/delete using the corresponding virtual tools. File modes are preserved.
- Use virtual status/diff/history/restore/merge instead of shell Git.
- Fork before independent work. Never mutate another agent's workspace without assignment.
- For storage/sharing questions, read `vwork_storage_info`. Do not create/drop probe workspaces merely to investigate storage. Base manifests share by repository ID and commit; edited blobs deduplicate across the daemon. Git objects deduplicate across registrations by object ID, independent of remote URL. Logical workspace size is not physical disk allocation.
- Native tools see a different filesystem. Do not use shell, grep, rg, find, cat, native file editors, or native Git to operate on virtual source.
- Builds/tests use `vwork_workspace_run_task` and a human-configured task name. Report tested revision, success, exit status and any truncated output accurately.
- npm/Cargo/pip/uv/CDK use `vwork_package_list` and `vwork_package_run` with a configured project/action. Do not run package managers in a shell or install packages into virtual paths.
- Build tasks and package actions reuse one tracked temporary copy per repo while its lease is live. Native Git there reports the base commit as HEAD; virtual edits are staged and make `git describe --dirty` dirty. It is not the original checkout.
- Inspect returned `source_changes` and package `input_changes`. Import needed manifest/lockfile/source edits with explicit paths and the returned revision before another build switches or repairs the checkout. Never import node_modules, target directories, virtual environments or caches.
- If package inputs change, run its install action before npm scripts or pip/uv actions. Missing package projects require human configuration; do not invent executable overrides.
- Prefer virtual operations. Request `vwork_workspace_temporary` only when a specific tool requires ordinary files; explain the need in `purpose`.
- Every build/package action returns `temporary.id`, path and expiry. After using the copy, import wanted edits, save wanted artifacts, and call `vwork_temporary_release` immediately. Do not keep heavy copies just in case.
- If a tool still needs a copy, renew before expiry. Check `vwork_temporary_list` for forgotten copies, failed cleanup and recovery workspace names. Active vwork jobs prevent deletion.
- Expiry and restart cleanup save bounded, nonignored source edits into a separate recovery workspace in redb, then remove the physical copy, dependencies and outputs. Review recovery workspaces before deleting them. Do not use `discard_changes` unless intentionally discarding edits.
- Cleanup failure stays visible for retry and counts against the copy limit. Report unresolved cleanup instead of silently leaving a heavy tree behind.
- Use `vwork_workspace_export` only for durable files the user explicitly wants to retain; these exports do not expire.
- Import physical edits only using explicit paths and the matching exported revision.
- Export a Git commit using `vwork_workspace_commit`. This stores the commit and private ref inside redb; it does not push or move the original branch.
- Conflicted merges make no changes. Resolve explicitly with revision-checked edits.
- Never fabricate successful operations. Missing tools/task definitions require human configuration.

This file governs consumers of virtual workspaces. The repository-root AGENTS.md governs development of vwork itself.
