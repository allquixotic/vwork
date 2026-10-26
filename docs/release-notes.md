vwork 0.3.0 stores all persistent Git objects, refs and source contents in redb.
Git objects deduplicate across repository registrations by object ID. git2/libgit2
handles normal reads, history, trees and commits in process. Models can clone a
selected source or register an existing repository through structured MCP tools;
no persistent source checkout or borrowed external object store remains.

Filesystem-only tools use tracked temporary copies. Tasks/packages return a lease,
expiry, exact revision, result and source changes. Explicit release removes the
copy and its environments/outputs; expiry and startup cleanup provide a fallback.
Unimported source edits are saved into separate recovery workspaces, within limits.
Defaults: four copies, 30-minute TTL, renewals up to 24 hours, 128 MiB source recovery.
Active vwork jobs prevent deletion; failed cleanup stays visible and is retried.
Legacy owned build directories are adopted and reclaimed with source recovery.

Configured package managers now include uv and CDK. uv supports sync/lock/check/
test/build; CDK supports local synth/list with lookups disabled. Tools operate on
leased files and return completion before the model decides when to release them.
Captured Windows subprocesses suppress console allocation. Read-only storage_info
provides sharing evidence without disposable probe workspaces.

Upgrade: stop the daemon and back up its state. Keep original registered Git stores
available for the first startup migration to schema 2. Original user checkouts are
preserved. Older binaries cannot reopen schema 2; rollback requires the backup.
Replace vwork.exe, reconnect Goose, and regenerate/merge agent instructions and
hook matchers while preserving local customizations. Developer may remain disabled;
no Fastrock/Goose rebuild is needed. An old unfiltered Goose shell hook can still
flash independently; regenerate its matcher to skip vwork calls.

The ZIP is built/signed/verified by CI with checksums and GitHub provenance.
Remote clone uses journaled temporary libgit2 transfer packs, cleaned after import
and restart. Native tools still require ordinary files; vwork does not mount redb.
Global package caches, explicitly configured external output paths and deliberate
user exports are not lease-owned. Cleanup runs while the daemon runs or at restart;
locked files and recovery limits produce actionable errors instead of data loss.
