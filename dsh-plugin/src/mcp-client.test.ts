import { describe, expect, it, vi, beforeEach } from 'vitest'

const h = vi.hoisted(() => ({
  state: {
    connects: [] as unknown[],
    calls: [] as { name: string; arguments: Record<string, unknown> }[],
    reads: [] as unknown[],
    closes: 0,
    failConnectTimes: 0,
    callResult: undefined as unknown,
    readResult: { contents: [] as { text?: string }[] },
  },
  cleanups: [] as (() => Promise<void> | void)[],
}))

vi.mock('@modelcontextprotocol/sdk/client/index.js', () => ({
  Client: class {
    async connect(transport: unknown) {
      h.state.connects.push(transport)
      if (h.state.failConnectTimes > 0) {
        h.state.failConnectTimes--
        throw new Error('connect failed')
      }
    }
    async callTool(params: unknown) {
      h.state.calls.push(params as never)
      return h.state.callResult
    }
    async readResource(params: unknown) {
      h.state.reads.push(params)
      return h.state.readResult
    }
    async close() {
      h.state.closes++
    }
  },
}))

vi.mock('@modelcontextprotocol/sdk/client/streamableHttp.js', () => ({
  StreamableHTTPClientTransport: class {
    constructor(_url: URL) {}
  },
}))

import { createMcpClient } from './mcp-client.js'

function fakeCtx() {
  return { effect: (fn: () => unknown) => { h.cleanups.push(fn as never); }, logger: { error: vi.fn() } } as never
}

const URL_HINT = new URL('http://127.0.0.1:3778/mcp')

beforeEach(() => {
  h.state.connects = []
  h.state.calls = []
  h.state.reads = []
  h.state.closes = 0
  h.state.failConnectTimes = 0
  h.state.callResult = { content: [] }
  h.state.readResult = { contents: [] }
  h.cleanups = []
})

describe('createMcpClient — session lifecycle', () => {
  it('connects lazily and parses the inject_session_id prefix', async () => {
    const client = createMcpClient(fakeCtx(), URL_HINT.toString())
    h.state.callResult = {
      content: [{ type: 'text', text: '[inject_session_id: inj_abc123def]\nMUST: rule' }],
    }
    const result = await client.sessionStart('agent-1', 'context', 'proj')
    expect(h.state.connects).toHaveLength(1)
    expect(h.state.calls[0].name).toBe('session_start')
    expect(h.state.calls[0].arguments).toEqual({
      agent_id: 'agent-1',
      context_hint: 'context',
      project: 'proj',
    })
    expect(result.injectSessionId).toBe('inj_abc123def')
    expect(result.text).toContain('MUST: rule')
  })

  it('returns an empty inject session id when parsing fails', async () => {
    const client = createMcpClient(fakeCtx(), URL_HINT.toString())
    h.state.callResult = { content: [{ type: 'text', text: 'no prefix here' }] }
    const result = await client.sessionStart('a')
    expect(result.injectSessionId).toBe('')
  })

  it('retries after a failed connection (does not cache failures)', async () => {
    const client = createMcpClient(fakeCtx(), URL_HINT.toString())
    h.state.failConnectTimes = 1
    h.state.callResult = { content: [{ type: 'text', text: '[inject_session_id: inj_abc12]\nok' }] }
    await expect(client.sessionStart('a')).rejects.toThrow('connect failed')
    // Same client, next call must reconnect and succeed.
    const result = await client.sessionStart('a')
    expect(result.injectSessionId).toBe('inj_abc12')
    expect(h.state.connects).toHaveLength(2)
  })
})

describe('createMcpClient — notify & resources', () => {
  it('notifyResponse sends user_text only when provided', async () => {
    const client = createMcpClient(fakeCtx(), URL_HINT.toString())
    h.state.callResult = { content: [{ type: 'text', text: 'done' }] }
    await client.notifyResponse('resp', 'agent-1', 'user text')
    expect(h.state.calls[0]).toEqual({
      name: 'notify_response',
      arguments: { response_text: 'resp', agent_id: 'agent-1', user_text: 'user text' },
    })

    await client.notifyResponse('resp2', 'agent-1')
    expect(h.state.calls[1].arguments).toEqual({ response_text: 'resp2', agent_id: 'agent-1' })
  })

  it('readSessionInject joins resource text contents', async () => {
    const client = createMcpClient(fakeCtx(), URL_HINT.toString())
    h.state.readResult = { contents: [{ text: 'MUST: a' }, { text: 'REF: b' }] }
    expect(await client.readSessionInject()).toBe('MUST: a\nREF: b')
    expect(h.state.reads[0]).toEqual({ uri: 'memory://session-inject' })
  })
})

describe('createMcpClient — connection cleanup', () => {
  it('disposes the client on effect cleanup', async () => {
    const ctx = fakeCtx()
    const client = createMcpClient(ctx, URL_HINT.toString())
    await client.readSessionInject() // connect once
    const cleanup = (h.cleanups[0] as () => Promise<void>)()
    await cleanup()
    expect(h.state.closes).toBe(1)
  })
})
