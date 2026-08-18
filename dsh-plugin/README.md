# @memvault/dsh-plugin

Cordis plugin bridging [MemVault](../README.md) into [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) (`dsh`).

Design rationale and the real `dsh`/Cordis API surface it relies on (verified
against the actual `deepseek-harness` source, not secondhand docs) are
documented in [`../docs/DSH-BRIDGE-DESIGN.md`](../docs/DSH-BRIDGE-DESIGN.md).

## What it does

- **Auto-injection**: keeps `memory://session-inject` cached and registers it
  as a system prompt section (`ctx.systemPrompt.section()`), refreshed
  eagerly at load and again on every `turn/end`. No agent tool call required.
- **Auto-extraction**: on `turn/end` with `reason.kind === 'completed'`, calls
  MemVault's `notify_response` with **both** that turn's assistant text and
  the user's own turn text (`user/message` events with `source.kind === 'user'`)
  automatically. Passing the user's own words matters — MemVault's extractor
  signal words are first-person ("我偏好"/"我喜欢"), which match a user's own
  statement far more reliably than an assistant's restatement of it.

If you only want the raw 13 MemVault tools exposed to the model (no
auto-injection/auto-extraction), you don't need this package — see
`docs/DSH-BRIDGE-DESIGN.md` §5 for the zero-code `@deepseek-ai/dsh-mcp-client`
recipe instead.

## Install

Prerequisites: a working `dsh` install (e.g. `npx @deepseek-ai/dsh web`) and the
`memvault-proxy` binary reachable from `PATH` (or point `binaryPath` at one —
see the override section below).

Build, then install into a dsh profile from the repo's `dsh-plugin/` directory:

```bash
cd dsh-plugin
npm install        # peer deps on 0.0.1-rc.1 packages need npm i --legacy-peer-deps
npm run build      # tsc --strict
npx @deepseek-ai/dsh plugin --profile <profile> add "$PWD"
```

`dsh plugin add` runs the equivalent of `pnpm add` and appends the package to
that profile's `dsh.profile.bundles` automatically — no manual edit of the
profile's `cordis.patch.yml` is needed for a default install. The plugin ships a
`cordis.patch.yml` that registers one `memvault` entry with the default config
(`mode: spawn`, `embeddingProvider: native`, …).

Verify the composed config, then start:

```bash
npx @deepseek-ai/dsh --profile <profile> --dump-config   # 组合后应只有一条 id: memvault 且字段正确
npx @deepseek-ai/dsh <profile> --port 0                  # 实际启动方式以你的 dsh 用法为准
```

At startup the plugin spawns `memvault-proxy` on port 3778 and probes
`http://127.0.0.1:3778/health` until the server is ready. See
`docs/DSH-BRIDGE-DESIGN.md` §7.3 for how to confirm the injection actually
lands in the assembled system prompt.

### Overriding config (local development)

To override a single field (e.g. point `binaryPath` at a locally built
`memvault-proxy`), add a **bare-id override patch** to the profile's
`cordis.patch.yml` and rewrite the whole `config` block — an override replaces
the entire `config`, it does not merge per-field:

```yaml
- id: memvault
  name: '@memvault/dsh-plugin'
  config:
    mode: spawn
    db: '~/.memvault/data.db'
    port: 3778
    binaryPath: /absolute/path/to/memvault-proxy
    embeddingProvider: native
    agentId: dsh
    injectOnAssemble: true
    extractOnTurnEnd: true
```

The `cordis.patch.yml` this plugin ships wraps its entry in `insert:` because
the entry doesn't exist in any earlier layer. A bare `- id:` patch is an
*override* that requires the id to already exist and errors with
`patch: entry "memvault" not found` otherwise — see
`docs/DSH-BRIDGE-DESIGN.md` §7.1 for the full write-up and the other three real
bugs found during the end-to-end verification.

## Config

| Field | Default | Meaning |
|---|---|---|
| `mode` | `spawn` | `spawn` manages a `memvault-proxy` child process; `attach` connects to `url`. |
| `db` | `~/.memvault/data.db` | Used when `mode: spawn`. |
| `port` | `3778` | Used when `mode: spawn`. |
| `url` | `''` | Used when `mode: attach`, e.g. `http://127.0.0.1:3778/mcp`. |
| `binaryPath` | `''` (resolved from `PATH`) | Used when `mode: spawn`. |
| `embeddingProvider` | `native` | Used when `mode: spawn` — forced as `MEMVAULT_EMBEDDING_PROVIDER` in the child's env, so it can't accidentally inherit dsh's own `OPENAI_API_KEY`. |
| `agentId` | `dsh` | Reported to MemVault on every call. |
| `injectOnAssemble` | `true` | Toggle auto-injection. |
| `extractOnTurnEnd` | `true` | Toggle auto-extraction. |

## Build

```bash
npm install
npm run build
npm test
```

## Status

Builds clean (`tsc --strict`) against the real, npm-published
`@deepseek-ai/cordis@4.0.1`, `@deepseek-ai/dsh-system-prompt`,
`@deepseek-ai/dsh-session`, and `@deepseek-ai/dsh-llm` packages (not mocks —
see `src/index.smoke.test.ts`, which mounts the plugin on a real `Context`
alongside the real `SystemPrompt` service and asserts `ctx.systemPrompt.assemble()`
actually contains the injected section).

**Fully verified end to end against a real `dsh` install** (`npx
@deepseek-ai/dsh web`, v0.1.0-rc.6). Four real bugs were found and fixed in
the process — see `docs/DSH-BRIDGE-DESIGN.md` §7.1–§7.3 for the full
postmortems:

1. `cordis.patch.yml` insert vs. override patch semantics (a bare `id` patch
   requires the id to already exist).
2. `memvault-proxy` inheriting a stray `OPENAI_API_KEY` from dsh's own
   process env, plus a stale (pre-`native`-embedding) local binary.
3. The eager injection-cache refresh at `apply()` time racing
   `memvault-proxy`'s own startup (`fetch failed`).
4. `mcp-client.ts` permanently caching a *failed* connection attempt —
   `connected ??= (...)` memoized on the first (raced, failing) call, so
   every later call kept re-rejecting forever, never retrying even after the
   proxy came up.

With all four fixed, a real conversation turn's session log
(`~/.dsh/sessions/.../session.jsonl.zstd`, decompressed with `zstd -d`) shows
the actual `request/header.data.header.system` text containing MemVault's
injected `[MUST]`/`[REF]` memories, and a direct `notify_response` call
against the running `memvault-proxy` moved MemVault's memory count from 8 to
10 (new preference/fact memories actually saved to Inbox).

A follow-up round extended `notify_response` to also extract from the user's
own turn text (see `docs/DSH-BRIDGE-DESIGN.md` §7.4) — MemVault's extractor
signal words are first-person, so a user's own statement ("我偏好用 tabs")
matches far more reliably than an assistant's restatement of it ("你偏好用
tabs" used to be silently rejected; the signal-word list was also extended
with second/third-person mirrors). Verified against the real running
`memvault-proxy`: memory count went from 10 to 13 across three scenarios,
with `source:user`/`source:assistant` tags on the results.

Note for anyone installing dsh-internal packages directly: several of them
(`@deepseek-ai/dsh-session`, `@deepseek-ai/dsh-system-prompt`) have peer deps
on other `0.0.1-rc.1` packages that aren't all published yet
(`@deepseek-ai/dsh-type-meta` 404s as of this writing) — `npm install
--legacy-peer-deps` gets you the ones that matter for this package.
