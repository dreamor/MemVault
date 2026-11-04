import { spawn, type ChildProcess } from 'node:child_process'
import type { Context } from '@deepseek-ai/cordis'
import type { Config } from './config.js'

/**
 * Poll the readiness endpoint until the spawned `memvault-proxy` is up. Named
 * "ready" rather than "port" because it probes a full HTTP liveness route —
 * `memvault-proxy`'s `/health` (crates/memvault-proxy/src/main.rs) returns 200
 * without touching the database or MCP session state, so polling is
 * side-effect free and cheaper than a fake MCP handshake on `/mcp`.
 */
/** Grace period between a successful health probe and declaring ready —
 * long enough for a bind-failure exit to surface (see waitForReady). */
const READY_SETTLE_MS = 250

async function waitForReady(
  port: number,
  timeoutMs: number,
  failed?: () => Error | undefined,
): Promise<void> {
  const deadline = Date.now() + timeoutMs
  let delay = 100
  let lastError: unknown
  while (Date.now() < deadline) {
    // Fail fast when the child already died (spawn error / non-zero exit):
    // a healthy `/health` on the port at that point belongs to SOMEONE ELSE
    // (e.g. another instance already owns it) — declaring ready would attach
    // the plugin to a service this process does not control (C8).
    const failure = failed?.()
    if (failure) throw failure
    let ready = false
    try {
      const response = await fetch(`http://127.0.0.1:${port}/health`)
      ready = response.ok
      if (!ready) lastError = new Error(`health probe returned HTTP ${response.status}`)
    } catch (error) {
      lastError = error
    }
    if (ready) {
      // The child may still be dying from a bind failure (the port was
      // already owned by another process — the healthy response then came
      // from THAT process, not ours, and the exit event is in flight).
      // Brief settle window, then re-check before declaring ready (C8).
      await new Promise(resolve => setTimeout(resolve, READY_SETTLE_MS))
      const lateFailure = failed?.()
      if (lateFailure) throw lateFailure
      return
    }
    await new Promise(resolve => setTimeout(resolve, delay))
    delay = Math.min(delay * 2, 2000)
  }
  throw new Error(`memvault-proxy did not become ready on port ${port} within ${timeoutMs}ms`, { cause: lastError })
}

export interface SpawnedProxy {
  /** The MCP endpoint the caller should connect to once `ready` resolves. */
  url: string
  /** Resolves once the port is accepting connections; rejects on startup failure. */
  ready: Promise<void>
}

/**
 * Spawn and own a `memvault-proxy` child process for the plugin's lifetime.
 * Registers a `ctx.effect()` that kills the process on plugin unload/HMR.
 */
export function startProxy(ctx: Context, config: Config, timeoutMs = 10_000): SpawnedProxy {
  const port = config.port
  const binaryPath = config.binaryPath || 'memvault-proxy'
  let child: ChildProcess | undefined

  const ready = (async () => {
    // Pass runtime overrides on the command line instead of rewriting
    // ~/.memvault/proxy.yaml: that file may hold the user's upstream topology
    // and markdown_dir, which a generated file would silently erase.
    child = spawn(
      binaryPath,
      ['--transport', 'sse', '--port', String(port), '--db', config.db],
      {
        stdio: 'inherit',
        // Explicit, not merely additive: this must WIN over an inherited
        // OPENAI_API_KEY from dsh's own process env (dsh's own model
        // credentials are not MemVault's embedding credentials).
        env: { ...process.env, MEMVAULT_EMBEDDING_PROVIDER: config.embeddingProvider },
      },
    )
    // C8: a spawn failure (missing binary, EACCES) is delivered as an
    // 'error' event — WITHOUT a listener Node rethrows it as an uncaught
    // exception and takes the whole dsh host down (exit 1). Capture it,
    // log it, and fail `ready` instead.
    let spawnFailure: Error | undefined
    child.on('error', (error: Error) => {
      spawnFailure = new Error(`memvault: failed to spawn ${binaryPath}: ${error.message}`, { cause: error })
      ctx.logger.error(spawnFailure)
    })
    child.on('exit', (code, signal) => {
      if (code !== 0 && code !== null) {
        const message = `memvault-proxy exited with code ${code} (signal ${signal ?? 'none'})`
        ctx.logger.error(new Error(message))
        // An exit before readiness means the port probe is measuring
        // someone else's service — see waitForReady's `failed` guard (C8).
        spawnFailure ??= new Error(`memvault: ${message}`)
      }
    })
    await waitForReady(port, timeoutMs, () => spawnFailure)
  })()

  // A startup failure must never crash the host as an unhandled rejection:
  // consumers see the same rejection through `ready`, but when the feature
  // that would consume it is disabled (no injection refresh scheduled),
  // Node's default unhandled-rejection behavior would kill dsh. Mark the
  // rejection handled here; every real consumer still gets it.
  ready.catch(() => {})

  ctx.effect(() => {
    return async () => {
      await ready.catch(() => {})
      child?.kill()
    }
  }, 'memvault.process-manager')

  return { url: `http://127.0.0.1:${port}/mcp`, ready }
}
