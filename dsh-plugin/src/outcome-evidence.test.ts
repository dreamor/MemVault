import { describe, expect, it } from 'vitest'
import { createOutcomeEvidenceTracker } from './outcome-evidence.js'

describe('createOutcomeEvidenceTracker', () => {
  it('stays resolved with no task text on a session with no turns', () => {
    const tracker = createOutcomeEvidenceTracker()
    const evidence = tracker.snapshot()
    expect(evidence.task).toBeUndefined()
    expect(evidence.terminal).toBe('resolved')
    expect(evidence.retryHits).toBe(0)
  })

  it('captures the first human turn as task text', () => {
    const tracker = createOutcomeEvidenceTracker()
    tracker.onUserMessage('deploy the checkout service to staging')
    tracker.onAssistantMessage('Done.')
    const evidence = tracker.snapshot()
    expect(evidence.task).toBe('deploy the checkout service to staging')
    expect(evidence.terminal).toBe('resolved')
  })

  it('a single tool error does not count as a retry (streak must reach 2)', () => {
    const tracker = createOutcomeEvidenceTracker()
    tracker.onUserMessage('build the release image')
    tracker.onToolCall('t1', 'Bash')
    tracker.onToolResult('t1', true, 'transient blip')
    const evidence = tracker.snapshot()
    expect(evidence.retryHits).toBe(0)
    expect(evidence.terminal).toBe('unresolved_tool_error')
  })

  it('two consecutive errors on the same tool count as one retry', () => {
    const tracker = createOutcomeEvidenceTracker()
    tracker.onUserMessage('build the release image')
    tracker.onToolCall('t1', 'Bash')
    tracker.onToolResult('t1', true, 'nope')
    tracker.onToolCall('t2', 'Bash')
    tracker.onToolResult('t2', true, 'nope again')
    const evidence = tracker.snapshot()
    expect(evidence.retryHits).toBe(1)
    expect(evidence.terminal).toBe('unresolved_tool_error')
    expect(evidence.terminalDetail).toContain('nope again')
  })

  it('a success after errors resets the streak and resolves the terminal state', () => {
    const tracker = createOutcomeEvidenceTracker()
    tracker.onUserMessage('build the release image')
    tracker.onToolCall('t1', 'Bash')
    tracker.onToolResult('t1', true, 'nope')
    tracker.onToolCall('t2', 'Bash')
    tracker.onToolResult('t2', true, 'nope again')
    tracker.onToolCall('t3', 'Bash')
    tracker.onToolResult('t3', false, 'ok')
    const evidence = tracker.snapshot()
    expect(evidence.retryHits).toBe(1)
    expect(evidence.terminal).toBe('resolved')
  })

  it('a rejection phrase in a tool result counts as a rejection and unresolved error', () => {
    const tracker = createOutcomeEvidenceTracker()
    tracker.onUserMessage('run the migration')
    tracker.onToolCall('t1', 'Bash')
    tracker.onToolResult('t1', false, "The user doesn't want to proceed with this tool use.")
    const evidence = tracker.snapshot()
    expect(evidence.rejectionHits).toBe(1)
    expect(evidence.terminal).toBe('unresolved_tool_error')
  })

  it('a correction in a later human turn with no reply is unresolved', () => {
    const tracker = createOutcomeEvidenceTracker()
    tracker.onUserMessage('refactor the auth module')
    tracker.onAssistantMessage('Done, I removed the old session code.')
    tracker.onUserMessage("wait, that's not right, revert that")
    const evidence = tracker.snapshot()
    expect(evidence.correctionHits).toBe(1)
    expect(evidence.terminal).toBe('unresolved_correction')
    expect(evidence.terminalDetail).toContain('revert')
  })

  it('a correction keyword in the opening prompt is not counted', () => {
    const tracker = createOutcomeEvidenceTracker()
    tracker.onUserMessage('wait for the build, then no-op if it fails')
    const evidence = tracker.snapshot()
    expect(evidence.correctionHits).toBe(0)
    expect(evidence.terminal).toBe('resolved')
  })

  it('a correction followed by an assistant reply resolves', () => {
    const tracker = createOutcomeEvidenceTracker()
    tracker.onUserMessage('refactor the auth module')
    tracker.onAssistantMessage('Done.')
    tracker.onUserMessage("wait, that's not right, revert that")
    tracker.onAssistantMessage('Reverted.')
    const evidence = tracker.snapshot()
    expect(evidence.terminal).toBe('resolved')
  })

  it('judgeContext includes the task, latest user text, and latest assistant text', () => {
    const tracker = createOutcomeEvidenceTracker()
    tracker.onUserMessage('deploy the checkout service')
    tracker.onAssistantMessage('Pushing the image.')
    tracker.onUserMessage('actually, hold off on that')
    const context = tracker.judgeContext()
    expect(context).toContain('deploy the checkout service')
    expect(context).toContain('Pushing the image.')
    expect(context).toContain('hold off')
  })
})
