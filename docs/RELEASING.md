# Releasing MemVault

Current state: **v0.4.3 is shipped everywhere**. crates.io (4 crates), npm
(`@dreamor/dsh-memvault`), Homebrew (`dreamor/tap`), ghcr.io + Docker Hub, the
official MCP Registry (`io.github.dreamor/memvault`) and the Obsidian community
directory are all live, and the repo is public. crates.io and npm use OIDC
trusted publishing (no token secrets); the Docker Hub mirror and the MCP
Registry listing are configured and verified. Everything below reflects that
reality.

Pushing a `v*` tag triggers `.github/workflows/release.yml`, which builds and
attaches to the GitHub Release:

- Rust binaries (`memvault-cli`, `memvault-mcp`, `memvault-proxy`) for **4 targets**:
  - Linux x86_64 (`x86_64-unknown-linux-gnu`, `.tar.gz`)
  - Linux ARM64 (`aarch64-unknown-linux-gnu`, `.tar.gz`, built on the GitHub arm64 runner)
  - macOS ARM64 (`aarch64-apple-darwin`, `.tar.gz`)
  - Windows x86_64 (`x86_64-pc-windows-msvc`, `.zip`)
  - Intel macOS has **no prebuilt binaries** (fastembed's bundled ONNX Runtime
    ships no `x86_64-apple-darwin` artifacts); Intel Mac users build from source.
  - Every archive is uploaded together with a `.sha256`; the Release also
    contains a summary `SHA256SUMS` covering all assets.
- A Docker image, pushed to `ghcr.io/<repo>:<tag>` and `:latest`, and mirrored
  to `docker.io/dreamor/memvault:<tag>` / `:latest` (the Docker Hub steps are
  secrets-gated on `DOCKERHUB_USERNAME` / `DOCKERHUB_TOKEN`).
- The Web Dashboard as a `dist/` archive (`memvault-dashboard-<tag>.tar.gz`), served by `memvault-mcp --serve-web`
- The Obsidian plugin is **not** published here — plugin releases live in the
  dedicated [`dreamor/memvault-obsidian`](https://github.com/dreamor/memvault-obsidian)
  repo; see §2.

The same run then publishes the package channels, once
`github-release` succeeds: **crates.io** (4 crates, OIDC), **npm**
(`@dreamor/dsh-memvault`, OIDC, versioned from the tag) and the **Homebrew
formula** (`HOMEBREW_TAP_TOKEN`). A `workflow_dispatch` run with
`publish-only = true` re-runs just those three, which is the recovery path when
a tag run dies after the GitHub Release is out; its `publish-*` inputs default
to on and only apply to dispatched runs (a tag push always publishes).

The steps below are **not** automated and must be done by hand after that.

## 0. One-line installer (no per-release work)

`scripts/install.sh` (Linux/macOS) and `scripts/install.ps1` (Windows) consume
the Release assets directly: they detect the host platform, download the
matching archive, verify it against `SHA256SUMS`, and install to
`~/.memvault/bin` (or `%LOCALAPPDATA%\memvault\bin`). They always pull
`latest`, so nothing to do per tag.

## 1. crates.io (automatic; these are fallbacks)

Published by every `v*` tag run. If it has to be redone:

- Run the manual **Publish (manual)** workflow (job `crates-io`). It
  authenticates via OIDC trusted publishing (`rust-lang/crates-io-auth-action@v1`,
  no `CRATES_IO_TOKEN` needed — trusted publishers are configured on crates.io
  as owner=dreamor repo=memvault workflow=publish.yml for each crate) and
  publishes in dependency order with retries for index-propagation lag.
- Or publish locally:

```bash
for crate in memvault-core memvault-cli memvault-mcp memvault-proxy; do
  cargo publish -p "$crate" --allow-dirty
done
```

`memvault-core` must land first. The other crates already declare
`memvault-core = { path = "...", version = "0.4.x" }`, so publishing replaces
the path dependency with the crates.io release automatically. Only the very
first publish of a new crate requires a one-time local `cargo login` token;
afterwards trusted publishing covers updates.

## 2. Obsidian plugin releases (dreamor/memvault-obsidian)

The plugin has a dedicated repo of record,
[`dreamor/memvault-obsidian`](https://github.com/dreamor/memvault-obsidian) — BRAT and
the community directory point there. The plugin is **listed** on the community
portal. Source of truth for code is THIS monorepo (`obsidian-plugin/`, "mode
A"); the release repo is a thin shell whose workflow checks out the monorepo,
builds, and attaches provenance attestations. Syncing and releasing run via
`.github/workflows/sync-obsidian-plugin.yml`:

- Edit `obsidian-plugin/**` here as usual. Do **not** edit the release repo directly:
  the sync mirrors `src/` exactly and refuses to run if it ever drifts ahead of this
  repo, so changes made there must be ported back into `obsidian-plugin/` first.
- **Code-only changes**: pushing to this repo's master syncs the sources into
  `memvault-obsidian` master. No release.
- **Releasing the plugin**: bump `version` in `obsidian-plugin/manifest.json`. The sync
  then appends the `versions.json` entry and pushes the `v<version>` tag there, which
  triggers its release (tag must equal the manifest version — that's the gate). Assets
  go out as individual `main.js` / `manifest.json` / `styles.css`, the exact shape BRAT
  and the community installer fetch by filename.
- **Community list**: already handled — the plugin is listed. Future version
  bumps need no list interaction.

The workflow needs the `OBSIDIAN_PLUGIN_SYNC_TOKEN` secret in this repo: a fine-grained
PAT scoped to `dreamor/memvault-obsidian` only, with **Contents: read and write**. A PAT
is mandatory — pushes made with Actions' default `GITHUB_TOKEN` do not trigger workflows
in the target repo, so the tag would never fire a release.

## 3. Homebrew tap

Homebrew uses the tap repository `dreamor/homebrew-tap` (already live).
Published by every `v*` tag run from the release assets. To regenerate by hand:

```bash
./scripts/update-homebrew-formula.sh v0.4.3 > ../homebrew-tap/Formula/memvault.rb
cd ../homebrew-tap && git add . && git commit -m "memvault 0.4.3" && git push
```

Users install with:

```bash
brew install dreamor/tap/memvault
```

The formula pins `depends_on arch: :arm64` — there are no prebuilt Intel macOS
binaries (ONNX Runtime), so the formula targets Apple Silicon only. Linux users
install via `scripts/install.sh` or `cargo install` instead.

## 4. npm (dsh plugin) — automatic; these are fallbacks

Published by every `v*` tag run (the job syncs `dsh-plugin/package.json` to the
tag first). To redo it: the **Publish (manual)** workflow (job `npm-dsh`,
trusted publishing — no `NPM_TOKEN`), or locally:

```bash
cd dsh-plugin
npm ci && npm run build && npm test
npm publish --access public   # publishes @dreamor/dsh-memvault
```

Use a Node version whose bundled npm supports OIDC trusted publishing
(npm >= 11.5.1, i.e. Node 24) for the trusted-publisher path.

## 5. MCP Registry (re-publication)

`integrations/mcp-registry/server.json` (schema 2025-12-11) is the listing
source. To publish a new version:

1. Bump `version` and the `packages[0].identifier` image tag in `server.json`.
   The identifier must name a tag that **exists in ghcr**: `release.yml` tags
   the image with the git tag, so it is `ghcr.io/dreamor/memvault:v0.4.3`
   (bare `0.4.x` tags only ever existed when someone ran `rebuild-docker.yml`
   by hand, and the registry rejects an identifier it cannot pull — `400 ...
   OCI image 'ghcr.io/dreamor/memvault:0.4.3' does not exist`).
2. Ensure the Docker image for that tag exists in ghcr (it does after
   `release.yml`; for label-only metadata changes there is
   `rebuild-docker.yml`).
3. Run the manual **Publish MCP Registry listing** workflow (job
   `mcp-registry`). It authenticates with the workflow's GitHub OIDC token
   (`mcp-publisher login github-oidc`), so nothing is stored and nothing
   expires.

Local publishing still works, but a laptop JWT dies after a while and fails
with `401 ... token has invalid claims: token is expired`; re-run
`mcp-publisher login github` when that happens:

```bash
mcp-publisher publish integrations/mcp-registry/server.json
```

Two bite-points: the registry `description` must be ≤ 100 chars, and the image
OCI label `io.modelcontextprotocol.server.name` must equal the `server.json`
`name` (`io.github.dreamor/memvault` — baked into the Dockerfile). Glama and
mcp.so pick the listing up automatically; PulseMCP is closed to new listings
and Smithery does not apply (HTTP-URL-only model vs. local-first stdio).

## 6. Web Dashboard artifact

`memvault-mcp` **embeds the dashboard in the binary** (rust-embed over
`crates/memvault-mcp/assets/web`, synced from `dashboard/dist` via
`scripts/sync-dashboard-assets.sh`; CI's dashboard job fails on drift). Every
distribution channel therefore ships the UI by default: `cargo install`,
Homebrew, the Release binaries / install scripts, and the Docker image (whose
`web` stage rebuilds `dist/` fresh before the Rust build). `--serve-web
<dir>` / `MEMVAULT_SERVE_WEB=<dir>` override the embedded assets; `--transport
http` alone serves them.

The `dashboard-web` CI job still attaches `memvault-dashboard-<tag>.tar.gz`
to the GitHub Release as a standalone archive for installs that don't upgrade
the binary. There is no desktop app, so **no macOS signing/notarization or
per-platform Windows/Linux packaging is needed** — the archive is served by
`memvault-mcp --serve-web <dist-dir>` on any OS.
