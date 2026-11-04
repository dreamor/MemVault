import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { Config } from './config.js'

const h = vi.hoisted(() => ({
  calls: [] as { method: string; args: unknown[] }[],
}))

vi.mock('./mcp-client.js', () => ({
  createMcpClient: () => ({
    readSessionInject: async () => '',
    notifyResponse: async (...args: unknown[]) => {
      h.calls.push({ method: 'notifyResponse', args })
      return ''
    },
    inferOutcome: async (...args: unknown[]) => {
      h.calls.push({ method: 'inferOutcome', args })
      return ''
    },
  }),
}))

import { apply } from './index.js'

type Handler = (...args: never[]) => void

function fakeCtx() {
  const handlers = new Map<string, Handler>()
  return {
    handlers,
    on: (event: string, cb: Handler) => {
      handlers.set(event, cb)
    },
    logger: { error: vi.fn() },
    systemPrompt: { section: vi.fn() },
  }
}

function config(overrides: Partial<Config> = {}): Config {
  return {
    mode: 'attach',
    url: 'http://127.0.0.1:9/mcp',
    db: '',
    port: 0,
    binaryPath: '',
    embeddingProvider: 'none',
    agentId: 'dsh',
    injectOnAssemble: false,
    extractOnTurnEnd: false,
    inferOutcomeOnSessionEnd: false,
    ...overrides,
  } as Config
}

const userMessage = (text: string) => ({
  type: 'user/message',
  data: { source: { kind: 'user' }, content: [{ type: 'text', text }] },
})
const assistantMessage = (turn: number, text: string) => ({
  type: 'assistant/message',
  data: { turn, message: { content: [{ type: 'text', text }] } },
})
const turnStart = (turn: number) => ({ type: 'turn/start', data: { turn } })
const turnEnd = (turn: number) => ({
  type: 'turn/end',
  data: { turn, reason: { kind: 'completed' } },
})

const flush = () => new Promise(resolve => setTimeout(resolve, 0))

/**
 * C7 regressions: one dsh Context serves multiple concurrent sessions, so
 * per-turn bookkeeping and outcome evidence must be keyed by session. The
 * previous plugin-global state let session A's disposal upload session B's
 * conversation as evidence/judge text, and let colliding turn numbers ship
 * one session's assistant reply for another's turn/end.
 */
describe('per-session isolation (C7)', () => {
  beforeEach(() => {
    h.calls = []
  })

  it('does not leak another session into infer_outcome evidence', async () => {
    const ctx = fakeCtx()
    apply(ctx as never, config({ inferOutcomeOnSessionEnd: true }))
    const emit = (id: string, event: unknown) =>
      ctx.handlers.get('session/event')!({ id }, event as never)
    const dispose = (id: string) => ctx.handlers.get('session/disposed')!({ id } as never)

    emit('A', userMessage('fix the login bug in service A'))
    emit('A', assistantMessage(1, 'login bug in A is fixed'))
    emit('B', userMessage('refactor the database layer in service B'))
    emit('B', assistantMessage(1, 'database layer in B refactored'))

    dispose('A')
    await flush()

    expect(h.calls).toHaveLength(1)
    expect(h.calls[0].method).toBe('inferOutcome')
    const [evidence, judgeContext] = h.calls[0].args as [
      { task?: string },
      string,
    ]
    expect(evidence.task).toBe('fix the login bug in service A')
    expect(judgeContext).toContain('login bug in A is fixed')
    expect(judgeContext).not.toContain('service B')
    expect(judgeContext).not.toContain('refactor')

    // Disposing B reports B's own conversation — the trackers were separate.
    dispose('B')
    await flush()
    expect(h.calls).toHaveLength(2)
    const [evidenceB] = h.calls[1].args as [{ task?: string }]
    expect(evidenceB.task).toBe('refactor the database layer in service B')
  })

  it('does not ship a foreign session text on colliding turn numbers', async () => {
    const ctx = fakeCtx()
    apply(ctx as never, config({ extractOnTurnEnd: true }))
    const emit = (id: string, event: unknown) =>
      ctx.handlers.get('session/event')!({ id }, event as never)

    emit('A', turnStart(1))
    emit('A', userMessage('question for A'))
    emit('A', assistantMessage(1, 'answer from A'))
    // Session B reuses turn number 1 and ends its turn with no assistant
    // reply of its own — the extraction must NOT carry A's answer.
    emit('B', turnStart(1))
    emit('B', userMessage('question for B'))
    emit('B', turnEnd(1))
    await flush()

    const bCall = h.calls.find(
      c =>
        c.method === 'notifyResponse' &&
        (c.args[2] as string | undefined)?.includes('question for B'),
    )
    expect(bCall, "B's turn/end must send B's own text").toBeDefined()
    expect(bCall!.args[0]).toBe('')
    expect(JSON.stringify(bCall!.args)).not.toContain('answer from A')

    // A's own pending turn is untouched and still ships when A ends.
    emit('A', turnEnd(1))
    await flush()
    const aCall = h.calls.find(
      c =>
        c.method === 'notifyResponse' &&
        (c.args[2] as string | undefined)?.includes('question for A'),
    )
    expect(aCall).toBeDefined()
    expect(aCall!.args[0]).toBe('answer from A')
  })
})
