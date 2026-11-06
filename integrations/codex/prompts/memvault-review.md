List the pending drafts in the MemVault review inbox with the `list_inbox`
MCP tool (or `memvault-cli review` on the CLI). For each draft, judge it against
its source: approve accurate and durable items with `review_memory`
(action=approve), fix wording with action=edit, reject wrong or sensitive
ones. Never approve content you cannot verify. An item carrying
`proposes_supersede_of` is not a new draft but an edit another agent proposed to
an existing memory, and it looks exactly like a near-duplicate; `list_inbox`
returns the target's current text beside it, so diff the two and judge the change
rather than the text in isolation — approving supersedes the target, and
rejecting it as "a duplicate" silently discards a correction. End with a short
tally (approved / edited / rejected / left pending).
