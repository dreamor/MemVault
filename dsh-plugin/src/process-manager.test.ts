import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest'
import type { Config } from './config.js'

const h = vi.hoisted(() => ({
  spawned: [] as {
    binary: string
    args: string[]
    options: Record<string, unknown>
    handlers: Record<string, (arg?: unknown) => void>
    killed: number
  }[],
  writes: [] as { file: string; data: string }[],
  mkdirs: [] as string[],
  cleanups: [] as (() => unknown)[],
}))

vi.mock('node:child_process', () => ({
  spawn: (binary: string, args: string[], options: Record<string, unknown>) => {
    const entry = {
      binary,
      args,
      options,
      handlers: {} as Record<string, (arg?: unknown) => void>,
      killed: 0,
    }
    h.spawned.push(entry)
    return {
      on: (event: string, handler: (arg?: unknown) => void) => { entry.handlers[event] = handler },
      kill: () => { entry.killed++ },
    }
  },
}))

vi.mock('node:fs/promises', () => ({
  mkdir: async (dir: string) => { h.mkdirs.push(dir) },
  writeFile: async (file: string, data: string) => { h.writes.push({ file, data }) },
}))

import { startProxy } from './process-manager.js'

const config = {
  mode: 'spawn',
  db: '/tmp/memvault-test.db',
  port: 3999,
  url: '',
  binaryPath: '/usr/local/bin/memvault-proxy',
  embeddingProvider: 'native',
  agentId: 'dsh',
  injectOnAssemble: true,
  extractOnTurnEnd: true,
} as unknown as Config

function fakeCtx() {
  return {
    effect: (fn: () => unknown) => { h.cleanups.push(fn) },
    logger: { error: vi.fn() },
  } as never
}

beforeEach(() => {
  h.spawned = []
  h.writes = []
  h.mkdirs = []
  h.cleanups = []
})

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('startProxy', () => {
  it('writes proxy.yaml, spawns the binary and resolves ready on /health OK', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => ({ ok: true })))
    const proxy = startProxy(fakeCtx(), config, 1000)
    await expect(proxy.ready).resolves.toBeUndefined()

    expect(proxy.url).toBe('http://127.0.0.1:3999/mcp')
    expect(h.mkdirs[0]).toContain('memvault')
    expect(h.writes[0].file).toContain('proxy.yaml')
    expect(h.writes[0].data).toContain('port: 3999')
    expect(h.writes[0].data).toContain('db: /tmp/memvault-test.db')
    expect(h.writes[0].data).toContain('transport: sse')

    expect(h.spawned).toHaveLength(1)
    expect(h.spawned[0].binary).toBe('/usr/local/bin/memvault-proxy')
    expect(h.spawned[0].args).toEqual(['--port', '3999'])
    expect(h.spawned[0].options.env).toMatchObject({ MEMVAULT_EMBEDDING_PROVIDER: 'native' })
  })

  it('logs an error when the child exits with a non-zero code', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => ({ ok: true })))
    const ctx = fakeCtx()
    startProxy(ctx, config, 1000)
    await vi.waitFor(() => expect(h.spawned).toHaveLength(1))
    h.spawned[0].handlers.exit?.(1, 'SIGTERM')
    expect(ctx.logger.error).toHaveBeenCalledTimes(1)
    expect(ctx.logger.error.mock.calls[0][0]).toMatchObject({
      message: expect.stringContaining('exited with code 1'),
    })
  })

  it('does not log when the child exits cleanly (code 0)', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => ({ ok: true })))
    const ctx = fakeCtx()
    startProxy(ctx, config, 1000)
    await vi.waitFor(() => expect(h.spawned).toHaveLength(1))
    h.spawned[0].handlers.exit?.(0, null)
    expect(ctx.logger.error).not.toHaveBeenCalled()
  })

  it('rejects ready when the proxy never becomes healthy within the timeout', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => ({ ok: false })))
    const proxy = startProxy(fakeCtx(), config, 20)
    await expect(proxy.ready).rejects.toThrow('did not become ready')
  })

  it('kills the child process on effect cleanup', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => ({ ok: true })))
    const proxy = startProxy(fakeCtx(), config, 1000)
    await proxy.ready
    const cleanup = (h.cleanups[0] as () => () => Promise<void>)()
    await cleanup()
    expect(h.spawned[0].killed).toBe(1)
  })
})
