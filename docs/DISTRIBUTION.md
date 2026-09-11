# MemVault Distribution Guide

Complete map of how MemVault ships to users, what is automated, what is manual,
and which credentials each channel needs. Operational steps live in
[`RELEASING.md`](RELEASING.md); this file is the overview + decision guide.

## Channel matrix

> **Status (2026-09-11)**: repo is **public**; channels 1-6 verified end-to-end for v0.3.0 (4-platform archives, ghcr multi-arch, brew, install.sh). Obsidian plugin is **listed** on the community portal via the standalone repo `dreamor/memvault-obsidian` (manifest at repo root, tag-driven release CI). Official MCP Registry listing is **active** (`io.github.dreamor/memvault` via `mcp-publisher` API, no PRs; needs the OCI label `io.modelcontextprotocol.server.name` on the image + description <= 100 chars). Docker Hub dual-push verified (`docker.io/dreamor/memvault`); workflows gate on `DOCKERHUB_*` secrets. npm scope is `@dreamor/dsh-memvault`.

| # | Channel | What ships | Automation | Credential needed |
|---|---------|-----------|------------|-------------------|
| 1 | GitHub Releases | Rust binaries × 4 targets (`tar.gz`/`zip`) + dashboard `dist` + Obsidian assets + `SHA256SUMS` | **fully automated** on `v*` tag | — |
| 2 | Docker (ghcr.io) | server image `:tag` + `:latest` | **fully automated** | `GITHUB_TOKEN` (built-in) |
| 3 | CLI installers | `scripts/install.sh` (Linux/macOS), `scripts/install.ps1` (Windows) | none per release (always `latest`) | — |
| 4 | crates.io | `memvault-core`, `memvault-cli`, `memvault-mcp`, `memvault-proxy` | manual: `publish.yml` job or local `cargo publish` | none (OIDC trusted publishing; initial publish done locally) |
| 5 | Obsidian | BRAT (instant) + community list (reviewed PR) | manual PR only | GitHub account |
| 6 | Homebrew | `memvault` formula via tap (`dreamor/homebrew-tap`) | `scripts/update-homebrew-formula.sh` generates formula | GitHub account |
| 7 | npm | `@dreamor/dsh-memvault` (dsh plugin) | manual: `publish.yml` job or `npm publish` | `NPM_TOKEN` |
| 8 | MCP registries | MCP server listing (discoverability) | manual submissions | account per registry |
| 9 | Docker Hub (optional) | image mirror `docker.io` | automated (secrets-gated) | `DOCKERHUB_USERNAME` / `DOCKERHUB_TOKEN` (configured) |

Channels 1–3 require no credentials and are the backbone. 4–8 need one-time
secret setup in the repo. 9 is discoverability/optional.

## Manual publish workflow

`.github/workflows/publish.yml` (trigger: **Actions → Publish (manual)**) runs
every job that has its token secret configured and skips the rest, so it is
safe to enable incrementally:

- `crates-io`  — requires `CRATES_IO_TOKEN`; publishes core → cli → mcp → proxy
- `npm-dsh`    — requires `NPM_TOKEN`

## MCP ecosystem registries (channel 10)

These listings make MemVault discoverable by MCP-capable agents (Claude
Desktop, Cursor, Cline, …) and are worth submitting once the project is stable:

- **Official MCP registry** — `modelcontextprotocol/registry` (curated, becoming
  the standard). Submit a pull request adding `memvault-mcp` with its
  installation command (`install.sh` or `cargo install memvault-mcp`).
- **smithery.ai** — hosted MCP servers; supports Docker or command deploy.
- **mcp.so** — index + reviews, quick registration.
- **Glama** — MCP server directory + uptime monitoring.
- **PulseMCP** — MCP marketplace/newsletter.

Each listing references the same protocol and install command, so the ongoing
cost is one registration per registry, not per release.

## Notable gaps / roadmap

- **macOS signing & notarization**: not required today (unsigned GitHub
  binaries, same as most OSS CLI tools). If macOS friction shows up in
  support, notarize the binaries and switch Homebrew to signed artifacts.
- **Windows ARM64**: no `aarch64-pc-windows-msvc` build yet (add the target to
  the release matrix when the toolchain cost justifies it).
- **Intel macOS**: no prebuilt binaries — fastembed's bundled ONNX Runtime has no
  `x86_64-apple-darwin` artifacts, so that target cannot be cross-built from the
  current dependency set. Intel Mac users install from source
  (`git clone … && cargo build --release`); `scripts/install.sh` says so explicitly.
- **Linux ARM64 via cross-compile**: currently built natively on GitHub's
  arm64 runner; if that runner is unavailable in your plan, fall back to
  `cross`/QEMU.
- **Self-update**: `cargo install` users already get `cargo-update`;
  installer users can re-run install.sh/install.ps1. A built-in `memvault
  upgrade` command is a possible future enhancement.
- **Binary signing**: `SHA256SUMS` enables integrity checking; adding
  `minisign`/`cosign` signatures is a hardening step for later.

## Quick decision guide

- "I want the widest reach with zero ops" → keep 1–3, add 4 (crates.io), 10 (MCP registries).
- "I want enterprise/self-host users" → add 2 (Docker Hub mirror) + 11; keep Linux ARM64 builds.
- "I want plugin ecosystem presence" → 5 (Obsidian), 7 (npm).
- "I want macOS developer convenience" → 8 (Homebrew tap).
