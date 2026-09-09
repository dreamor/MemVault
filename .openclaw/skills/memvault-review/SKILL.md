---
name: memvault-review
description: Work the MemVault review inbox — inspect auto-extracted memory drafts (with their source snippets) and approve, edit, or reject each one. Use when the user asks to review memories, or /memvault-review is invoked.
---

# MemVault review

Unreviewed memory is untrusted memory. Drafts arrive from auto-extraction and
imports; the human (aided by you) decides what becomes context.

1. List pending drafts with the MemVault `list_inbox` tool or
   `memvault review` on the CLI.
2. For each draft, judge it against its source snippet:
   - Accurate and durable → `review_memory` with action `approve`.
   - Right idea, wrong wording → action `edit` with corrected content.
   - Wrong, transient, or sensitive → action `reject`.
3. Never approve content you cannot verify from the snippet; rejected items
   are deleted, not archived — when in doubt, leave it pending for the user.
4. Report the outcome as a short tally (approved / edited / rejected / left).
