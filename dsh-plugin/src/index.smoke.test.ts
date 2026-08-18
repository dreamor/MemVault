import { Context } from '@deepseek-ai/cordis'
import SystemPrompt from '@deepseek-ai/dsh-system-prompt'
import { describe, expect, it } from 'vitest'
import * as memvault from './index.js'

/**
 * Runs the plugin against the REAL `@deepseek-ai/cordis` + `@deepseek-ai/dsh-system-prompt`
 * packages (installed from npm — not mocks) to verify `apply()` actually
 * mounts without crashing and that `ctx.systemPrompt.section()` registration
 * really works. Does not require a running `memvault-proxy`: `mode: attach`
 * with a dummy url means the MCP client is only lazily constructed, never
 * connected, in this test.
 */
describe('memvault dsh plugin (real cordis + dsh-system-prompt)', () => {
  it('mounts and registers a system prompt section', async () => {
    const root = new Context()
    await root.plugin(SystemPrompt, {}).then()

    const fiber = root.plugin(memvault, {
      mode: 'attach',
      url: 'http://127.0.0.1:9/mcp',
      db: '',
      port: 0,
      binaryPath: '',
      agentId: 'test-agent',
      injectOnAssemble: true,
      extractOnTurnEnd: true,
    })
    await fiber.then()

    const assembly = await root.systemPrompt.assemble()
    const section = assembly.sections.find(s => s.name === 'memvault:injection')
    expect(section).toBeDefined()
    expect(section?.text).toBe('') // cache never refreshed — no turn/start fired

    await fiber.dispose()
  })
})
