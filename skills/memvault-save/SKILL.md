---
name: memvault-save
description: Write a durable learning (explicit user preference, correction, project decision) into the shared MemVault store so every future session and agent inherits it. Use whenever the user states a lasting preference or corrects course, or asks to remember something.
---

# MemVault save

Make this session's learnings available to every future session.

1. Save with the MemVault `save_memory` tool, or `memvault save --content "..."
   --priority ... --type ...` on the CLI.
2. Choose deliberately:
   - `priority`: MUST only for hard rules that must never be violated;
     REFERENCE (default) for everything else.
   - `type`: `preference`, `fact`, `episode`, `entity`, or `skill`.
   - `namespace`: keep project-specific memory in a project namespace instead
     of `global`.
   - `instruction`: for MUST items, phrase the actionable instruction.
3. Saving is delta-aware: near-duplicates are skipped or merged; do not force
   re-saves (`force_insert`) unless specifically asked.
4. Never save secrets, credentials, session transcripts, or anything the user
   asked to keep out of memory. When unsure, propose the memory text first.
