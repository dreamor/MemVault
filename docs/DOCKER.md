# Deploying MemVault with Docker

The image serves both the CLI and the MCP Server in local/server scenarios.

## Quick start

### Pull a prebuilt image

Release builds are published to both registries (dual-push, verified):

```bash
docker pull ghcr.io/dreamor/memvault:latest
# Mirror:
docker pull docker.io/dreamor/memvault:latest
```

### Build locally

```bash
docker build -t memvault:local .
```

### Run the CLI

```bash
# Persistent data volume
docker volume create memvault-data

docker run --rm \
  -v memvault-data:/home/memvault/.memvault \
  memvault:local \
  memvault-cli --help

# Save a memory
docker run --rm \
  -v memvault-data:/home/memvault/.memvault \
  memvault:local \
  memvault-cli save --content "User prefers Python" --priority MUST --type preference
```

### Run as an MCP Server (stdio)

> The stdio MCP transport requires PID 1 inside the container to be the service
> process and to forward signals correctly. The image entrypoint wraps it with
> `tini`.

```bash
# Interactive stdio (for Claude Desktop / claude mcp add)
docker run --rm -i \
  -v memvault-data:/home/memvault/.memvault \
  memvault:local
```

Claude Desktop configuration:

```json
{
  "mcpServers": {
    "memvault": {
      "command": "docker",
      "args": [
        "run", "--rm", "-i",
        "-v", "memvault-data:/home/memvault/.memvault",
        "memvault:local",
        "memvault-mcp",
        "--db", "/home/memvault/.memvault/data.db"
      ]
    }
  }
}
```

### Run as REST API + Web Dashboard (http)

The Web Dashboard is a purely static frontend, hosted by `memvault-mcp
--transport http` on the same port (same origin, no CORS). `memvault-mcp`
embeds the dashboard in the binary (rust-embed), so every image — like every
install channel — serves the UI with no frontend build, volume mount, or
flags:

```bash
docker run --rm -p 3777:3777 \
  -v memvault-data:/home/memvault/.memvault \
  memvault:local \
  memvault-mcp --db /home/memvault/.memvault/data.db --transport http
```

Open `http://127.0.0.1:3777` in a browser.

To serve a different build of the frontend, mount it and point `--serve-web`
at it (the flag wins over the embedded default and the env override):

```bash
# 1. Build the frontend dist/ (on the host)
cd dashboard && npm ci && npm run build

# 2. Mount dist/ and override via --serve-web
docker run --rm -p 3777:3777 \
  -v memvault-data:/home/memvault/.memvault \
  -v "$PWD/dashboard/dist:/srv/override:ro" \
  memvault:local \
  memvault-mcp --db /home/memvault/.memvault/data.db \
    --transport http --port 3777 --serve-web /srv/override
```

> **Caveat**: SSE / REST in `memvault-mcp` always listen on `127.0.0.1` — there
> is **no** `--bind` / `--host` option. Under the default bridge network,
> `-p 3777:3777` therefore usually cannot reach the service inside the container
> (the container's loopback interface does not accept traffic on eth0). To serve
> REST externally, use `--network host` or expose it through a reverse proxy.

> The image's default command is `memvault-mcp` (stdio), which ignores
> `MEMVAULT_SERVE_WEB` silently — the dashboard only activates on
> `--transport http/rest`.

## Configuration

### Mount points

| Path | Purpose | Recommendation |
|------|---------|----------------|
| `/home/memvault/.memvault` | SQLite data / embedding model cache | **must mount** |
| `/home/memvault/.memvault/agents.yaml` | Agent Registry (must live next to the DB — the program looks for `agents.yaml` in the `--db` directory) | recommended |

### Environment variables

| Variable | Purpose |
|----------|---------|
| `MEMVAULT_EMBEDDING_API_KEY` (fallback `OPENAI_API_KEY`) | optional — switches embedding/search to an OpenAI-compatible remote endpoint; the built-in `native` fastembed model is the default and needs no key |
| `MEMVAULT_EMBEDDING_API_BASE` | custom embedding endpoint (any OpenAI-compatible server) |
| `MEMVAULT_EMBEDDING_MODEL` | embedding model name |
| `HF_ENDPOINT` | HuggingFace mirror for model downloads (e.g. `https://hf-mirror.com` where huggingface.co is unreachable) |
| `MEMVAULT_DB` | absolute DB path, default `/home/memvault/.memvault/data.db` |
| `MEMVAULT_SERVE_WEB` | optional dashboard dist dir served on `--transport http`, overriding the dist embedded in the binary; overridden by `--serve-web`, ignored on stdio/sse |
| `RUST_LOG` | log level, e.g. `info,memvault_core=debug` |

### Full example

```bash
docker run -d --name memvault \
  -v memvault-data:/home/memvault/.memvault \
  -v $PWD/agents.yaml:/home/memvault/.memvault/agents.yaml:ro \
  -e RUST_LOG=info \
  -e MEMVAULT_DB=/home/memvault/.memvault/data.db \
  memvault:local
```

## Image layering strategy

The `Dockerfile` uses the following BuildKit features to maximize cache hits:

- **Dependencies before source**: copies `Cargo.toml` + `Cargo.lock` +
  `crates/` first, then runs the release build, so source edits don't
  re-download dependencies
- **`--mount=type=cache`**: `/usr/local/cargo/registry` and `/build/target`
  are preserved across builds instead of being re-fetched every time
- **`--locked`**: `cargo build --locked` pins `Cargo.lock`, preventing
  CI/local drift
- **Web stage**: the dashboard `dist/` is built inside a `node:24-slim` stage
  (lockfile-first for cache), swapped into `crates/memvault-mcp/assets/web`,
  and rust-embed-baked into the binary at compile time — the frontend is never
  taken from the host
- **`debian-slim` + non-root user**: minimal runtime (no embedding model baked
  in — it downloads on first use to `~/.memvault/models`), running as the
  `memvault` user (uid 10001) per container security best practice
- **`tini` entrypoint**: forwards SIGTERM to the MCP stdio child process so the
  service doesn't hang when an MCP client disconnects
- **MCP Registry OCI label**: the label
  `io.modelcontextprotocol.server.name="io.github.dreamor/memvault"` proves to
  the MCP Registry that this image belongs to the listed server; its value must
  match the `name` in `integrations/mcp-registry/server.json`

## Troubleshooting

| Symptom | Check |
|---------|-------|
| Saved data lost | forgot `-v memvault-data:/home/memvault/.memvault`? |
| `permission denied` on the data volume | the host volume may be owned by root: `docker run --user $(id -u):$(id -g) ...`, or create as root then `chown` |
| MCP won't connect | use `-i`, not `-t`; make sure the client can invoke `docker` |
| Embedding fails | pass keys via `docker run -e` (never bake them into the image build); or drop them entirely — the built-in `native` model needs no key |
| SQLite errors at startup | SQLite is statically linked (`bundled`), no system library needed |

## CI / Release integration

- `.github/workflows/ci.yml` has a `docker-smoke` job that builds the image and
  smoke-tests it locally but **never pushes**.
- `.github/workflows/release.yml` (on `v*` tags) builds and pushes to
  `ghcr.io/<repo>:<tag>` and `:<latest>`, and mirrors to
  `docker.io/dreamor/memvault` (steps gated on the `DOCKERHUB_*` secrets).
- `.github/workflows/rebuild-docker.yml` is a manual (workflow_dispatch)
  rebuild for build-metadata changes — e.g. adding/updating OCI labels — that
  must reach the registries without bumping any package version. It pushes to
  both registries as well.

Local build (same Dockerfile as release):

```bash
DOCKER_BUILDKIT=1 docker build --tag memvault:local .
```
