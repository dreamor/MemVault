# Releasing MemVault

Pushing a `v*` tag triggers `.github/workflows/release.yml`, which builds and
attaches to the GitHub Release:

- Rust binaries (`memvault-cli`, `memvault-mcp`, `memvault-proxy`) for Linux + macOS (x86_64/arm64)
- A Docker image, pushed to `ghcr.io/<repo>:<tag>` and `:latest`
- The Web Dashboard as a `dist/` archive (`memvault-dashboard-<tag>.tar.gz`), served by `memvault-mcp --serve-web`
- The VS Code extension packaged as a `.vsix`
- The Obsidian plugin packaged as a `.zip`

Everything above is fully automated. The steps below are **not**, and must be
done by hand after the GitHub Release is published.

## 1. VS Code Marketplace publish

The CI job only runs `vsce package` — it deliberately does **not** run `vsce
publish`, since that requires a Marketplace Personal Access Token and is a
decision each release should make deliberately (not every tag needs a
Marketplace release; a GitHub `.vsix` may be enough for a beta).

To publish:

```bash
cd vscode-extension
npx vsce publish --pat <marketplace-PAT>
# or, to publish the exact .vsix already built by CI:
npx vsce publish --packagePath /path/to/memvault-<tag>.vsix
```

The PAT needs the Marketplace "Manage" scope on the `memvault` publisher
(Azure DevOps organization). See <https://code.visualstudio.com/api/working-with-extensions/publishing-extension>.

## 2. Obsidian community plugin submission

Obsidian has no equivalent of `vsce publish` — plugins are distributed either via:

- **BRAT** (beta channel): users add the GitHub repo URL directly, no submission needed. This works off the tagged release's `manifest.json` + `main.js` + `styles.css` attached as **individual assets** (not zipped — BRAT and the community-plugins installer fetch each file by exact name). The `obsidian-package` CI job uploads both a convenience `.zip` and the three files unzipped; only the unzipped ones are functional for BRAT/community install.
- **Official community plugin list**: requires a one-time PR to
  [`obsidianmd/obsidian-releases`](https://github.com/obsidianmd/obsidian-releases)
  adding an entry to `community-plugins.json`. Subsequent version bumps also
  need a PR (or, once approved, some maintainers automate this with the
  `obsidian-releases` bot — not something to build ad hoc here).
- This is a manual, reviewed process — budget for review lag on first submission.

## 3. Web Dashboard artifact

The `dashboard-web` CI job runs `npm ci && npm run build` in `dashboard/` and
tars the resulting `dist/` into `memvault-dashboard-<tag>.tar.gz`, attached to
the GitHub Release. There is no desktop app, so **no macOS signing/notarization
or per-platform Windows/Linux packaging is needed** — the archive is served by
`memvault-mcp --serve-web <dist-dir>` on any OS.
