import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { appendStreamText, completeStream, getMessagesSnapshot, startStream } from '../src/store'
import { hasPersistedRunFailure, RunFooter } from '../src/components/workbench/ChatArea'
import { mapSessionRun, normalizeSessionStatus } from '../src/api/client'
import type { Message, SessionRun } from '../src/types'

function run(overrides: Partial<SessionRun> = {}): SessionRun {
  return {
    run_id: 'restored-run',
    ordinal: 1,
    frame_id: 'frame',
    task_summary: 'test',
    plan_mode: false,
    status: 'failed',
    kind: 'max_iters',
    awaiting: null,
    pending_ask: null,
    error: null,
    usage: { input_tokens: 10, output_tokens: 5 },
    iterations: 25,
    plan: null,
    start_seq: 1,
    end_seq: 2,
    started_at: '2026-08-05T00:00:00Z',
    completed_at: '2026-08-05T00:01:00Z',
    ...overrides,
  }
}

describe('run terminal state', () => {
  test('live max_iters fails closed with fallback detail', () => {
    const sid = 'live-max-iters-fallback'
    startStream(sid)
    appendStreamText(sid, 'partial result')
    completeStream(sid, { input_tokens: 12, output_tokens: 3 }, 25, 'max_iters', null, null, null, null)

    const attached = getMessagesSnapshot(sid).at(-1)?.run
    expect(attached?.kind).toBe('max_iters')
    expect(attached?.status).toBe('failed')
    expect(attached?.error).toBeTruthy()
  })

  test('restored legacy max_iters always renders a partial-output failure banner', () => {
    const html = renderToStaticMarkup(<RunFooter run={run()} />)
    expect(html).toContain('Agent Failed')
    expect(html).toContain('执行预算已耗尽')
    expect(html).toContain('以上输出可能不完整')
    expect(html).toContain('达到迭代上限')
    expect(html).not.toContain('已完成')
  })

  test('restored max_iters is normalized from kind even when legacy fields claim success', () => {
    const restored = mapSessionRun({
      run_id: 'legacy-max-iters',
      ordinal: 1,
      frame_id: 'frame',
      task_summary: 'test',
      plan_mode: false,
      status: 'completed',
      kind: 'max_iters',
      error: null,
      started_at: '2026-08-05T00:00:00Z',
    })
    expect(restored.status).toBe('failed')
    expect(restored.error).toBeTruthy()
  })

  test('unknown and legacy failure-like session statuses fail closed', () => {
    expect(normalizeSessionStatus('max_iters')).toBe('failed')
    expect(normalizeSessionStatus('ERRORED')).toBe('failed')
    expect(normalizeSessionStatus('future_terminal_state')).toBe('failed')
    expect(normalizeSessionStatus('completed')).toBe('completed')
    expect(normalizeSessionStatus('needs_reconciliation')).toBe('needs_reconciliation')
  })

  test('reconciliation is rendered as an actionable safety boundary, not a generic failure', () => {
    const reconciliation = run({
      status: 'needs_reconciliation',
      kind: 'reconciliation',
      awaiting: 'tool_reconciliation',
      error: 'External result unknown',
      completed_at: null,
    })
    const html = renderToStaticMarkup(<RunFooter run={reconciliation} />)
    expect(html).toContain('外部工具结果未知')
    expect(html).toContain('等待外部结果对账')
    expect(html).not.toContain('Agent Failed')
    expect(
      hasPersistedRunFailure([{ role: 'assistant', content: '', run: reconciliation }]),
    ).toBe(false)

    const restored = mapSessionRun({
      run_id: reconciliation.run_id,
      ordinal: reconciliation.ordinal,
      frame_id: reconciliation.frame_id,
      task_summary: reconciliation.task_summary,
      plan_mode: false,
      status: 'needs_reconciliation',
      kind: 'reconciliation',
      awaiting: 'tool_reconciliation',
      error: reconciliation.error,
      started_at: reconciliation.started_at,
    })
    expect(restored.status).toBe('needs_reconciliation')
  })

  test('restored failed status renders failure even with natural kind and no error detail', () => {
    const failedRun = run({ kind: 'natural', status: 'failed', error: null, iterations: 1 })
    const html = renderToStaticMarkup(<RunFooter run={failedRun} />)
    expect(html).toContain('Agent Failed')
    expect(html).toContain('后端未返回详细原因')
    expect(html).toContain('执行失败')
    expect(html).not.toContain('已完成')

    const messages: Message[] = [{ role: 'assistant', content: 'partial', run: failedRun }]
    expect(hasPersistedRunFailure(messages)).toBe(true)
  })

  test('natural completion does not inherit the failure banner', () => {
    const html = renderToStaticMarkup(
      <RunFooter run={run({ kind: 'natural', status: 'completed', error: null, iterations: 1 })} />,
    )
    expect(html).toContain('已完成')
    expect(html).not.toContain('Agent Failed')
    expect(html).not.toContain('以上输出可能不完整')
  })

  test('restored interrupted run never masquerades as completed', () => {
    const interrupted = run({
      status: 'interrupted',
      kind: 'cancelled',
      error: 'App exited before this run completed',
      usage: { input_tokens: 0, output_tokens: 0 },
      iterations: 0,
    })
    const html = renderToStaticMarkup(
      <RunFooter run={interrupted} />,
    )
    expect(html).toContain('已停止')
    expect(html).not.toContain('已完成')
    expect(html).not.toContain('Agent Failed')
    expect(hasPersistedRunFailure([{ role: 'assistant', content: '', run: interrupted }])).toBe(
      false,
    )

    const restored = mapSessionRun({
      run_id: interrupted.run_id,
      ordinal: interrupted.ordinal,
      frame_id: interrupted.frame_id,
      task_summary: interrupted.task_summary,
      plan_mode: false,
      status: 'interrupted',
      kind: 'cancelled',
      error: interrupted.error,
      started_at: interrupted.started_at,
    })
    expect(restored.status).toBe('interrupted')
  })

  test('legacy interrupted status is truthful even when kind is missing', () => {
    const html = renderToStaticMarkup(
      <RunFooter
        run={run({
          status: 'interrupted',
          kind: null,
          error: null,
          usage: { input_tokens: 0, output_tokens: 0 },
          iterations: 0,
        })}
      />,
    )
    expect(html).toContain('已停止')
    expect(html).not.toContain('已完成')
  })
})
