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
