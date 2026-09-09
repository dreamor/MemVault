---
description: Check the MemVault integration health (CLI, DB, backend) and report
---

Run the MemVault integration check and report the results as a short table:

1. CLI: `memvault status` (or `memvault-cli status` if `memvault` is not on
   PATH) — database reachable, memory count.
2. Pipeline: `memvault doctor` — flag stale or unreviewed items if reported.
3. Backend: if MEMVAULT_HTTP_URL (default http://127.0.0.1:3777) answers
   `/health`, report the backend version; otherwise say the HTTP backend is
   not running (fine for CLI/MCP usage).
4. Hooks: state whether this session received an automatic memory injection
   (a MEMORY CONTEXT block) or whether recall would rely on the
   session_start tool.
