import { describe, expect, it } from 'vitest'
import { Config } from './config.js'

describe('Config schema', () => {
  it('applies defaults when no config is given', () => {
    const cfg = Config(undefined)
    expect(cfg.mode).toBe('spawn')
    expect(cfg.db).toBe('~/.memvault/data.db')
    expect(cfg.port).toBe(3778)
    expect(cfg.url).toBe('')
    expect(cfg.binaryPath).toBe('')
    expect(cfg.embeddingProvider).toBe('native')
    expect(cfg.agentId).toBe('dsh')
    expect(cfg.injectOnAssemble).toBe(true)
    expect(cfg.extractOnTurnEnd).toBe(true)
  })

  it('merges provided values over defaults', () => {
    const cfg = Config({ mode: 'attach', url: 'http://x/mcp', port: 5000, agentId: 'a' })
    expect(cfg.mode).toBe('attach')
    expect(cfg.url).toBe('http://x/mcp')
    expect(cfg.port).toBe(5000)
    expect(cfg.agentId).toBe('a')
    expect(cfg.embeddingProvider).toBe('native')
  })

  it('rejects invalid mode values', () => {
    expect(() => Config({ mode: 'bogus' })).toThrow()
  })
})
