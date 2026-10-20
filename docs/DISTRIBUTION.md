# MemVault Distribution Guide

Complete map of how MemVault ships to users, what is automated, what is manual,
and which credentials each channel needs. Operational steps live in
[`RELEASING.md`](RELEASING.md); this file is the overview + decision guide.

## Channel matrix

| # | Channel | What ships | Automation | Credential needed |
|---|---------|-----------|------------|-------------------|
| 1 | GitHub Releases | Rust binaries × 4 targets (`tar.gz`/`zip`) + dashboard `dist` archive + per-archive `.sha256` + `SHA256SUMS` | **fully automated** on `v*` tag (`release.yml`) | — |
| 2 | Docker | server image `:v<version>` + `:latest` on `ghcr.io/dreamor/memvault` **and** mirrored to `docker.io/dreamor/memvault` | **fully automated** (`release.yml`); the Docker Hub mirror fails the run if its secrets are missing — opt out with `publish-dockerhub=false` | `GITHUB_TOKEN` (built-in); `DOCKERHUB_USERNAME` / `DOCKERHUB_TOKEN` for Docker Hub |
| 3 | CLI installers | `scripts/install.sh` (Linux/macOS ARM64), `scripts/install.ps1` (Windows x86_64) | none per release (installers always pull `latest`) | — |
| 4 | crates.io | `memvault-core`, `memvault-cli`, `memvault-mcp`, `memvault-proxy` | semi-automated: **Publish (manual)** workflow (`publish.yml`) or local `cargo publish` | none — OIDC trusted publishing (`rust-lang/crates-io-auth-action@v1`); only the very first publish of a crate ever needs `cargo login` |
| 5 | Obsidian | BRAT (instant) + community list (listed) | release loop automated: bump `obsidian-plugin/manifest.json` here → sync workflow appends `versions.json` and pushes a `v<version>` tag in `dreamor/memvault-obsidian` → its release CI builds the plugin | `OBSIDIAN_PLUGIN_SYNC_TOKEN` in this repo (PAT scoped to the release repo only) |
| 6 | Homebrew | `memvault` formula in tap `dreamor/homebrew-tap` (Apple Silicon only — no Intel prebuilt binaries) | **fully automated** on `v*` tag (`release-npm.yml` pushes the tap; `scripts/update-homebrew-formula.sh` generates the formula); opt out with `publish-homebrew=false` on that workflow's dispatch | `HOMEBREW_TAP_TOKEN` (PAT with Contents: write on the tap) |
| 7 | npm binaries | `@dreamor/memvault-bin` — the prebuilt Apple Silicon macOS binaries, as the transport for the Homebrew formula | **fully automated** on `v*` tag (`release-npm.yml` builds and publishes via OIDC, waits for the mirror, then repoints the tap) | none — npm trusted publishing (OIDC) for the publish; `HOMEBREW_TAP_TOKEN` for the tap push |
| 8 | npm | `@dreamor/dsh-memvault` (dsh plugin) | semi-automated: **Publish (manual)** workflow (job `npm-dsh`) or local `npm publish` | none — npm trusted publishing (requires Node 24 for OIDC support) |
| 9 | MCP ecosystem registries | MCP server listing `io.github.dreamor/memvault` (discoverability) | **fully automated** on `v*` tag (`release.yml` job `publish-mcp`); manual repair via the **Publish MCP Registry listing** workflow (`repoint_version` / `retire_version`); Glama and mcp.so sync automatically from the official listing | none — the workflow's own GitHub OIDC token |

Channels 1–3 need no credentials at all and form the backbone. Channels 4, 7,
8 and 9 authenticate via OIDC trusted publishing (crates.io per-package
settings, npmjs.com package settings) or the workflow's own GitHub OIDC token
(MCP Registry), so no long-lived secret is stored. Channels 6 and 7 also use the
`HOMEBREW_TAP_TOKEN` secret, because they push to the tap repository. Channel 5
(Obsidian) needs one-time account setup in its own repo.

Every release-locked channel is verified after each release, and daily, against
the latest GitHub Release — see [`RELEASING.md`](RELEASING.md) "Verification".

Notes that prevent regressions:

- The Web Dashboard is embedded in every `memvault-mcp` binary (rust-embed
  from `crates/memvault-mcp/assets/web`) and is auto-started by the CLI /
  installers / Homebrew (`memvault-cli server start`, idempotent; see
  `docs/INSTALL.md §2.6`). The Obsidian plugin, npm `@dreamor/dsh-memvault`
  and the MCP registry listing remain REST/MCP clients of an external backend
  and never bundle the UI themselves.
- `brew install memvault` downloads `@dreamor/memvault-bin` from
  `registry.npmmirror.com`, **not** the GitHub Release archive: the GitHub CDN is
  shaped to single-digit KB/s on some corporate networks, so the 48 MB release
  tarball took tens of minutes. The formula pins the *mirror* tarball's sha256
  and keeps `registry.npmjs.org` as a Homebrew `mirror` fallback (npm mirrors
  serve identical bytes). Consequence: the npm publish must land before the tap
  is repointed — that ordering is `release-npm.yml`'s `update-homebrew` job
  depending on its `publish-npm` job, and it is why `release.yml` has no
  `update-homebrew` job (one workflow, one writer).
- The npm scope is `@dreamor/dsh-memvault`. The `@memvault` scope was
  **rejected** during setup — that org is owned by someone else. Never present
  `@memvault/…` as a live package.
- The Docker image carries the OCI label
  `io.modelcontextprotocol.server.name="io.github.dreamor/memvault"`, whose
  value must equal the `name` in `integrations/mcp-registry/server.json` for
  MCP Registry ownership verification to pass. The Dockerfile bakes the label
  in; changing the registry name requires an image rebuild (see
  `rebuild-docker.yml`, which exists precisely for this).
- GitHub Releases binaries are **unsigned**; `.sha256` files and `SHA256SUMS`
  provide integrity checking.
- **One version, every channel.** `scripts/check-release-versions.py` is the
  single list of version-bearing files; `release.yml`'s `verify-version` job
  blocks a tag push that disagrees with any of them and CI runs the same check
  on every build (see [`RELEASING.md`](RELEASING.md) §0).
- **Docker tags are always `v<version>`** — never bare `0.4.x` and never a
  branch name. The only bare tag, `0.3.0`, came from `rebuild-docker.yml`'s old
  free-form default; it and `v0.4.1` (a version that was never tagged or
  released on any other channel — its content shipped in 0.4.2) have since been
  **deleted** from ghcr and Docker Hub, and the `0.3.0` MCP listing that pointed
  at the bare tag has been retired (`status: deleted`). Don't pin either.

## Manual publish workflow

`.github/workflows/publish.yml` (trigger: **Actions → Publish (manual)**) is
safe to run at any time after a tag. Its publish jobs do **not** require token
secrets — both authenticate with short-lived OIDC tokens issued to the workflow:

- `crates-io` — authenticates via `rust-lang/crates-io-auth-action@v1`, then
  publishes core → cli → mcp → proxy with retry (a freshly published crate can
  take a while to propagate through the crates.io index, and the dependent
  crates resolve `memvault-core` from the index, not from the local path).
- `npm-dsh` — builds + tests the dsh plugin, then `npm publish --access public`
  under npm trusted publishing (Node 24, npm >= 11.5.1; provenance is generated
  automatically for a public repo + public package).
- `plugin-release-checks` — verifies that the plugin surfaces shipped as part
  of the repo (Claude Code marketplace, Gemini extension, Qoder/Grok manifests,
  MCP registry `server.json`, rule copies) are valid at release time.

## MCP ecosystem registries

The **official MCP registry** listing (`io.github.dreamor/memvault`,
`integrations/mcp-registry/server.json`) is the source that other directories
mirror. It is live and API-driven (`mcp-publisher` — no pull requests). See
[`RELEASING.md`](RELEASING.md) §5 for the re-publish procedure and its two
bite-points (description ≤ 100 chars; OCI label must match `server.json` name).

- **Glama** and **mcp.so** sync automatically from the official registry — no
  separate submission model.
- **PulseMCP** has closed its intake; no action exists.
- **Smithery** is not applicable: its listing model only accepts hosted
  HTTP-URL servers, which conflicts with MemVault's local-first stdio design.
  The old `smithery.yaml` has been removed.

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
- **Linux ARM64**: built natively on GitHub's arm64 runner; if that runner is
  ever unavailable in the plan, fall back to `cross`/QEMU.
- **Self-update**: `cargo install` users can use `cargo-update`; installer users
  re-run `install.sh`/`install.ps1`. A built-in `memvault-cli upgrade` command is a
  possible future enhancement.
- **Binary signing**: `SHA256SUMS` enables integrity checking; adding
  `minisign`/`cosign` signatures is a hardening step for later.

## Quick decision guide

- "I want the widest reach with zero ops" → keep 1–3, 4 and 8 are already live.
- "I want enterprise/self-host users" → Docker Hub mirror (in 2) is already live; keep Linux ARM64 builds.
- "I want plugin ecosystem presence" → 5 (Obsidian) and 7 (npm) are already live.
- "I want macOS developer convenience" → 6 (Homebrew tap) is already live.
