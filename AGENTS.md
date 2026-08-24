# Repository Guidelines

MemVault is a local-first agent-memory system: a Rust/SQLite core exposed via CLI, MCP/REST server, and client plugins. This guide covers structure and contribution conventions.

## Project Structure & Module Organization

- `crates/` — Rust workspace: `memvault-core` (storage, search, decay, embedding), `memvault-cli`, `memvault-mcp` (MCP/REST server), `memvault-proxy`.
- `dashboard/` — React + Vite + TypeScript UI; source and tests in `src/`, production build to `dist/`.
- `obsidian-plugin/`, `vscode-extension/`, `dsh-plugin/` — TypeScript client plugins with `src/` plus `package.json` / `manifest.json`.
- `docs/` — `DESIGN.md` is the authoritative design doc; runbooks and release notes live here. New docs must be added to the `README.md` index.
- `assets/` — brand images; `docs/experiments/` — archived hypothesis-validation research (2026-08-11); `.github/workflows/ci.yml` gates every PR.

## Build, Test, and Development Commands

- `cargo fmt --all` — format; `cargo clippy --all-targets --all-features -- -D warnings` — lint; `cargo test` — tests; `cargo build --release` — all binaries.
- `cargo llvm-cov --workspace` — coverage (~86% line target); `cargo audit` — dependency CVE scan.
- Dashboard: `cd dashboard && npm ci && npm run dev`, or `npm run build` and `npm test` (Vitest).
- Plugins: `cd <dir> && npm install && npm run build && npm test` (some need `--legacy-peer-deps`).
- Docker: `docker build -t memvault:local .`; run server with `memvault-mcp --transport http --port 3777` (default `stdio`).

## Coding Style & Naming Conventions

- Rust (edition 2024): rustfmt-formatted, clippy-clean with `-D warnings`. No `unwrap()` outside tests — application errors go through `anyhow`, domain errors through `thiserror`. Use `snake_case` and `tracing`.
- TypeScript: strict mode in every package (`noUnusedLocals`/`noUnusedParameters` on); `camelCase` functions, `PascalCase` components; `tsc` is the sole style gate.

## Testing Guidelines

- Rust: `#[cfg(test)]` unit tests plus integration tests; new/changed modules need ≥80% coverage, verified with `cargo llvm-cov`.
- TypeScript: Vitest; colocate `*.test.ts(x)` beside source (e.g., `App.test.tsx`, `api.test.ts`). CI runs all suites.

## Commit & Pull Request Guidelines

- Follow Conventional Commits: `<type>(<scope>): <summary>` — e.g., `feat(core):`, `fix(cli):`. Types: `feat`, `fix`, `refactor`, `docs`, `test`, `chore`, `ci`.
- Add a `CHANGELOG.md` entry under `[Unreleased]`; mark breaking changes with a `BREAKING CHANGE:` footer.
- PRs use `.github/PULL_REQUEST_TEMPLATE.md`: link issues (`Closes #123`), select change type, run the self-check list (fmt, clippy, tests, docs synced, no new `unwrap()`, no secrets), and screenshot UI changes.

## Security & Configuration

- All config is environment-based — see `.env.example`; never commit keys. Report vulnerabilities via `SECURITY.md`, not public issues.
