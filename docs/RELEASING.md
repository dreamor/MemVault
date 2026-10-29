# Releasing MemVault

Current state: **v0.6.8 is shipped**. Since 0.6.2, the Codex plugin ships only `plugins/memvault/.claude-plugin/plugin.json`; `plugins/memvault/plugin.json` is deliberately absent. crates.io (4 crates), npm
(`@dreamor/dsh-memvault`), Homebrew (`dreamor/tap`), ghcr.io + Docker Hub, the
official MCP Registry (`io.github.dreamor/memvault`) and the Obsidian community
directory are all live, and the repo is public. crates.io and npm use OIDC
trusted publishing (no token secrets); the Docker Hub mirror and the MCP
Registry listing are configured and verified. Everything below reflects that
reality.

## Standard release sequence

1. Start from a clean, synced `master` whose CI is green.
2. Bump every file listed by `scripts/check-release-versions.py` (plus
   `Cargo.lock`, the README/install version tables and `CHANGELOG.md`), and
   point `integrations/mcp-registry/server.json` at
   `ghcr.io/dreamor/memvault:v<version>`.
3. Run `python3 scripts/check-release-versions.py v<version>`, commit
   `chore(release): v<version>`, and push that commit to `master`.
4. Create and push the annotated tag: `git tag -a v<version> -m "MemVault
   v<version>" && git push origin v<version>`. CI then runs `release.yml`
   (GitHub Release, binaries, dashboard, GHCR/Docker Hub, crates.io, dsh npm,
   MCP Registry) and `release-npm.yml` (memvault-bin npm, Homebrew tap), and
   each workflow verifies the channels it owns.
5. After both workflows are green, run
   `GH_TOKEN=$(gh auth token) python3 scripts/verify-release-channels.py v<version>`
   and update the current-state sentence at the top of this file to
   `v<version> is shipped`.
6. The Obsidian plugin releases separately from
   `obsidian-plugin/manifest.json` and `dreamor/memvault-obsidian`; do not bump
   it for a core tag.

## 0. Version bump (before you tag)

Every version-bearing file is listed in one place,
[`scripts/check-release-versions.py`](../scripts/check-release-versions.py).
`release.yml` runs it as the `verify-version` job, which fails a tag push
*before* anything is built, published or pushed, and CI runs the same script on
every build — so a half-bumped tree can no longer ship. For a release, update
these in the same commit:

- `Cargo.toml` (`[workspace.package] version`), the `memvault-core` version
  requirement in each of `crates/memvault-cli|memvault-mcp|memvault-proxy/Cargo.toml`,
  and the regenerated `Cargo.lock`.
- `plugin.json`, `plugins/memvault/.claude-plugin/plugin.json`,
  `.qoder-plugin/plugin.json`, `gemini-extension.json`,
  `pi-extension/package.json`.
- The plugin bundle ships only
  `plugins/memvault/.claude-plugin/plugin.json`. Do **not** add
  `plugins/memvault/plugin.json`: even without an explicit `hooks` field, its
  presence made Codex `0.163.0-alpha.11.1` stop enumerating
  `hooks/hooks.json` (see `docs/TROUBLESHOOTING.md` §7.4 and
  `integrations/codex/README.md`).
- `dsh-plugin/package.json` + its `package-lock.json`, and
  `dashboard/package.json` (it drifted at 0.4.0 through the 0.4.2/0.4.3 cycle).
- `integrations/hermes/plugin.yaml`, and `integrations/mcp-registry/server.json`
  — both its `version` **and** the `ghcr.io/dreamor/memvault:v<version>` image
  tag it advertises (so publish the listing *after* the release; see §5).
- The version tables and `--version` samples in `README.md`, `README.zh-CN.md`
  and `docs/INSTALL.md`.
- `CHANGELOG.md`: rename `[Unreleased]` to `## [<version>] - <date>` and leave a
  fresh empty `[Unreleased]` above it.

Check it locally with `python3 scripts/check-release-versions.py v<version>`
(no argument = "every manifest agrees with `Cargo.toml`"), then push the
`v<version>` tag. The Obsidian plugin version (`obsidian-plugin/manifest.json`)
is deliberately *not* part of this — it releases on its own schedule, see §2.

**Tag shape matters.** Git tags are `v<major>.<minor>.<patch>`, and Docker image
tags are always that same `v<version>` string (never bare `0.4.x`, never a
branch name). A version that escapes without a tag is not recoverable: `0.4.1`
(2026-10-14) reached npm (dsh plugin — npm is append-only) and left one stray
`ghcr.io/dreamor/memvault:v0.4.1` tag plus its Docker Hub mirror, but was never
tagged, released, published to crates.io or written into Homebrew. Its content
shipped in 0.4.2, so don't pin `:v0.4.1`. The gate above exists so this cannot
happen twice.

Removing a tag that already escaped is a manual, one-tag job: run the
**Delete a stray image tag** workflow (`workflow_dispatch`, dry run by
default). It uses the workflow's own `GITHUB_TOKEN` for ghcr and the same
`DOCKERHUB_*` secrets the mirror push uses — so the Docker Hub half needs the
token to be a Personal Access Token with **Read, Write and Delete** scope
(a read/write token authenticates but gets HTTP 403 on delete). A bare tag a
listing still references is easier to repoint than to remove: the **Publish MCP
Registry listing** workflow takes a `repoint_version` input that rewrites the
image tag to `v<version>`, and a `retire_version` input that marks the listing
`deleted` when no replacement tag exists.

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
  to `docker.io/dreamor/memvault:<tag>` / `:latest` (the mirror needs
  `DOCKERHUB_USERNAME` / `DOCKERHUB_TOKEN` and **fails** the run if they are
  missing — opt out with `publish-dockerhub = false`).
- The Web Dashboard as a `dist/` archive (`memvault-dashboard-<tag>.tar.gz`), served by `memvault-mcp --serve-web`
- The Obsidian plugin is **not** published here — plugin releases live in the
  dedicated [`dreamor/memvault-obsidian`](https://github.com/dreamor/memvault-obsidian)
  repo; see §2.

The same run then publishes the package channels, once
`github-release` succeeds: **crates.io** (4 crates, OIDC), **npm**
(`@dreamor/dsh-memvault`, OIDC, versioned from the tag) and the **MCP Registry
listing** (OIDC). A `workflow_dispatch` run with `publish-only = true` re-runs
just those, which is the recovery path when a tag run dies after the GitHub
Release is out; its `publish-crates` / `publish-npm` / `publish-mcp` /
`publish-dockerhub` inputs default to on and only apply to dispatched runs (a
tag push always publishes).

A second workflow, [`.github/workflows/release-npm.yml`](../.github/workflows/release-npm.yml),
runs on the same `v*` tag push and owns the Homebrew side: it builds the Apple
Silicon binaries, publishes them as the npm package `@dreamor/memvault-bin`
(OIDC trusted publishing), waits for the npm China mirror to serve the tarball,
and only then repoints `dreamor/homebrew-tap` at it. It is the *only* writer of
the tap — `release.yml` deliberately has no `update-homebrew` job, so the two
workflows cannot race on `Formula/memvault.rb`. Repairs go through its own
dispatch inputs (`version`, `publish-homebrew`); the npm publish is idempotent,
so re-running it for an already-published version just fixes the tap.

Dispatch a repair with `--ref` pointed at the branch that owns the workflow
(`main`/`master`), **not** at the release tag: only `publish-npm` checks out the
tagged commit (the binaries have to be built from the released source), while
`update-homebrew` and `verify` take their scripts from the ref the run was
started on. A dispatch checked out on an old tag once used that tag's pre-npm
`update-homebrew-formula.sh` and pushed a formula pointing back at the GitHub
Release archive — the slow download the npm mirror exists to replace. Both jobs
now refuse to do that: `update-homebrew` fails if the formula it generated does
not pull `@dreamor/memvault-bin` from the mirror.

Docker Hub and Homebrew are required channels: if their credentials are missing
the run **fails** rather than skipping them, so a green release can never mean
"we quietly shipped one registry". Opt out on purpose with
`publish-dockerhub = false` (on `release.yml`) / `publish-homebrew = false`
(on `release-npm.yml`).

## Verification (automatic)

Nothing above proves a version is *live* — only that a push step exited 0. Two
checks close that gap, both driven by
[`scripts/verify-release-channels.py`](../scripts/verify-release-channels.py):

- **Per release** — `release.yml`'s `verify-release` job reads the GitHub
  Release, the crates.io API, the ghcr and Docker Hub registries and the MCP
  Registry listing back over the public network, and fails the run if any of
  them does not serve the tag. It also checks that the image tag the MCP listing
  advertises still resolves (a listing pointing at a deleted image is the
  `0.3.0` bug). The `npm-bin` and `homebrew` channels are verified instead by
  `release-npm.yml`'s own `verify` job, because that workflow is what publishes
  them (and `release.yml` would otherwise race its tap push); `release.yml`
  skips those two explicitly. Channels opted out of with
  `publish-dockerhub = false` are skipped.
- **Daily** — `.github/workflows/verify-release-drift.yml` runs the same script
  against the latest GitHub Release at 02:47 UTC and opens (or comments on) a
  `Release channel drift detected` issue when a channel stops serving it.

Both cover the release-locked channels only; the npm dsh plugin and the Obsidian
plugin release on their own schedule and are intentionally excluded. Run it
locally with:

```bash
GH_TOKEN=$(gh auth token) python3 scripts/verify-release-channels.py v0.5.0 --attempts 1
```

The sections below are the manual fallbacks for channels `release.yml` already
publishes (§1, §3, §4, §5) plus the work that genuinely needs a human (§2, the
Obsidian plugin's own repo). §0 is the pre-tag bump, §6 a dashboard-artifact
note, and §7 needs no per-release work at all.

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
  then appends the `versions.json` entry and pushes the `<version>` tag there — **bare,
  no `v` prefix** — which triggers its release. The tag must be *identical* to the
  manifest version: that's the gate the release workflow enforces, and it is also how
  Obsidian finds the installable release (`v0.3.4` vs `0.3.4` looks fine to CI but the
  store then keeps serving the previous version). Assets
  go out as individual `main.js` / `manifest.json` / `styles.css`, the exact shape BRAT
  and the community installer fetch by filename.
- **Community list**: already handled — the plugin is listed. Future version
  bumps need no list interaction.

The workflow needs the `OBSIDIAN_PLUGIN_SYNC_TOKEN` secret in this repo: a fine-grained
PAT scoped to `dreamor/memvault-obsidian` only, with **Contents: read and write**. A PAT
is mandatory — pushes made with Actions' default `GITHUB_TOKEN` do not trigger workflows
in the target repo, so the tag would never fire a release.

## 3. Homebrew tap

Homebrew uses the tap repository `dreamor/homebrew-tap` (already live). It is
published by every `v*` tag, from
[`.github/workflows/release-npm.yml`](../.github/workflows/release-npm.yml):
build the Apple Silicon binaries → publish `@dreamor/memvault-bin` to npm →
wait for `registry.npmmirror.com` to serve the tarball → write the formula →
push the tap. To regenerate by hand (the script polls the mirror and prints the
formula; pass a sha256 as the second argument to skip the download):

```bash
./scripts/update-homebrew-formula.sh v0.6.0 > ../homebrew-tap/Formula/memvault.rb
cd ../homebrew-tap && git add . && git commit -m "memvault 0.6.0" && git push
```

> `registry.npmmirror.com` mirrors npmjs **lazily**: a brand-new package is not
> pulled until something asks for its *metadata*, and the
> `/<pkg>/-/<file>.tgz` URL does not ask — it is a blind rewrite to their CDN
> that keeps returning 404 until the package has been mirrored. So every
> npmmirror wait in this repo polls the **version endpoint**
> (`/<pkg>/<version>`, which is what triggers the pull) while nudging
> npmmirror with a metadata request plus `PUT /-/package/<pkg>/syncs`. Never
> wait on the tarball URL — the first bootstrap did, saw ten minutes of 404s,
> and gave up on a perfectly good publish.

The formula's `url` is the npmmirror tarball, not the GitHub Release archive.
That is deliberate: the GitHub CDN is shaped to a few KB/s on some corporate
networks, so the 48 MB release tarball could take tens of minutes to download.
`registry.npmjs.org` is kept as a Homebrew `mirror` fallback, and the published
`sha256` is the mirror tarball's checksum.

#### One-time bootstrap (interactive)

npm only lets you configure a trusted publisher for a package that **already
exists**, and the first publish needs interactive 2FA — neither of which CI can
do. So the very first version of the package is published by hand, from a
terminal:

```bash
scripts/bootstrap-npm-bin-package.sh 0.6.0 --dry-run   # validate, publish nothing
scripts/bootstrap-npm-bin-package.sh 0.6.0             # publish + configure the trusted publisher
```

The script stages the binaries (from the Homebrew Cellar by default, so it does
not re-download 48 MB from the throttled GitHub CDN), publishes with
`--access public`, runs `npm trust github @dreamor/memvault-bin --repo
dreamor/MemVault --file release-npm.yml --allow-publish`, then waits for
`registry.npmmirror.com` and asserts it serves byte-identical bytes. It finishes
by printing the tap commands to run.

Two things to keep in mind:

- The trusted publisher is bound to the **workflow filename**
  (`release-npm.yml`), because npm matches the OIDC `workflow_ref` claim, which
  for a reusable workflow is the *caller*. Moving the publish job into
  `release.yml` would silently break publishing — keep it where it is.
- Re-create the trusted publisher if it is ever revoked, or if the npm account
  loses 2FA: `npm trust list @dreamor/memvault-bin` shows the current entry.

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
source. **Every `v*` tag run publishes it** (job `publish-mcp`, which calls the
same reusable workflow the manual path uses) once the ghcr image exists, so a
normal release needs no action here. To re-publish or repair by hand:

1. Bump `version` and the `packages[0].identifier` image tag in `server.json`.
   The identifier must name a tag that **exists in ghcr**: `release.yml` tags
   the image with the git tag, so it is `ghcr.io/dreamor/memvault:v0.4.4`.
   Bare `0.4.x` tags are long gone (`rebuild-docker.yml` normalizes the `v`
   prefix now), and the registry rejects an identifier it cannot pull — `400 ...
   OCI image 'ghcr.io/dreamor/memvault:0.4.4' does not exist`.
2. Ensure the Docker image for that tag exists in ghcr (it does after
   `release.yml`; for label-only metadata changes there is
   `rebuild-docker.yml`).
3. Run the manual **Publish MCP Registry listing** workflow. It authenticates
   with the workflow's GitHub OIDC token (`mcp-publisher login github-oidc`), so
   nothing is stored and nothing expires.

The same workflow repairs a listing that has gone stale:

- `repoint_version` — republish an older listing with its image tag rewritten to
  `v<version>`. Use it when the listing's tag was deleted but a tag for the same
  content still exists. It cannot revive an image built before the OCI label
  existed (the registry answers `400 ... missing required annotation`).
- `retire_version` — run `mcp-publisher status --status deleted` for that
  version. This is the only supported way to take down a listing whose image tag
  is gone for good (there is no DELETE endpoint); it retired the `0.3.0` listing
  after the bare `ghcr.io/dreamor/memvault:0.3.0` tag was removed.

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

`dashboard/package.json` tracks the release version (enforced by
`scripts/check-release-versions.py`; it used to sit at 0.4.0 while the archive
shipped as `memvault-dashboard-v0.4.3.tar.gz`).

The `dashboard-web` CI job still attaches `memvault-dashboard-<tag>.tar.gz`
to the GitHub Release as a standalone archive for installs that don't upgrade
the binary. There is no desktop app, so **no macOS signing/notarization or
per-platform Windows/Linux packaging is needed** — the archive is served by
`memvault-mcp --serve-web <dist-dir>` on any OS.

## 7. One-line installer (no per-release work)

`scripts/install.sh` (Linux/macOS) and `scripts/install.ps1` (Windows) consume
the Release assets directly: they detect the host platform, download the
matching archive, verify it against `SHA256SUMS`, and install to
`~/.memvault/bin` (or `%LOCALAPPDATA%\memvault\bin`). They always pull
`latest`, so nothing to do per tag.
