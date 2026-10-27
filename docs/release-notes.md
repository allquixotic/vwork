vwork 0.3.2 makes Goose flexible while keeping repository work virtual by default.

- Generated setup enables Developer for unrestricted host reads, host-file edits,
  and one-off Bun/Python/shell scripts. Other MCPs, including Exa and AWS, remain
  available through your existing Goose configuration.
- MCP initialization, recipes and generated AGENTS.md share one concise policy.
  Use vwork for repository work, leased temporary copies for filesystem-only
  tools, and workspace_export for intentional durable copies. Exports remain
  indefinite.
- The optional hook redirects recognizable direct git clone/init/worktree-add
  calls. It allows reads, ordinary edits and general scripts; hook failures also
  allow calls. It is a best-effort detector, not a sandbox or filesystem monitor.

Upgrade: stop vwork, back up its state, replace the executable and restart. Schema
2 remains compatible. Regenerate setup, merge Developer/vwork into your Goose
config while preserving other MCPs and credentials, replace the old vwork hint
section and installed vwork-policy plugin, then start a new conversation. Leaving
the old blanket-denial hook or old shell bans installed will retain restrictions.
See docs/goose.md in the ZIP for exact migration steps. No Fastrock update needed.

The Windows x64 ZIP is built, signed, timestamped and verified by GitHub Actions.
