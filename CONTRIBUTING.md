# Contributing Guide

Thanks for your interest in MemVault! This document explains how to get involved with the project.

## Development Workflow

We use a PR-centric collaboration model:

1. **Fork** this repository and clone it locally
2. Branch off `master`: `git switch -c feat/<short-desc>`
3. **Write tests first** (TDD) — see "Development Conventions" below
4. Implement the feature / fix the bug
5. `cargo fmt` + `cargo clippy` + `cargo test` all pass
6. Push the branch and open a PR

## Available Commands

<!-- AUTO-GENERATED: commands reference -->

### Core Rust Crates

| Command | Description |
|------|------|
| `cargo build --release` | Release build of CLI (`memvault-cli`) + MCP Server (`memvault-mcp`) + MCP Proxy (`memvault-proxy`) |
| `cargo build -p memvault-cli` | Build the CLI only |
| `cargo build -p memvault-mcp` | Build the MCP Server only |
| `cargo build -p memvault-core` | Build the core library only |
| `cargo test` | Run all tests (~849 tests: core 580 + e2e 19, MCP 120, proxy 82, CLI 48) |
| `cargo test -- --nocapture` | Run tests with `println!` output visible |
| `cargo test -p memvault-core` | Run only the core library's tests |
| `cargo clippy --all-targets --all-features -- -D warnings` | Lint check (zero warnings) |
| `cargo fmt` | Format code |
| `cargo llvm-cov --workspace --all-features` | Coverage gate (CI: line ≥92% / region ≥90% / function ≥85%) |
| `cargo audit` | Security audit (dependency CVE scan) |

### Running the MCP Server

| Command | Description |
|------|------|
| `memvault-mcp --transport stdio` | (default) stdio mode, for Claude Desktop |
| `memvault-mcp --transport sse --port 3777` | SSE network mode, multi-client support (default port 3777) |
| `memvault-mcp --transport http --port 3777` | REST API mode |

### memvault sync

| Command | Description |
|------|------|
| `memvault-cli sync` | Generate CLAUDE.md / AGENTS.md and similar instruction files |
| `memvault-cli sync --watch` | Polling mode — regenerates automatically on DB changes |
| `memvault-cli sync --dir /path/to/project` | Target a specific project directory |

### Web Dashboard (browser)

| Command | Description |
|------|------|
| `cd dashboard && npm ci && npm run dev` | Start the Vite dev server (proxies /api → 127.0.0.1:3777) |
| `cd dashboard && npm run build` | TypeScript check + production build (outputs `dist/`) |
| `cd dashboard && npm run preview` | Preview the Vite production build |
| `cd dashboard && npm test` | Frontend unit tests (Vitest) |

### VS Code Extension

| Command | Description |
|------|------|
| `cd vscode-extension && npm run compile` | Compile the extension |
| `cd vscode-extension && npm run watch` | Compile in watch mode |
| `cd vscode-extension && npm test` | Run the extension's unit tests (vitest) |

### Obsidian Plugin

| Command | Description |
|------|------|
| `cd obsidian-plugin && npm run build` | Build the plugin |
| `cd obsidian-plugin && npm run watch` | Compile in watch mode |
| `cd obsidian-plugin && npm test` | Run the plugin's unit tests (vitest) |

### DeepSeek Harness bridge plugin (dsh-plugin)

| Command | Description |
|------|------|
| `cd dsh-plugin && npm install --legacy-peer-deps` | Install deps (peer deps pin `0.0.1-rc.1`, needs `--legacy-peer-deps`) |
| `cd dsh-plugin && npm run build` | Build the bridge plugin (`tsc --strict`, outputs `dist/`) |
| `cd dsh-plugin && npm run watch` | Compile in watch mode |
| `cd dsh-plugin && npm test` | Run the plugin's unit tests (vitest) |

> This plugin auto-injects MemVault memories into the dsh system prompt and auto-extracts at the end of each turn. Design notes were cross-referenced against the real dsh source.

### Docker

| Command | Description |
|------|------|
| `docker build -t memvault:local .` | Build a local Docker image |
| `docker run --rm memvault:local --help` | Show CLI help |

<!-- AUTO-GENERATED -->

### Environment Variables

| Variable | Required | Description | Default |
|------|------|------|--------|
| `MEMVAULT_EMBEDDING_PROVIDER` | No | Provider: `native` (in-process inference, default) / `auto` (prefers local Ollama, falls back to native) / `ollama` / `local` / `openai` / `openai-compatible` / `none` | `native` |
| `MEMVAULT_EMBEDDING_MODEL` | No | Model: for native, `zh` (default) or `multilingual`; for API providers, the exact model name | `bge-small-zh` (native) / `text-embedding-3-small` (API) |
| `MEMVAULT_EMBEDDING_DIM` | No | Embedding dimension (auto-detected for native, no need to set) | auto |
| `MEMVAULT_EMBEDDING_API_KEY` (legacy fallback: `OPENAI_API_KEY`) | No | API key for remote providers (not needed for native local inference) | — |
| `MEMVAULT_EMBEDDING_API_BASE` | No | Any OpenAI-compatible endpoint (OpenAI / Azure / vLLM / gateway); the legacy `OPENAI_API_BASE` alias is removed | `https://api.openai.com/v1` |
| `MEMVAULT_LLM_EXTRACTION_PROVIDER` | No | LLM extraction (`extract_memories(mode=llm)` / reflection / relation extraction): unset/`auto` probes local Ollama and falls back to rule-based otherwise; `ollama`/`local`; `openai`/`openai-compatible`; `off`/`disabled`/`none` forces rule-based | auto-probe |
| `MEMVAULT_RELATIONS` | No | When `on`, `extract_memories(mode=llm)` additionally persists `supports`/`contradicts`/`sourced_from` relation triples | off |
| `MEMVAULT_DELTA_WRITE` | No | On save, checks for near-duplicates in the same namespace first (skips exact repeats, merges residuals into similar entries); `false` disables it (aliases `off`/`0` accepted); bypass a single save with `--force`/`force_insert` | true |
| `MEMVAULT_CONTEXT_NGRAM_WINDOW` | No | Session n-gram retrieval window: how many recent turns the proxy injection engine uses to build a recency-weighted retrieval key | `5` |
| `MEMVAULT_DB` | No | Database path | `~/.memvault/data.db` |
| `MEMVAULT_DB_POOL_SIZE` | No | SQLite connection pool size | `5` |
| `MEMVAULT_HOME` | No | Override the base data directory (model cache, DB location) | `~/.memvault` |
| `MEMVAULT_CORS_ORIGIN` | No | REST mode CORS allow-list: comma-separated origins, or `*` to allow all (trusted networks only) | localhost-only |
| `RUST_LOG` | No | Log level | `info` |

Full details in [`.env.example`](.env.example).

## Development Conventions

### Rust Code

- `cargo +stable fmt` must produce no diff
- `cargo +stable clippy --all-targets --all-features -- -D warnings` must pass
- Test coverage: unit + integration (workspace gate: line ≥92% / region ≥90% / function ≥85%)
- Error handling: recoverable errors go through `anyhow`, domain errors through custom `thiserror` types — **no `unwrap()`** (except in tests or genuinely unreachable branches)
- Public API changes must be reflected in the corresponding section of `docs/DESIGN.md`

### Commit Messages

Follow [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <description>

[optional body]

[optional footer(s)]
```

Common `type`s: `feat` / `fix` / `refactor` / `docs` / `test` / `chore` / `perf` / `ci` / `build`

### Documentation

- `docs/DESIGN.md` is the single source of truth for design — architecture/interface changes must update the relevant section
- New `docs/*.md` files must be registered in the `README.md` documentation index

## Pre-PR Checklist

- [ ] `cargo fmt` + `cargo clippy` + `cargo test` all pass
- [ ] Added an entry under `CHANGELOG.md`'s `[Unreleased]` section
- [ ] Breaking changes are noted with a `BREAKING CHANGE:` footer
- [ ] Opened an issue to discuss before writing into a new domain (reduces rework risk)

## Code of Conduct

Please read [CODE_OF_CONDUCT.md](.github/CODE_OF_CONDUCT.md) — all interactions are governed by it.

## Contact

- Bugs / feature requests: [GitHub Issues](https://github.com/dreamor/memvault/issues)
- Security issues: see [SECURITY.md](SECURITY.md) (**do not** report via public issues)
- Design & discussion: [GitHub Discussions](https://github.com/dreamor/memvault/discussions)
