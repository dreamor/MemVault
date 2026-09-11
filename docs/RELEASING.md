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
- The Obsidian plugin packaged as a `.zip` plus the individual
  `main.js` / `manifest.json` / `styles.css` needed by BRAT / community install

Everything above is fully automated. The steps below are **not**, and must be
done by hand after the GitHub Release is published.

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

## 2. Obsidian community plugin submission

Obsidian plugins are distributed either via:

- **BRAT** (beta channel): users add the GitHub repo URL directly, no submission needed. This works off the tagged release's `manifest.json` + `main.js` + `styles.css` attached as **individual assets** (not zipped — BRAT and the community-plugins installer fetch each file by exact name). The `obsidian-package` CI job uploads both a convenience `.zip` and the three files unzipped; only the unzipped ones are functional for BRAT/community install.
- **Official community plugin list**: requires a one-time PR to
  [`obsidianmd/obsidian-releases`](https://github.com/obsidianmd/obsidian-releases)
  adding an entry to `community-plugins.json`. Subsequent version bumps also
  need a PR (or, once approved, some maintainers automate this with the
  `obsidian-releases` bot — not something to build ad hoc here).
- This is a manual, reviewed process — budget for review lag on first submission.

## 3. Homebrew tap

Homebrew needs a dedicated tap repository — this project uses `dreamor/homebrew-tap`
(already created and pushed, see `docs/DISTRIBUTION-TODO.md`).
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
npm publish --access public   # publishes @memvault/dsh-memvault
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
