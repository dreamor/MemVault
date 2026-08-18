import { spawn, type ChildProcess } from 'node:child_process'
import { mkdir, writeFile } from 'node:fs/promises'
import { homedir } from 'node:os'
import { dirname, join } from 'node:path'
import type { Context } from '@deepseek-ai/cordis'
import type { Config } from './config.js'

const PROXY_CONFIG_PATH = join(homedir(), '.memvault', 'proxy.yaml')

/** Write `~/.memvault/proxy.yaml` so the spawned `memvault-proxy` uses SSE on the configured port/db. */
async function writeProxyConfig(db: string, port: number): Promise<void> {
  const yaml = [
    'proxy:',
    '  transport: sse',
    `  port: ${port}`,
    `  db: ${db}`,
    '  upstreams: []',
    '',
  ].join('\n')
  await mkdir(dirname(PROXY_CONFIG_PATH), { recursive: true })
  await writeFile(PROXY_CONFIG_PATH, yaml, 'utf8')
}

/**
 * Poll the readiness endpoint until the spawned `memvault-proxy` is up. Named
 * "ready" rather than "port" because it probes a full HTTP liveness route —
 * `memvault-proxy`'s `/health` (crates/memvault-proxy/src/main.rs) returns 200
 * without touching the database or MCP session state, so polling is
 * side-effect free and cheaper than a fake MCP handshake on `/mcp`.
 */
async function waitForReady(port: number, timeoutMs: number): Promise<void> {
  const deadline = Date.now() + timeoutMs
  let delay = 100
  let lastError: unknown
  while (Date.now() < deadline) {
    let ready = false
    try {
      const response = await fetch(`http://127.0.0.1:${port}/health`)
      ready = response.ok
      if (!ready) lastError = new Error(`health probe returned HTTP ${response.status}`)
    } catch (error) {
      lastError = error
    }
    if (ready) return
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
export function startProxy(ctx: Context, config: Config): SpawnedProxy {
  const port = config.port
  const binaryPath = config.binaryPath || 'memvault-proxy'
  let child: ChildProcess | undefined

  const ready = (async () => {
    await writeProxyConfig(config.db, port)
    child = spawn(binaryPath, [], {
      stdio: 'inherit',
      // Explicit, not merely additive: this must WIN over an inherited
      // OPENAI_API_KEY/OPENAI_API_BASE from dsh's own process env (dsh's own
      // model credentials are not MemVault's embedding credentials).
      env: { ...process.env, MEMVAULT_EMBEDDING_PROVIDER: config.embeddingProvider },
    })
    child.on('exit', (code, signal) => {
      if (code !== 0 && code !== null) {
        ctx.logger.error(new Error(`memvault-proxy exited with code ${code} (signal ${signal ?? 'none'})`))
      }
    })
    await waitForReady(port, 10_000)
  })()

  ctx.effect(() => {
    return async () => {
      await ready.catch(() => {})
      child?.kill()
    }
  }, 'memvault.process-manager')

  return { url: `http://127.0.0.1:${port}/mcp`, ready }
}
