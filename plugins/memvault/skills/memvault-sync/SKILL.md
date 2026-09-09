---
name: memvault-sync
description: Regenerate agent-native rule files (CLAUDE.md, AGENTS.md, .github/copilot-instructions.md) from the MemVault store for hosts without hook injection. Use when the user asks to sync/refresh memory into rule files, or /memvault-sync is invoked.
---

# MemVault sync

Some hosts have no hooks and no MCP; for those, MemVault projects selected
memories into the rule files those agents already read.

1. Run `memvault sync` (add `--out <dir>` to target a project directory).
   Use `--watch` to keep the files fresh during long sessions.
2. Show the user which files changed and what was included (MUST items lead,
   REFERENCE items follow, with links back to the store).
3. Remind the user to commit the generated files so agents without MemVault
   installed still receive the same memory.
4. Never hand-edit the generated blocks: they carry deterministic markers and
   the next `sync` overwrites manual edits. Change the memories, not the file.
