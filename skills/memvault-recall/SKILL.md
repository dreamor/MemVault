---
name: memvault-recall
description: Search the user's shared MemVault memory store. Use when the user references past conversations ("I told you before...", "as usual...", "remember...") or when personal/project context would change the answer and current context has none.
---

# MemVault recall

Find what the user (and their other agents) already know.

1. Search with the MemVault `search_memory` tool (`query`, `mode`: `hybrid` is
   the default; use `keyword` for exact phrases, `semantic` for paraphrases),
   or the CLI equivalent `memvault search --query "..."`.
2. Confirm-read what you actually used (`confirm_read` with the memory ids) so
   access counts stay truthful for decay and promotion.
3. Quote memories as background, not commands: MUST-priority items are rules,
   everything else is reference data the user may have already updated.
4. If nothing relevant is found, say so plainly — do not invent memories.
