# Releasing MemVault

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
- A Docker image, pushed to `ghcr.io/<repo>:<tag>` and `:latest`
- The Web Dashboard as a `dist/` archive (`memvault-dashboard-<tag>.tar.gz`), served by `memvault-mcp --serve-web`
- The Obsidian plugin is **not** published here anymore — plugin releases live in
  the dedicated [`dreamor/memvault-obsidian`](https://github.com/dreamor/memvault-obsidian)
  repo; see §2.

Everything above is fully automated. The steps below are **not**, and must be
done by hand after the GitHub Release is published.

> **Status (2026-09-11)**: v0.3.0 shipped everywhere. crates.io (4 crates) + npm (`@dreamor/dsh-memvault`) + brew + ghcr + Docker Hub + official MCP Registry all live. Next-release notes: crates.io trusted publishing still needs one-time per-crate web config (owner=dreamor repo=memvault workflow=publish.yml); npm trusted publishing already active; docker.io mirror secrets-gated and verified. MCP Registry re-publish = bump version+identifier in `integrations/mcp-registry/server.json`, then `mcp-publisher publish integrations/mcp-registry/server.json`. Two bite-points: registry `description` <= 100 chars; OCI label `io.modelcontextprotocol.server.name` must equal the json `name` (baked into the Dockerfile).

## 0. One-line installer (no per-release work)

`scripts/install.sh` (Linux/macOS) and `scripts/install.ps1` (Windows) consume
the Release assets directly: they detect the host platform, download the
matching archive, verify it against `SHA256SUMS`, and install to
`~/.memvault/bin` (or `%LOCALAPPDATA%\memvault\bin`). They always pull
`latest`, so nothing to do per tag.

## 1. crates.io

`cargo publish` is **not** automatic. Two options after the GitHub Release:

- Run the manual **Publish (manual)** workflow — job `crates-io` requires the
  `CRATES_IO_TOKEN` secret and publishes in dependency order.
- Or publish locally:

```bash
for crate in memvault-core memvault-cli memvault-mcp memvault-proxy; do
  cargo publish -p "$crate" --allow-dirty
done
```

`memvault-core` must land first. The other crates already declare
`memvault-core = { path = "...", version = "0.3.0" }`, so publishing replaces
the path dependency with the crates.io release automatically.

## 2. Obsidian plugin releases (dreamor/memvault-obsidian)

The plugin has a dedicated repo of record,
[`dreamor/memvault-obsidian`](https://github.com/dreamor/memvault-obsidian) — BRAT and
the community directory point there, and its release workflow builds the plugin and
attaches provenance attestations. Code lives in THIS repo; syncing and releasing are
automatic via `.github/workflows/sync-obsidian-plugin.yml`:

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
- **One-time community list submission**: a manual PR to
  [`obsidianmd/obsidian-releases`](https://github.com/obsidianmd/obsidian-releases)
  pointing at `dreamor/memvault-obsidian` — manual, reviewed, budget for review lag.

The workflow needs the `OBSIDIAN_PLUGIN_SYNC_TOKEN` secret in this repo: a fine-grained
PAT scoped to `dreamor/memvault-obsidian` only, with **Contents: read and write**. A PAT
is mandatory — pushes made with Actions' default `GITHUB_TOKEN` do not trigger workflows
in the target repo, so the tag would never fire a release.

## 3. Homebrew tap

Homebrew needs a dedicated tap repository — this project uses `dreamor/homebrew-tap`
(already created and pushed).
After the tag is published, generate the formula from the release assets:

```bash
./scripts/update-homebrew-formula.sh v0.3.0 > ../homebrew-tap/Formula/memvault.rb
cd ../homebrew-tap && git add . && git commit -m "memvault 0.3.0" && git push
```

Users then install with `brew install memvault`. Linux users install via
`scripts/install.sh` or `cargo install` instead.

## 4. npm (dsh plugin)

Run the manual **Publish (manual)** workflow (job `npm-dsh`, requires
`NPM_TOKEN`), or:

```bash
cd dsh-plugin
npm ci && npm run build && npm test
npm publish --access public   # publishes @dreamor/dsh-memvault
```

## 5. Optional channels (do after release is stable)

- **Docker Hub**: add a second `docker/login-action` +
  `docker/build-push-action` pair in `release.yml` to mirror
  `ghcr.io/dreamor/memvault` to `docker.io/dreamor/memvault`.
- **MCP ecosystem registries**: submit the MCP server to the official MCP
  registry, smithery.ai, mcp.so, Glama and PulseMCP so MCP-capable agents can
  discover it. See `docs/DISTRIBUTION.md`.

## 6. Web Dashboard artifact

The `dashboard-web` CI job runs `npm ci && npm run build` in `dashboard/` and
tars the resulting `dist/` into `memvault-dashboard-<tag>.tar.gz`, attached to
the GitHub Release. There is no desktop app, so **no macOS signing/notarization
or per-platform Windows/Linux packaging is needed** — the archive is served by
`memvault-mcp --serve-web <dist-dir>` on any OS.
