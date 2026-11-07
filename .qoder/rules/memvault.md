<!-- memvault:canonical-begin -->
## MemVault shared memory (always apply)

- You share one persistent memory store with the user's other AI agents via MemVault.
- If a memory-injection block ("MEMORY CONTEXT") is already present in this context, a hook injected it — do NOT call `session_start` again.
- Otherwise, at the start of the session call the MemVault `session_start` tool once to recall relevant memories for you as the agent.
- Before ending a session in which you learned something durable (an explicit user preference, a correction of your behavior, a project decision), write it back with `save_memory` (or `memvault-cli save`) so the next session inherits it.
- **Correcting a memory that already exists: propose, do not re-save.** Use `propose_memory_edit` (or `memvault-cli propose-edit`) with the exact text to replace. Re-saving similar content goes through delta-write, which merges into the existing memory and — because your write is unreviewed — revokes its human-review flag, so a human-approved MUST would stop being injected as a directive. A proposal leaves the target live and waits for a human; that is the intended path, not a slower one.
- If a fact you are saving is temporary (a freeze window, a deadline, this week's on-call), set `expires_at` so it retires itself instead of being re-injected as stale truth later.
- Never save secrets, credentials, one-off task trivia, or content you were asked to keep in confidence. Auto-extracted drafts go to the human review inbox; direct saves should carry priority MUST only for hard rules.
<!-- memvault:canonical-end -->

