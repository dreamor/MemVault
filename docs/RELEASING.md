# Releasing MemVault

Current state: **v0.3.0 is shipped everywhere**. crates.io (4 crates), npm
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

Everything above is fully automated. The steps below are **not**, and must be
done by hand after the GitHub Release is published.

## 0. One-line installer (no per-release work)

`scripts/install.sh` (Linux/macOS) and `scripts/install.ps1` (Windows) consume
the Release assets directly: they detect the host platform, download the
matching archive, verify it against `SHA256SUMS`, and install to
`~/.memvault/bin` (or `%LOCALAPPDATA%\memvault\bin`). They always pull
`latest`, so nothing to do per tag.

## 1. crates.io

Publishing is not tag-triggered. Two options after the GitHub Release:

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
`memvault-core = { path = "...", version = "0.3.x" }`, so publishing replaces
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
After the tag is published, generate the formula from the release assets:

```bash
./scripts/update-homebrew-formula.sh v0.3.0 > ../homebrew-tap/Formula/memvault.rb
cd ../homebrew-tap && git add . && git commit -m "memvault 0.3.0" && git push
```

Users install with:

```bash
brew install dreamor/tap/memvault
```

The formula pins `depends_on arch: :arm64` — there are no prebuilt Intel macOS
binaries (ONNX Runtime), so the formula targets Apple Silicon only. Linux users
install via `scripts/install.sh` or `cargo install` instead.

## 4. npm (dsh plugin)

Run the manual **Publish (manual)** workflow (job `npm-dsh`, trusted publishing
— no `NPM_TOKEN`), or locally:

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

1. Bump `version` and the `packages[0].identifier` image tag (e.g.
   `ghcr.io/dreamor/memvault:0.3.x`) in `server.json`.
2. Ensure the Docker image for that tag exists in ghcr (it does after
   `release.yml`; for label-only metadata changes there is
   `rebuild-docker.yml`).
3. Run:

```bash
mcp-publisher publish integrations/mcp-registry/server.json
```

Two bite-points: the registry `description` must be ≤ 100 chars, and the image
OCI label `io.modelcontextprotocol.server.name` must equal the `server.json`
`name` (`io.github.dreamor/memvault` — baked into the Dockerfile). Glama and
mcp.so pick the listing up automatically; PulseMCP is closed to new listings
and Smithery does not apply (HTTP-URL-only model vs. local-first stdio).

## 6. Web Dashboard artifact

The `dashboard-web` CI job runs `npm ci && npm run build` in `dashboard/` and
tars the resulting `dist/` into `memvault-dashboard-<tag>.tar.gz`, attached to
the GitHub Release. There is no desktop app, so **no macOS signing/notarization
or per-platform Windows/Linux packaging is needed** — the archive is served by
`memvault-mcp --serve-web <dist-dir>` on any OS.
