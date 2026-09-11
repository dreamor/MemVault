# Troubleshooting

This guide follows a **symptom → cause → fix** pattern, noting error codes,
versions and severity. If the steps below don't resolve the issue, attach the
output of `memvault-cli status` and `memvault-cli list --limit 10` to a GitHub
issue.

Related documents:

- Install & build: [INSTALL.md](INSTALL.md)
- Product & architecture: [DESIGN.md](DESIGN.md)

---

## 0. Quick index

| Symptom | Jump to |
|---------|---------|
| `failed to bind` / `Address already in use` | §1.1 port conflict |
| `OPENAI_API_KEY invalid` / embedding failure | §2 embedding config |
| MCP server unreachable, but the CLI works | §3 stdio protocol |
| `permission denied` on `data.db` | §4 file permissions |
| Schema errors after upgrading | §5 upgrades / migration |
| Dashboard connection failure / empty lists | §6 Web Dashboard |
| `agent_memory too large` console warning | §7 token budget |
| `memvault sync` produced AGENTS.md but agents ignore it | §8 zero-intrusion sync |
| `status` reports missing embedding / link errors | §9 build/link dependencies |

---

## 1. Startup and binding

### 1.1 `failed to bind` (port in use)

**Symptom**

```
memvault-mcp[ERROR] failed to bind 127.0.0.1:3777
thread 'main' panicked at ... Os { code: 98, kind: AddrInUse }
```

**Cause**: the default port 3777 is occupied, or the previous process is in
`TIME_WAIT`.

**Locate**

```bash
# macOS / Linux
lsof -iTCP:3777 -sTCP:LISTEN
sudo lsof -nP -i:3777

# Windows
netstat -ano | findstr :3777
```

**Fix**

```bash
# A. Change the port (SSE / REST only support --port; there is no --bind flag)
memvault-mcp --transport sse --port 9876 --db ~/.memvault/data.db

# B. Kill the stale process
kill $(lsof -t -i:3777)        # macOS / Linux
taskkill /PID <pid> /F          # Windows (admin)

# C. Wait ~60s for TIME_WAIT to expire, then restart

# D. Reverse proxy (SSE / REST always listen on 127.0.0.1 and cannot rebind)
# For container/remote access, expose 127.0.0.1:3777 through a reverse proxy
# instead of trying to change the bind address
```

### 1.2 Windows: `link.exe not found`

**Symptom**: first `cargo build` fails with

```
error: linker `link.exe` not found
note: the msvc targets require a linker
```

**Fix**

1. Install [Visual Studio Build Tools 2022](https://visualstudio.microsoft.com/visual-studio-build-tools/) with **C++ build tools** + **Windows 10/11 SDK**
2. Open the "x64 Native Tools Command Prompt for VS 2022" and run `cargo build` inside it
3. Or switch to the GNU toolchain (no Visual Studio required):

```bash
rustup default stable-x86_64-pc-windows-gnu
pacman -S mingw-w64-x86_64-gcc          # MSYS2
cargo build --release
```

### 1.3 Linux: `libssl.so.1.1 not found`

**Symptom**: at runtime, `libssl.so.1.1: cannot open shared object file`.

**Fix**: depends on the distro's OpenSSL/Glibc generation. The Linux binaries
link the system OpenSSL — install a version matching what the binary expects:

```bash
# Debian 11 / Ubuntu 20.04 and newer
sudo apt install libssl3

# Old distros that still ship 1.1
sudo apt install libssl1.1
```

Prefer running a distro whose libssl matches the binary's build — for old
targets, install the matching `libssl1.1`/`libssl3` package from your distro's
archive, or rebuild MemVault from source on the target machine.

### 1.4 macOS: `dyld: Library not loaded: @rpath/libssl.3.dylib`

**Symptom**: dyld error when running `memvault-mcp`.

**Fix**

```bash
brew install openssl@3
# Let the binary find it
export DYLD_FALLBACK_LIBRARY_PATH="$(brew --prefix openssl@3)/lib:$DYLD_FALLBACK_LIBRARY_PATH"
```

To persist, add the export line to `~/.zshrc`. The Apple Silicon path is
`/opt/homebrew/opt/openssl@3/lib`; Intel is `/usr/local/opt/openssl@3/lib`.

### 1.5 Docker: `permission denied writing data.db`

See §4 — ownership of `/home/memvault/.memvault`.

---

## 2. Embedding / OpenAI integration

### 2.1 `OPENAI_API_KEY invalid`

**Symptom**

```
[embedding] request failed: 401 Unauthorized
error: OPENAI_API_KEY invalid
```

**Checklist**

1. Is `echo $OPENAI_API_KEY` empty? Does it contain stray whitespace/newlines?
2. Has the key expired? Been revoked?
3. Is it a direct OpenAI key, or Azure/self-hosted? For the latter, see §2.3
4. Is the balance exhausted (OpenAI console `Usage`)?

**Fix**

```bash
# Verify quickly (current shell only)
export OPENAI_API_KEY="sk-proj-..."
memvault-cli search --query "Python" --top-k 5

# Persist: write to ~/.memvault/.env (not your shell rc — .env is read only by MemVault and keeps the global env clean)
echo 'MEMVAULT_EMBEDDING_API_KEY="sk-proj-..."' >> ~/.memvault/.env
```

If you use a local proxy (Zed/Cline etc. don't pass env through):

```jsonc
// ~/Library/Application Support/Claude/claude_desktop_config.json
{
  "mcpServers": {
    "memvault": {
      "command": "/path/to/memvault-mcp",
      "args": ["--db", "~/.memvault/data.db"],
      "env": { "OPENAI_API_KEY": "sk-proj-..." }
    }
  }
}
```

> **Did `OPENAI_API_KEY` end up in your process env without you configuring it?**
> When `MEMVAULT_EMBEDDING_PROVIDER` is unset, MemVault treats a present
> `OPENAI_API_KEY` as a guess and validates it with a test embedding call. If
> the call fails (401/unreachable), it automatically falls back to the built-in
> `native` model at startup instead of retrying a known-bad key on every
> search. To skip the validation and pin a provider explicitly, set
> `MEMVAULT_EMBEDDING_PROVIDER` (e.g. `native`) — explicit config is never
> overridden by validation.

### 2.2 `insufficient_quota`

**Symptom**: HTTP 429 + `You exceeded your current quota`.

**Fix**: OpenAI balance exhausted.

- Top up at <https://platform.openai.com/account/billing>
- Or switch to keyword-only mode: `MEMVAULT_EMBEDDING_PROVIDER=none memvault-cli search --query "<keywords>"`

### 2.3 Using any OpenAI-compatible provider (self-hosted / Azure / vLLM / gateways)

Embedding defaults to the built-in native model (zero external services). To
point at any remote API, use the OpenAI-compatible protocol
`POST {base}/embeddings`. Write these keys into `~/.memvault/.env` to persist:

```bash
MEMVAULT_EMBEDDING_PROVIDER=openai-compatible   # any identifier also works
MEMVAULT_EMBEDDING_API_BASE=https://your-host/v1 # OpenAI / Azure / vLLM / gateway-compatible endpoint
MEMVAULT_EMBEDDING_API_KEY=<key>                 # omit for unauthenticated services
MEMVAULT_EMBEDDING_MODEL=text-embedding-3-small  # or the provider's model name
MEMVAULT_EMBEDDING_DIM=1536                      # must match the provider's real output dimension
```

> For Azure, encode the deployment in the base (e.g.
> `https://<res>.openai.azure.com/openai/deployments/<dep>`). Provider decision
> chain: an explicit `MEMVAULT_EMBEDDING_PROVIDER` wins; if unset but an API key
> is configured (including the `OPENAI_API_KEY` fallback), MemVault guesses
> `openai` (a wrong guess is validated then downgraded to native, see §2.1);
> with neither set, the built-in `native` provider is used. `ollama` / `auto`
> point at a local Ollama instance (`auto` falls back to native when Ollama
> isn't running).

### 2.4 Embedding dimension mismatch with existing vectors

**Symptom**

```
[embedding] dimension mismatch: db=1536 model=1024
```

This happens when you first used `text-embedding-3-small` (1536-d) and then
switched to `bge-m3` (1024-d) without recomputing old rows. **Two ways out**:

```bash
# A. Switch back to the original model (simple; persist in ~/.memvault/.env)
MEMVAULT_EMBEDDING_MODEL=text-embedding-3-small
MEMVAULT_EMBEDDING_DIM=1536
```

B. Or accept the new model and let the background embedding backfill
   regenerate vectors: MemVault re-embeds rows that don't match the active
   model asynchronously (`spawn_embedding_backfill`), so semantic search
   quality ramps up again automatically. There is no separate "rebuild
   vectors" CLI subcommand to run.

---

### 2.5 Native model download failure

**Symptom**: at startup `WARN native embedding init failed — ... Failed to
retrieve onnx/model.onnx`, and semantic search degrades to keyword mode.
`memvault-cli status` distinguishes three states: `none configured` (nothing
set), `explicitly disabled` (`MEMVAULT_EMBEDDING_PROVIDER=off`), and `native
configured but unavailable` (configured, init failed). An init failure is
**not** an unrecoverable fallback — `save`/`status` retry building the
provider every run, and the model downloads on the first success.

**Cause**: `provider=native` downloads the model from HuggingFace on first use
(default `bge-small-zh-v1.5`, ~95MB); download fails when `huggingface.co` is
unreachable.

**Fix**

1. Point `HF_ENDPOINT` at a mirror (the hf-hub library reads it):
   ```bash
   export HF_ENDPOINT=https://hf-mirror.com   # one-off; persist in ~/.memvault/.env
   ```
2. Retry; the model lands in `~/.memvault/models/`.
3. Or set `HTTPS_PROXY` in a proxied environment and retry.
4. Make sure `~/.memvault/models` is writable.

> Native model selection: Chinese `bge-small-zh` (~95MB) by default;
> `MEMVAULT_EMBEDDING_MODEL=multilingual` selects `multilingual-e5-base`
> (~470MB, multilingual).

---

## 3. MCP integration / stdio protocol

### 3.1 Claude Desktop: the memvault server doesn't appear

**Checklist**

1. Config file path correct?
   - macOS: `~/Library/Application Support/Claude/claude_desktop_config.json`
   - Windows: `%APPDATA%\Claude\claude_desktop_config.json`
   - Linux: `~/.config/Claude/claude_desktop_config.json`

2. Is `command` an absolute path? Claude Desktop does not expand `~` — use `/Users/...`
3. Is the file valid JSON (trailing commas, comments)? Check with `jq . ~/.config/Claude/claude_desktop_config.json`
4. Does the binary work when launched directly?

   ```bash
   echo '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' | \
     /path/to/memvault-mcp --db ~/.memvault/data.db
   ```

**Restart Claude Desktop** (fully quit, not just close the window), then check logs:

- macOS: `~/Library/Logs/Claude/mcp*.log`
- Windows: `%APPDATA%\Claude\logs\mcp*.log`

### 3.2 `Cannot find module @modelcontextprotocol/sdk`

Claude Code checklist:

```bash
claude mcp list         # registry state
claude mcp add memvault /absolute/path/memvault-mcp -- --db ~/.memvault/data.db
```

### 3.3 stdio MCP exits immediately

**Symptom**: starts and immediately gets `ECONNRESET` / exits.

**Cause**: MCP uses newline-delimited JSON-RPC over stdio. Running
`memvault-mcp` directly in a terminal exits at `EOF` (normal MCP server
behavior). Launch it from an MCP client (Claude Desktop, Claude Code, Cline).

For manual debugging, use the `echo '…' | memvault-mcp` pattern from §3.1.

### 3.4 Server starts slowly (>3s)

**Symptom**: high first-call latency.

**Cause**: SQLite builds the FTS5 index on first open; embedding model loads lazily.

```bash
# See what is slow
RUST_LOG=info memvault-mcp --db ~/.memvault/data.db 2>&1 | head -20
```

Optimizations:

- Enable `MEMVAULT_WAL=1` (on by default)
- Warm up: call `search_memory` once right after client startup
- Put local embedding models on an SSD

---

## 4. Database and file permissions

### 4.1 `permission denied` on `data.db`

**Symptom**

```
sqlx: PoolError: PoolTimedOut ... os error 13 (permission denied)
```

**Fix**

```bash
ls -la ~/.memvault/
# owner should be the current user
sudo chown -R $(id -u):$(id -g) ~/.memvault
chmod 600 ~/.memvault/data.db     # keep other users from reading
chmod 700 ~/.memvault             # the directory itself
```

### 4.2 Docker volume permission errors

```bash
# The container runs as uid 10001 (the memvault user); a different host UID causes owner drift
docker run --rm -v memvault-data:/home/memvault/.memvault \
  --user $(id -u):$(id -g) \
  memvault:local memvault-cli status
```

If root-owned data got written earlier:

```bash
docker run --rm -v memvault-data:/data alpine chown -R 10001:10001 /data
```

### 4.3 Data disk full

```bash
df -h ~/.memvault/
du -sh ~/.memvault/*.db ~/.memvault/models/
```

Clean up:

```bash
memvault-cli decay --archive    # archive low-priority memories
```

To shrink the DB file itself, stop the server and run `sqlite3
~/.memvault/data.db "VACUUM;"` (MemVault's own `backup` uses SQLite's
`VACUUM INTO`, which produces a compacted snapshot without touching the live
file).

---

## 5. Upgrades / migration

### 5.1 Schema errors after upgrading

**Symptom**: after swapping the binary, startup reports
`Sqlite error: no such column: priority` or similar.

**Fix**: MemVault applies schema migrations automatically when the database
opens — there is no separate `migrate` command. If a problem persists:

```bash
# Back up
memvault-cli export --format json --output ~/backup-$(date +%Y%m%d).json
# Upgrade the binary
cargo install --path crates/memvault-cli --locked --force
# Smoke test
memvault-cli search --query "smoke"
```

If an error remains, run the previous binary version against the old DB, export,
then import into a fresh DB.

### 5.2 Cross-machine migration

```bash
# Source
memvault-cli export --format json --output bundle.json
memvault-cli export --format markdown --output ./memories/

# Target (JSON file, or a Markdown directory / a single .md file); importing the same backup twice is idempotent — existing ids are skipped, not overwritten or errored
memvault-cli import --format json --input bundle.json
memvault-cli import --format markdown --input ./memories/
```

Vectors are not ported across machines (semantic search needs rebuilding) —
the background embedding backfill regenerates them, while keyword search works
immediately.

---

## 6. Web Dashboard

### 6.1 Page stuck at "Unreachable"

The settings page shows backend connection state. If it shows Unreachable /
list loading fails:

1. Confirm `memvault-mcp` was started with `--transport http` (not the default `stdio`).
2. Confirm the port: REST defaults to `3777`; the frontend dev server proxies `/api` to `127.0.0.1:3777` (see [INSTALL.md §3.2](INSTALL.md#32-development-mode)).
3. If the `--serve-web` directory doesn't exist, startup logs print
   `--serve-web: ... is not a directory; web dashboard not served` — point it at
   the `dashboard/dist/` produced by `npm run build`.

### 6.2 SPA route refresh 404

`memvault-mcp --serve-web` serves the frontend with `ServeDir` and falls back
to `index.html` for unknown paths. If refreshing `/memories/...` 404s, either
`--serve-web` isn't in effect (see 6.1) or a proxy is swallowing the paths.

### 6.3 `HMR` fails repeatedly / port occupied

The frontend dev server defaults to port `1420`. If busy:

```bash
# Find the occupier
ss -ltnp | grep 1420   # Linux
lsof -iTCP:1420 -sTCP:LISTEN
# Kill it and re-run npm run dev
```

---

## 7. Token budget and injection

### 7.1 `agent_memory too large`

**Symptom**: CLI or MCP logs warn `payload exceeds token_budget`.

**Fix**: budgets are configured per agent in `agents.yaml` (`inject_rules`):

```yaml
agents:
  - id: claude-code
    agent_type: coding-assistant
    inject_rules:
      token_budget: 1500     # default 1500
      max_memories: 8        # default 8
```

Committing more memories is usually the better fix than raising the budget;
trimmed REFERENCE memories are surfaced as pointers with a hint that
`search_memory` can fetch them.

### 7.2 Poor injection quality

MemVault ships seven recall optimizations (word-level tokenization /
multi-field search / synonym expansion / relevance scoring / soft intent
filtering / cross-namespace fallback / automatic embedding backfill). If a
target memory still can't be found, relax filters and widen the result set:

```bash
memvault-cli search --query "<keywords>" --top-k 20 --namespace default
```

---

## 8. `memvault sync` zero-intrusion sync

**Symptom**: after `memvault sync`, Claude Code / Cursor / Cline still don't
pick up the generated AGENTS.md / CLAUDE.md.

**Checklist**

1. Confirm the files landed in the project root:

   ```bash
   ls -la ./CLAUDE.md ./AGENTS.md ./.github/copilot-instructions.md
   git status   # note: CLAUDE.md / AGENTS.md must be committed for agents to read them
   ```

2. Default read paths per agent:

   | Agent | Expected file | Must be git-tracked? |
   |-------|---------------|----------------------|
   | Claude Code | `./CLAUDE.md` or `~/.claude/CLAUDE.md` | yes (project-level) |
   | Cursor | `./.cursorrules` or `./AGENTS.md` | depends on Cursor version |
   | Copilot | `./.github/copilot-instructions.md` | yes |
   | Windsurf | `./AGENTS.md` | yes |
   | Codex CLI | `./AGENTS.md` | yes |

3. If it still doesn't take effect, **restart the client completely** (not just
   reload window) and reopen the session.

### 8.1 Generated directory ignored by .gitignore

If the project's `.gitignore` excludes `CLAUDE.md`, add an explicit
`!CLAUDE.md` negation rule.

### 8.2 Watched files didn't trigger sync

```bash
# Force a full sync
memvault-cli sync --all --namespace default

# Sync only MUST priority
memvault-cli sync --priority MUST

# Generate AGENTS.md only
memvault-cli sync --targets agents.md
```

---

## 9. Build/link dependencies

### 9.1 `pkg-config` can't find openssl

```bash
# Debian / Ubuntu
sudo apt install pkg-config libssl-dev

# Fedora
sudo dnf install pkgconf-pkg-config openssl-devel

# macOS
brew install pkg-config openssl@3
export PKG_CONFIG_PATH="$(brew --prefix openssl@3)/lib/pkgconfig"
```

### 9.2 `linker not found` / missing `cc`

```bash
# Debian / Ubuntu
sudo apt install build-essential

# Fedora
sudo dnf install gcc gcc-c++

# Alpine
sudo apk add build-base

# macOS
xcode-select --install
```

### 9.3 `failed to read rusqlite`

Make sure the `bundled` feature is not disabled. `Cargo.toml` should contain
`rusqlite = { version = "0.40", features = ["bundled"] }`; if you use a custom
feature set, re-add `bundled`.

---

## 10. Collecting diagnostics

Before filing an issue, collect diagnostics (prefer the read-only
`memvault-cli doctor --json` sweep, then `status` for capability state):

```bash
memvault-cli doctor --json
memvault-cli status
memvault-cli list --limit 10
memvault-cli --version
rustc --version
cargo --version
uname -a
```

Attach:

- The output of the commands above
- OS/browser version
- The exact repro commands and logs (wrap in ```` ``` ````, **never** paste real API keys)
- Whether `memvault-cli search --query "smoke"` passes

Reference: what `memvault-cli status` checks:

```
[✓] SQLite WAL ok
[✓] data dir writable: ~/.memvault/
[✓] MCP tool count: 16
[✓] MCP resource count: 2
[✓] (optional) embedding API reachable
[✓] (optional) agent registry parses
```

---

## 11. Known non-bug behavior

| Symptom | Explanation |
|---------|-------------|
| `memvault-cli` prints ANSI colors | disable with `NO_COLOR=1` |
| Brief `not a key …` warning at startup | SQLite startup warning, harmless |
| First embedding call is slow | model lazy-loads; the second call doesn't reload |
| MCP stdio server exits immediately | normal; launch it from a client |
| A `data.db-wal` file appears | SQLite WAL mode, normal while running |

---

## 12. Still stuck?

1. Full reset (keep data):

   ```bash
   mv ~/.memvault ~/.memvault.bak.$(date +%s)
   memvault-cli import --format json --input ~/.memvault.bak.*/export.json
   ```

2. Full reset (discard data):

   ```bash
   rm -rf ~/.memvault/        # no manual init needed; created on first run
   memvault-cli status        # confirm the DB is ready
   ```

3. File an issue: <https://github.com/dreamor/memvault/issues>, attaching the §10 diagnostics
4. Security-sensitive: see [SECURITY.md](../SECURITY.md); don't reproduce sensitive findings in public issues.
