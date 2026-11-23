# Migration Notes

This page covers behaviour and command-line changes that can break an existing
deployment or automation. It is intentionally narrower than the
[CHANGELOG](../CHANGELOG.md): the changelog records what changed, while this
page tells an operator what to change in their setup.

## 0.7.x authentication and credential scope

Keyless deployments are unchanged: with no `api_key` registered in
`agents.yaml`, MemVault keeps its local unauthenticated mode.

Once **any** agent has a registered key:

- **Writes fail closed.** Save, extract/auto-save, outcome, proposal,
  delete/forget, review, dedup/decay/promote, evidence and compliance writes
  require a registered agent id and its key. An unregistered or invented
  `agent_id` is rejected; it no longer falls through to unauthenticated mode.
- **Reads follow `MEMVAULT_AUTH_SCOPE`.**
  - `all` (default): search, session start, evidence/inbox/report reads all
    require a registered keyed identity. Omitting `agent_id` is an error, not
    an anonymous read.
  - `writes`: read-only tools are allowed without a key, but naming a keyed
    agent still requires proving that agent's key. This applies to MCP read
    tools and REST `/api/search` / `/api/session`; REST admin/operational
    endpoints remain admin-keyed.
- **Server-side MCP credential:** `MEMVAULT_AGENT_<NORMALIZED_ID>_API_KEY` and
  the generic `MEMVAULT_AGENT_API_KEY` (only for the agent named by
  `MEMVAULT_AGENT_ID`) now apply to every MCP tool, not only save/extract. The
  stdio process binding still applies: a network request cannot make the
  server lend it another agent's env key.
- **Hook REST fallback:** when the agent is keyed, export `MEMVAULT_API_KEY`
  for the bundled SessionStart/Qoder hooks. The CLI path needs no key; the REST
  fallback sends the key as `X-MemVault-Api-Key`.

If you only want the old read-only behaviour while keeping write protection,
set:

```bash
export MEMVAULT_AUTH_SCOPE=writes
```

If a host cannot attach either a per-call key or the server-side env credential
and you do not want keyless reads, the deployment is not usable for that host;
register a keyed identity and configure its credential rather than inventing an
`agent_id`.

## 0.7.x session endpoint and injection shapes

- Session bootstrap is **`POST /api/session`**. `GET /api/session` is not a
  route and returns `405` with an empty body.
- MCP `session_start` uses the layered format: full MUST instructions plus
  `[MORE - 摘要]` overflow bullets and a remaining-count hint. The CLI
  `session-start` command and the SessionStart hook use flat text. REST returns
  a JSON envelope, with `?output=plain` returning the flat text. The selected
  memories and safety gates are shared; the rendered shapes are not.

## 0.7.x priority and layer updates

`priority` and `layer` are separate persisted fields. Updating only
`priority` now re-derives the default layer:

| Priority | Default layer |
|----------|---------------|
| `MUST` | `L3` |
| `REFERENCE` | `L2` |
| `BACKGROUND` | `L1` |

Use an explicit layer when you need an exception:

```bash
memvault-cli update <id> --priority MUST --layer L2
memvault-cli update <id> --layer L2   # layer only; priority unchanged
```

REST `PUT /api/memories/<id>` follows the same rule: an explicit `layer` wins;
otherwise a priority change replaces the stale derived layer.

## 0.6.x CLI command and flag drift

The 0.6.0–0.6.6 transition renamed several commands and flags. The 0.7.x names
below are stable; update old scripts rather than relying on aliases (there are
none).

| Older/transitional 0.6.x script shape | 0.7.x invocation |
|---------------------------------------|------------------|
| `memvault-cli session-extract ...` | `memvault-cli extract ...` (use `--hook-input <payload>` for a host Stop hook, or `--transcript <file>`) |
| `memvault-cli save --memory-type fact ...` | `memvault-cli save --type fact ...` |
| `memvault-cli supersede <old> <new>` (positional form) | `memvault-cli supersede --old <id> --new <id>` |
| `memvault-cli restore <history-id>` (positional form) | `memvault-cli restore --history-id <id>` |

The left column lists forms that appeared while the 0.6.x command surface was
settling; if your script uses another variant, compare it with `--help` before
upgrading.

Verify a migrated script against the current help output before deploying:

```bash
memvault-cli extract --help
memvault-cli save --help
memvault-cli supersede --help
memvault-cli restore --help
memvault-cli update --help
```

## Known product decisions without an automatic undo

- Approving an edit proposal supersedes the target and creates a new row
  (the id changes). There is currently no atomic "revert this approval" verb;
  withdraw works only while a proposal is still pending. To undo an approved
  content change, propose the inverse edit and approve it, or restore an older
  checkpoint with `memvault-cli checkpoints` / `restore`.
- MCP overflow summaries are intentionally lossy. Use `search_memory` to fetch
  details rather than expecting the CLI's flat full text.

## Release-check locale

`scripts/e2e-release-check.sh` forces `grep -a` so UTF-8 CLI output is never
treated as a binary file under `LANG=C`. A minimal CI image now needs no extra
locale package for the gate.
