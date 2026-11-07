---
name: memvault-save
description: Write a durable learning (explicit user preference, correction, project decision) into the shared MemVault store so every future session and agent inherits it. Use whenever the user states a lasting preference or corrects course, or asks to remember something.
---

# MemVault save

Make this session's learnings available to every future session.

1. Save with the MemVault `save_memory` tool, or `memvault-cli save --content "..."
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
4. **If the fact is temporary, say when it ends.** Pass `expires_at` (RFC3339),
   or `--ttl 30m|12h|7d` on the CLI, for anything time-bounded — a freeze window,
   a deadline, who is on call this week, a flag left on for an incident. Past
   that instant the memory stops being retrieved and injected on its own instead
   of decaying slowly and being re-served as stale truth. Expiry retires a fact;
   it does not erase it, and it stays inspectable and restorable. A temporary
   fact saved without a bound becomes a permanent one, which is the failure this
   exists to prevent. If the fact *happened* earlier than now (backfilling a
   transcript, recording last week's decision), also set `occurred_at` — that is
   a different axis from `expires_at`: "when it happened" versus "until when it
   is true".
5. **Correcting a memory you did not author: propose, do not overwrite.** Use
   `propose_memory_edit` (or `memvault-cli propose-edit`) with the exact
   `old_text` to replace. It creates a pending proposal and leaves the target
   live and injecting until a human approves — so a wrong correction cannot
   silently take effect, and neither can deleting and re-saving, which would
   drop the review history and the MUST's provenance. `old_text` must match
   exactly once; an ambiguous anchor is refused rather than guessed. Editing
   `field: "instruction"` changes the directive the agent is actually given,
   while `field: "content"` changes only the body — if the two say the same
   thing, propose both or the behaviour will not change.
6. Never save secrets, credentials, session transcripts, or anything the user
   asked to keep out of memory. When unsure, propose the memory text first.
