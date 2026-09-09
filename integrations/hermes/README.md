# MemVault for Hermes Agent

Python plugin: pre-LLM-call recall injection + end-of-session extraction
(REST-only, stdlib only — no `memvault` CLI required on the machine).

| Capability | Status |
|---|---|
| MCP | use `memvault-mcp --transport http` backend and any MCP client config |
| Session-start recall | ✅ `pre_llm_call`, once per session |
| End-of-session extraction | ⚠️ `extract_session()` ships, host hook name unverified — call manually or wire when confirmed |

## Install

```bash
hermes plugins install <path to integrations/hermes>
```

(Package layout: `plugin.yaml` + `__init__.py`. Field names follow the
Hermes plugin manifest convention — if the installer complains, rename
against the current Hermes plugin docs and keep both files.)

## Environment

`MEMVAULT_HTTP_URL` (default `http://127.0.0.1:3777`) ·
`MEMVAULT_AGENT_ID` (default `hermes`) ·
`MEMVAULT_HOOK_EXTRACT=1` to enable extraction ·
`MEMVAULT_TIMEOUT` seconds (default `5`).

## Verify on install

1. `pre_llm_call` hook name and its return-value semantics (injection path
   is isolated in `_as_context` — one function to adjust).
2. Session-end hook name; wire it to `MemVaultPlugin.extract_session`.
