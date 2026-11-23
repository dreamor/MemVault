# Migration Notes

This page covers behaviour and command-line changes that can break an existing
deployment or automation. It is intentionally narrower than the
[CHANGELOG](../CHANGELOG.md): the changelog records what changed, while this
page tells an operator what to change in their setup.

## 0.7.x authentication and credential scope

Keyless deployments are unchanged: with no `api_key` registered in
`agents.yaml`, MemVault keeps its local unauthenticated mode.

Once **any** agent has a registered key:

- **Writes fail closed** under the default `all` and `writes` scopes. Save,
  extract/auto-save, outcome, proposal, delete/forget, review,
  dedup/decay/promote, evidence and compliance writes require a registered
  agent id and its key. An unregistered or invented `agent_id` is rejected; it
  no longer falls through to unauthenticated mode.
- **Reads follow `MEMVAULT_AUTH_SCOPE`.**
  - `all` (default): search, session start, evidence/inbox/report reads all
    require a registered keyed identity. Omitting `agent_id` is an error, not
    an anonymous read.
  - `writes`: read-only tools are allowed without a key, but naming a keyed
    agent still requires proving that agent's key. This applies to MCP read
    tools and REST `/api/search` / `/api/session`; REST admin/operational
    endpoints remain admin-keyed.
  - `producer`: the registered key is a `fact_write=resolve` capability, not an
    access gate. Ordinary MCP/REST reads *and* writes may omit it; a key that
    is supplied is still validated, and only a proven key can enable fact
    management or stamp `identity_verified`. `fact_write=resolve` without proof
    returns `review_required`; admin/operational REST endpoints remain
    admin-keyed. Use this mode for harnesses that register a producer key but
    cannot attach it to every ordinary tool call.
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

- Session bootstrap is **`POST /api/session`**. For compatibility with older
  hook fallbacks, `GET /api/session?agent_id=<id>&output=plain` is also served;
  it requires an agent id in the query or `X-MemVault-Agent-Id` header and
  returns a structured 400 JSON error if one is missing. New integrations
  should still use POST.
- Disclosure is explicit on every channel. MCP `session_start` defaults to
  `disclosure=layered` (full selected instructions plus `[MORE - 摘要]`
  overflow bullets and a remaining-count hint). CLI `session-start` defaults
  to `--disclosure flat` (full instruction text plus the conflict warning), and
  will emit the MCP layered shape with `--disclosure layered`. REST accepts
  `disclosure` in the POST body or query, reports `data.disclosure`, and
  `?output=plain` returns whichever mode was selected. MCP and CLI can each opt
  into the other mode; `flat` is byte-identical across all three.

## 0.7.x approved-edit reversal

Approving an edit proposal is still a supersede: the approved replacement has
a new id and the original target is archived. The transaction now also records
the pre-approval target snapshot and approved replacement revision. Undo it
with one of:

```bash
memvault-cli revert-approval --id <approved-replacement-id>
```

```http
POST /api/memories/<approved-replacement-id>/revert-approval
```

MCP exposes the same operation as `revert_approval` (admin auth). It restores
the target and soft-deletes the replacement atomically. It refuses if the
replacement was edited after approval or the target changed, because either
would discard newer knowledge; a repeated call returns `already_reverted`.
There is no need to propose a reverse edit just to undo an approval.

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

## Known product decisions and limitations

- Approving an edit proposal still supersedes the target and creates a new row
  (the id changes). The SQLite backend now records the pre-approval target
  snapshot and replacement revision so `revert-approval` can undo that exact
  approval atomically; withdraw remains the proposer-side operation for edits
  that are still pending. The Markdown backend reports
  `revert-approval requires the SQLite backend` because its private approval
  journal deliberately retains no reversible content payload.
- MCP overflow summaries are intentionally lossy. Use `search_memory` to fetch
  details, or select `disclosure=flat` / `--disclosure flat` when the caller
  needs the shared full-text payload.

## Release-check locale

`scripts/e2e-release-check.sh` forces `grep -a` so UTF-8 CLI output is never
treated as a binary file under `LANG=C`. A minimal CI image now needs no extra
locale package for the gate.
