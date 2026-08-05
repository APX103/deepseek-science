import { describe, expect, test } from 'bun:test'
import {
  advanceStreamIteration,
  appendStreamThinking,
  appendStreamText,
  appendStreamToolCall,
  appendStreamToolResult,
  completeStream,
  failStream,
  getMessagesSnapshot,
  getStreamSnapshot,
  resetStreamDraft,
  startStream,
} from '../src/store'
import { HIDDEN_ASSISTANT_PROTOCOL_NOTICE } from '../src/api/assistantProtocol'
import dsmlDisplayCorpus from '../../test-fixtures/dsml-display-corpus.json'

describe('ordered stream timeline', () => {
  test('commits each iteration without moving tools behind the final answer', () => {
    const sid = 'timeline-two-iterations'
    startStream(sid)
    advanceStreamIteration(sid, 1)
    appendStreamText(sid, '先检查数据。')
    appendStreamToolCall(sid, [
      { id: 'a', name: 'read_file', input: { path: 'a.csv' } },
      { id: 'b', name: 'read_file', input: { path: 'b.csv' } },
    ])
    appendStreamToolResult(sid, [
      { tool_use_id: 'b', content: 'B', is_error: false },
      { tool_use_id: 'a', content: 'A', is_error: false },
    ])

    advanceStreamIteration(sid, 2)
    appendStreamText(sid, '最终结论。')
    completeStream(sid, { input_tokens: 10, output_tokens: 5 }, 2)

    const messages = getMessagesSnapshot(sid)
    expect(messages).toHaveLength(2)
    expect(messages[0]?.content).toEqual([
      { type: 'text', text: '先检查数据。' },
      { type: 'tool_use', id: 'a', name: 'read_file', input: { path: 'a.csv' } },
      { type: 'tool_result', tool_use_id: 'a', content: 'A', is_error: false },
      { type: 'tool_use', id: 'b', name: 'read_file', input: { path: 'b.csv' } },
      { type: 'tool_result', tool_use_id: 'b', content: 'B', is_error: false },
    ])
    expect(messages[1]?.content).toEqual([{ type: 'text', text: '最终结论。' }])
    expect(messages[1]?.run?.iterations).toBe(2)
    expect(messages[1]?.run?.usage).toEqual({ input_tokens: 10, output_tokens: 5 })
  })

  test('upgrades result-before-call placeholders and resets only the current draft', () => {
    const sid = 'timeline-placeholder-reset'
    startStream(sid)
    advanceStreamIteration(sid, 1)
    appendStreamToolResult(sid, [{ tool_use_id: 'late', content: 'ready', is_error: false }])
    appendStreamToolCall(sid, [{ id: 'late', name: 'list_files', input: { path: '.' } }])
    expect(getStreamSnapshot(sid)?.toolCalls[0]).toMatchObject({
      id: 'late',
      name: 'list_files',
      input: { path: '.' },
      content: 'ready',
      resolved: true,
    })

    advanceStreamIteration(sid, 2)
    appendStreamText(sid, 'rejected')
    resetStreamDraft(sid)
    expect(getMessagesSnapshot(sid)).toHaveLength(1)
    expect(getStreamSnapshot(sid)?.text).toBe('')
    expect(JSON.stringify(getMessagesSnapshot(sid))).toContain('list_files')
  })

  test('commits anomalous live DSML text only as a display-safe notice', () => {
    const sid = 'timeline-raw-dsml-shield'
    startStream(sid)
    advanceStreamIteration(sid, 1)
    for (const chunk of [
      '<｜｜DS',
      'ML｜｜tool_calls>\n<｜｜DSML｜｜invoke name="python">\n',
      '<｜｜DSML｜｜parameter name="code" string="true">\n',
      '# 3. agenda checks\nprint("must not persist in visible state")\n',
      '</｜｜DSML｜｜parameter>\n</｜｜DSML｜｜invoke>\n</｜｜DSML｜｜tool_calls>',
    ]) {
      appendStreamText(sid, chunk)
    }
    completeStream(sid, { input_tokens: 1, output_tokens: 1 }, 1)

    const serialized = JSON.stringify(getMessagesSnapshot(sid))
    expect(serialized).toContain(HIDDEN_ASSISTANT_PROTOCOL_NOTICE.slice(2))
    expect(serialized).not.toContain('DSML')
    expect(serialized).not.toContain('agenda checks')
    expect(serialized).not.toContain('must not persist')
    expect(serialized).not.toContain('tool_use')
  })

  test('accepts exactly one terminal and rejects every post-terminal frame', () => {
    const sid = 'timeline-terminal-idempotent'
    const runId = startStream(sid)
    expect(appendStreamText(sid, 'only once', runId)).toBe(true)
    expect(appendStreamToolCall(sid, [{ id: 'done', name: 'bash', input: {} }], runId)).toBe(true)

    expect(
      completeStream(sid, null, 1, 'natural', null, null, null, null, undefined, runId),
    ).toBe(true)
    expect(
      completeStream(sid, null, 1, 'natural', null, null, null, null, undefined, runId),
    ).toBe(false)
    expect(appendStreamText(sid, 'late text', runId)).toBe(false)
    expect(failStream(sid, 'late transport error', runId)).toBe(false)

    expect(getMessagesSnapshot(sid)).toHaveLength(1)
    expect(JSON.stringify(getMessagesSnapshot(sid))).not.toContain('late')
    expect(getStreamSnapshot(sid)).toMatchObject({
      running: false,
      text: '',
      thinking: '',
      toolCalls: [],
      kind: 'natural',
      error: null,
    })
  })

  test('cannot let an old connection mutate or terminate a newer run', () => {
    const sid = 'timeline-old-run-isolation'
    const oldRunId = startStream(sid)
    expect(
      completeStream(sid, null, 1, 'natural', null, null, null, null, undefined, oldRunId),
    ).toBe(true)

    const newRunId = startStream(sid)
    expect(appendStreamText(sid, 'new run draft', newRunId)).toBe(true)
    expect(appendStreamText(sid, 'old pollution', oldRunId)).toBe(false)
    expect(failStream(sid, 'old error', oldRunId)).toBe(false)
    expect(
      completeStream(sid, null, 99, 'error', null, null, null, 'old terminal', undefined, oldRunId),
    ).toBe(false)

    expect(getStreamSnapshot(sid)).toMatchObject({
      runId: newRunId,
      running: true,
      text: 'new run draft',
      error: null,
    })
  })

  test('sanitizes anomalous thinking before it enters the transcript', () => {
    const sid = 'timeline-thinking-dsml-shield'
    const runId = startStream(sid)
    expect(appendStreamThinking(sid, RAW_PYTHON_DSML_FOR_THINKING, runId)).toBe(true)
    expect(
      completeStream(sid, null, 1, 'natural', null, null, null, null, undefined, runId),
    ).toBe(true)

    const serialized = JSON.stringify(getMessagesSnapshot(sid))
    expect(serialized).toContain(HIDDEN_ASSISTANT_PROTOCOL_NOTICE.slice(2))
    expect(serialized).not.toContain('DSML')
    expect(serialized).not.toContain('private thought')
  })

  test('shields CommonMark ambiguity before live text is committed', () => {
    for (const [suffix, source, secret] of [
      [
        'paragraph-indent',
        dsmlDisplayCorpus.regressions.paragraph_indented_protocol,
        'INDENT_SECRET',
      ],
      [
        'escaped-backticks',
        dsmlDisplayCorpus.regressions.escaped_backticks_protocol,
        'ESCAPED_SECRET',
      ],
    ] as const) {
      const sid = `timeline-commonmark-shield-${suffix}`
      const runId = startStream(sid)
      expect(appendStreamText(sid, source, runId)).toBe(true)
      expect(
        completeStream(sid, null, 1, 'natural', null, null, null, null, undefined, runId),
      ).toBe(true)
      const serialized = JSON.stringify(getMessagesSnapshot(sid))
      expect(serialized).toContain(HIDDEN_ASSISTANT_PROTOCOL_NOTICE.slice(2))
      expect(serialized).not.toContain('DSML')
      expect(serialized).not.toContain(secret)
    }
  })
})

const RAW_PYTHON_DSML_FOR_THINKING = `<｜DSML｜tool_calls>
<｜DSML｜invoke name="python">
<｜DSML｜parameter name="code" string="true">private thought</｜DSML｜parameter>
</｜DSML｜invoke>
</｜DSML｜tool_calls>`
