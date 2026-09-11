# MemVault Runbook

This document targets server-side deployments of MemVault. Day-to-day CLI usage
is covered in the [README](../README.md#cli-commands); install steps in
[INSTALL.md](INSTALL.md).

## Architecture and binaries

MemVault ships 3 binaries:

| Binary | Purpose |
|--------|---------|
| `memvault-cli` | command-line management tool (save / search / sync / backup, 25 subcommands incl. doctor / outcome / supersede / bench / import-skills / import-agent / checkpoints / restore / status / review) |
| `memvault-mcp` | MCP Server (stdio / SSE / REST transports) |
| `memvault-proxy` | MCP transparent proxy (upstream merge + memory injection + compliance tracking) |

<!-- AUTO-GENERATED: start modes / ports / health checks / REST endpoints derive
from rest_api.rs, sse_server.rs and the main.rs files; do not hand-edit. Regenerate
with the update-docs skill after code changes. -->
## Start modes and ports

### memvault-mcp

| Mode | Command | Port / endpoint |
|------|---------|-----------------|
| stdio (default) | `memvault-mcp --db ~/.memvault/data.db` | stdin/stdout |
| SSE (multi-client) | `memvault-mcp --transport sse --port 3777` | `http://127.0.0.1:3777/mcp` |
| REST API | `memvault-mcp --transport http --port 3777` (`rest` is a synonym) | `http://127.0.0.1:3777/` |
| REST + Web Dashboard | `memvault-mcp --transport http --port 3777 --serve-web ./dashboard/dist` | frontend + `/api/*` on the same port |

> Default port is **3777** (override with `--port`). DB default path
> `~/.memvault/data.db` (override with `--db`).
> `--serve-web <dir>` hosts the static frontend (from `npm run build` in
> `dashboard/`) at the REST port root (SPA routes fall back to `index.html`),
> same-origin — no CORS. HTTP binds are fixed to `127.0.0.1`; there is no
> `--bind` flag, so use a reverse proxy for remote access.

### memvault-proxy

| Mode | Command | Port / endpoint |
|------|---------|-----------------|
| stdio (default) | `memvault-proxy --config ~/.memvault/proxy.yaml` | none |
| SSE | `memvault-proxy --config ~/.memvault/proxy.yaml --transport sse --port 3778` | SSE / MCP |

The proxy's transport, port, DB and upstream MCP list are all configured in
`proxy.yaml` (example: [proxy.example.yaml](../proxy.example.yaml)).

---

## State files and data

| Path | Purpose | Notes |
|------|---------|-------|
| `~/.memvault/data.db` | SQLite database (memories / embedding cache / `memory_history` snapshot table) | **core data — must be persisted / backed up** |
| `~/.memvault/agents.yaml` | Agent Registry (injection rules / optional API keys) | optional; defaults apply when absent |
| `~/.memvault/proxy.yaml` | Proxy config (`memvault-proxy` only) | optional |

> API keys in the Agent Registry are SHA-256-hashed when the YAML is loaded;
> no plaintext is retained.

---

## Health checks and monitoring

### Health

```bash
# memvault-mcp (REST / SSE, default port 3777) — plain-text "ok"
curl -s http://127.0.0.1:3777/health
# ok (HTTP 200)

# memvault-proxy (SSE, port 3778) — JSON liveness probe; touches neither the DB nor MCP sessions
curl -s http://127.0.0.1:3778/health
# {"status":"ok","service":"memvault-proxy"} (HTTP 200)
```

> The proxy `/health` endpoint exists for startup/supervisor readiness probes;
> it reads no database and has no side effects, so it is safe to poll at high
> frequency.

### Metrics (Prometheus text format)

```bash
curl -s http://127.0.0.1:3777/metrics
```

`/metrics` is rendered by `metrics-exporter-prometheus` and can be scraped
directly for Grafana dashboards and alerting. MemVault itself ships **no**
alert push; recommended setup:

1. Prometheus scrapes `/metrics`
2. Use `up == 0` to detect service liveness
3. Add threshold alerts on business metrics (injection count / compliance rate
   / inbox backlog) as needed

---

## REST endpoint reference

`memvault-mcp --transport http` exposes the following endpoints (axum-based):

| Method & path | Purpose |
|---------------|---------|
| `GET /health` | liveness probe |
| `GET /metrics` | Prometheus metrics |
| `GET /api/memories` | list memories (`?namespace=`, `?limit=`, `?offset=` pagination) |
| `GET /api/stats` | aggregate stats (total / MUST-REF counts / per-layer / agents / namespaces / skills) |
| `POST /api/memories` | save a memory (supports `human_reviewed` / `ai_generated` overrides) |
| `DELETE /api/memories/{id}` | delete a memory |
| `PUT /api/memories/{id}` | update / edit a memory |
| `POST /api/search` | search (keyword / semantic / hybrid) |
| `POST /api/outcome` | report a task outcome (episodic memory); failures are reflected into lessons |
| `GET /api/episodes` | list episodic records filtered by task_type / status / namespace / limit (with lesson backlinks) |
| `POST /api/memories/{id}/supersede` | archive an outdated fact and point at its replacement (no deletion, reversible) |
| `GET /api/memories/{id}/relations` | relation triples attached to a memory |
| `GET /api/memories/{id}/evidence` | evidence chain for a memory (supports / contradicts / sourced from) |
| `GET /api/memories/{id}/checkpoints` | change history of a single memory |
| `POST /api/session` | inject context per agent identity, returns `inject_session_id` |
| `POST /api/extract` | extract structured memories from free text |
| `POST /api/dedup` | dedup scan |
| `POST /api/decay` | decay + archive |
| `POST /api/promote` | condensation pipeline (L1→L2→L3) |
| `POST /api/confirm-read` | mark as read (updates access_count) |
| `GET /api/inbox` | memories pending human review |
| `POST /api/inbox/{id}/approve` | approve |
| `POST /api/inbox/{id}/reject` | reject |
| `POST /api/inbox/{id}/edit` | edit |
| `GET /api/compliance/session` | compliance report for one injection session |
| `GET /api/compliance/summary` | aggregate compliance statistics |
| `GET /api/effectiveness` | effectiveness report (skill success rates / lesson recall) |
| `GET /api/agents` | Agent Registry listing (injection rules / API-key enforcement state) |
| `GET /api/agents/import/scan` | scan for importable agent configs |
| `POST /api/agents/import/preview` | preview import (parse only, no writes) |
| `POST /api/agents/import/run` | run agent config import |
| `GET /api/capabilities` | capability list (for client probing) |
| `GET /api/doctor` | self-diagnosis checks (health + key path probes) |
| `GET /api/export` | export memories (JSON / Markdown) |
| `POST /api/import` | import memories (JSON / Markdown) |
| `POST /api/backup` | consistent SQLite snapshot backup (equivalent to `memvault-cli backup`) |
| `GET /api/checkpoints` | global change history (paginated / filterable by memory_id) |
| `POST /api/checkpoints/{history_id}/restore` | restore a specific history snapshot |
| `GET /api/skills/import` / `POST /api/skills/import` | skill import |

> If an agent has an `api_key` in `agents.yaml`, its requests must carry the
> `X-MemVault-Api-Key` header (`401` on auth failure, `404` for missing
> resources); agents without a configured key require no authentication
> (backwards compatible).

---

<!-- AUTO-GENERATED -->

## Deployment

### Option A: Docker (recommended)

```bash
# 1. Build the image (or pull from a registry: ghcr.io/dreamor/memvault)
docker build -t memvault:local .

# 2. Create a persistent data volume
docker volume create memvault-data

# 3. Run in MCP stdio mode (for Claude Desktop / claude mcp add)
docker run --rm -i \
  -v memvault-data:/home/memvault/.memvault \
  memvault:local

# 4. Or run REST / SSE as a long-lived service
docker run -d --name memvault \
  -v memvault-data:/home/memvault/.memvault \
  -v $PWD/agents.yaml:/home/memvault/.memvault/agents.yaml:ro \
  -e MEMVAULT_DB=/home/memvault/.memvault/data.db \
  memvault:local \
  memvault-mcp --transport http --port 3777
```

- The data volume `/home/memvault/.memvault` **must be mounted**, or data is
  lost on container recreation.
- The image entrypoint is `tini`, which forwards SIGTERM to the MCP child.
- It runs as the non-root user `memvault` (uid 10001).

### Option B: binaries / source

```bash
cargo install --path crates/memvault-cli --locked
cargo install --path crates/memvault-mcp --locked
cargo install --path crates/memvault-proxy --locked

# Long-lived service (wrap with systemd / supervisord as needed)
memvault-mcp --transport http --port 3777
```

### Backup (mandatory before upgrades)

```bash
# Consistent snapshot (point-in-time SQLite backup)
memvault-cli backup --output ~/backups/memvault-$(date +%F).db

# Or entity export / import (JSON / Markdown)
memvault-cli export --format json --output ~/backups/
memvault-cli import --format json --input ~/backups/xxx.json
```

---

## Common problems

| Symptom | Diagnosis / fix |
|---------|-----------------|
| `failed to bind` / port occupied | confirm 3777 (MCP) / 3778 (proxy) are free; `lsof -i :3777` |
| `OPENAI_API_KEY invalid` | check the env var; with no remote provider configured MemVault degrades to the built-in native model or keyword search (functional, no semantic search) |
| Data lost | data volume `-v memvault-data:/home/memvault/.memvault` not mounted, or the volume was recreated |
| `permission denied` (data volume) | host volume owned by root; `docker run --user $(id -u):$(id -g) ...` or create as root then `chown` |
| MCP client can't connect | stdio needs `-i` not `-t`; confirm the client can execute `docker` / that binary paths are absolute |
| SQLite errors at startup | `rusqlite` is `bundled`, no system SQLite needed; on file-lock errors check for stale processes holding `data.db` |
| Injection not taking effect | confirm `~/.memvault/agents.yaml` exists and the profile `id` matches the connecting agent's; MUST memories are never filtered out |
| A memory was accidentally edited / deleted | locate with `memvault-cli checkpoints --memory-id <id>`, then restore the snapshot with `memvault-cli restore --history-id <n>` — no full-DB restore needed |

---

## Rollback procedures

Two paths, by recovery granularity:

### Single-memory rollback (lightweight, no full restore)

```bash
# History of one memory, or recent global changes
memvault-cli checkpoints --memory-id <memory_id>
memvault-cli checkpoints --limit 50

# Restore to a snapshot; recreates the row if the memory since got deleted
memvault-cli restore --history-id <history_id>
```

> Every update/delete snapshots the old row into `memory_history` inside a
> transaction, and the restore itself records a new snapshot — "undo of undo"
> remains traceable level by level.

### Whole-database rollback (snapshot / image)

1. **Stop**: halt the container / process.
2. **Restore the DB**: overwrite `~/.memvault/data.db` with a `memvault-cli backup`
   snapshot (or remount the old data volume in Docker).
3. **Roll back binary / image**: `docker run ghcr.io/dreamor/memvault:<previous-tag>`
   or `git checkout` the previous release tag and rebuild.
4. **Verify**: after startup `curl /health` returns `ok`, and `memvault-cli list`
   shows the original memories.

> Schema changes are normally backwards compatible (`serde(default)`); if a DB
> can't be read, read it with the older binary first, export, then import into
> the new DB.

---

## Alerting and upgrade advice

- Alerting: Prometheus scrapes `/metrics` → Alertmanager → mail / IM (MemVault
  ships no alerting of its own).
- Before upgrading: `memvault-cli backup` + `export` as double insurance.
- Security updates: follow [SECURITY.md](../SECURITY.md) and rotate leaked API
  keys promptly.
