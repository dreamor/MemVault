List the pending drafts in the MemVault review inbox with the `list_inbox`
MCP tool (or `memvault review` on the CLI). For each draft, judge it against
its source: approve accurate and durable items with `review_memory`
(action=approve), fix wording with action=edit, reject wrong or sensitive
ones. Never approve content you cannot verify. End with a short tally
(approved / edited / rejected / left pending).
