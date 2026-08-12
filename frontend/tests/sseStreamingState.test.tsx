import { afterEach, describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { connectSSE, type StreamHandlers } from '../src/api/client'
import AgentMarkdown from '../src/components/workbench/AgentMarkdown'
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
  setStreamPlan,
  setStreamStart,
  startStream,
} from '../src/store'
import type { SSEEvent } from '../src/types'

const originalFetch = globalThis.fetch
const originalWindow = (globalThis as { window?: unknown }).window
const originalLocalStorage = (globalThis as { localStorage?: unknown }).localStorage
const encoder = new TextEncoder()

interface TerminalResult {
  accepted: boolean
  finalText: string | null
  error: string | null
}

interface Deferred<T> {
  promise: Promise<T>
  resolve: (value: T) => void
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((done) => {
    resolve = done
  })
  return { promise, resolve }
}

function installBrowserGlobals() {
  ;(globalThis as { window?: unknown }).window = {}
  ;(globalThis as { localStorage?: unknown }).localStorage = {
    getItem: () => null,
    setItem: () => undefined,
  }
}

function controlledSSE() {
  let controller!: ReadableStreamDefaultController<Uint8Array>
  let cancelled = false
  const body = new ReadableStream<Uint8Array>({
    start(nextController) {
      controller = nextController
    },
    cancel() {
      cancelled = true
    },
  })

  const bytesFor = (event: SSEEvent) =>
    encoder.encode(`data: ${JSON.stringify(event)}\n\n`)

  return {
    response: new Response(body, { status: 200 }),
    enqueue(event: SSEEvent) {
      controller.enqueue(bytesFor(event))
    },
    enqueueBytes(bytes: Uint8Array) {
      controller.enqueue(bytes)
    },
    bytesFor,
    get cancelled() {
      return cancelled
    },
  }
}

function workbenchHandlers(
  sid: string,
  runId: string,
  terminal: Deferred<TerminalResult>,
): StreamHandlers {
  return {
    onStart: (frameId, taskSummary) => setStreamStart(sid, frameId, taskSummary, runId),
    onIteration: (iteration) => advanceStreamIteration(sid, iteration, runId),
    onThinking: (text) => appendStreamThinking(sid, text, runId),
    onText: (text) => appendStreamText(sid, text, runId),
    onDraftReset: () => resetStreamDraft(sid, runId),
    onToolCalls: (calls) => appendStreamToolCall(sid, calls, runId),
    onToolResults: (results) => appendStreamToolResult(sid, results, runId),
    onPlanUpdate: (plan) => setStreamPlan(sid, plan, runId),
    onComplete: (event) => {
      terminal.resolve({
        accepted: completeStream(
          sid,
          event.usage ?? null,
          event.iterations ?? 0,
          event.kind,
          event.pending_ask ?? null,
          event.awaiting ?? null,
          event.plan ?? null,
          event.error ?? null,
          event.artifacts,
          runId,
        ),
        finalText: event.final_text,
        error: null,
      })
    },
    onError: (message) => {
      terminal.resolve({
        accepted: failStream(sid, message, runId),
        finalText: null,
        error: message,
      })
    },
  }
}

async function eventually(assertion: () => boolean, label: string): Promise<void> {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    if (assertion()) return
    await new Promise((resolve) => setTimeout(resolve, 0))
  }
  throw new Error(`Timed out waiting for ${label}`)
}

async function withTimeout<T>(promise: Promise<T>, label: string): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new Error(`Timed out waiting for ${label}`)), 500)
      }),
    ])
  } finally {
    if (timer !== undefined) clearTimeout(timer)
  }
}

function byteIndex(haystack: Uint8Array, needle: Uint8Array): number {
  outer: for (let index = 0; index <= haystack.length - needle.length; index += 1) {
    for (let offset = 0; offset < needle.length; offset += 1) {
      if (haystack[index + offset] !== needle[offset]) continue outer
    }
    return index
  }
  return -1
}

afterEach(() => {
  globalThis.fetch = originalFetch
  if (originalWindow === undefined) delete (globalThis as { window?: unknown }).window
  else (globalThis as { window?: unknown }).window = originalWindow
  if (originalLocalStorage === undefined) {
    delete (globalThis as { localStorage?: unknown }).localStorage
  } else {
    ;(globalThis as { localStorage?: unknown }).localStorage = originalLocalStorage
  }
})

describe('SSE intermediate streaming state', () => {
  test('renders each accepted chunk before terminal and commits repeated final_text only once', async () => {
    installBrowserGlobals()
    const sid = 'sse-controlled-incremental-state'
    const runId = startStream(sid)
    const stream = controlledSSE()
    const terminal = deferred<TerminalResult>()
    globalThis.fetch = async () => stream.response

    connectSSE(sid, 'stream progressively', workbenchHandlers(sid, runId, terminal), { runId })

    stream.enqueue({ type: 'iteration', n: 1 })
    await eventually(() => getStreamSnapshot(sid)?.currentIteration === 1, 'iteration 1')

    // Each incomplete construct is a valid intermediate render state even
    // before the controlled stream combines several of them below.
    for (const intermediate of [
      '#',
      '**尚未结束的强调',
      '```ts\nconst 结果 = "你好"',
      '$$E = mc^2',
      '[资料](https://exam',
      '多字节 emoji 🚀',
    ]) {
      expect(() => renderToStaticMarkup(<AgentMarkdown content={intermediate} />)).not.toThrow()
    }

    // This is deliberately incomplete Markdown: heading/emphasis/fence/math/link
    // prefixes and multibyte content must all remain renderable mid-stream.
    const chunkOne =
      '#\n\n流式标题 🚀\n\n**尚未结束的强调\n\n```ts\nconst 结果 = "你好"\n\n$$E = mc^2\n\n[资料](https://exam'
    const chunkTwo = 'ple.test)\n$$\n```\n**\n\n第二段已到达 🧪'

    stream.enqueue({ type: 'text', text: chunkOne })
    await eventually(() => getStreamSnapshot(sid)?.text === chunkOne, 'first text chunk')

    const firstSnapshot = getStreamSnapshot(sid)
    expect(firstSnapshot).toMatchObject({ running: true, text: chunkOne })
    expect(() => renderToStaticMarkup(<AgentMarkdown content={firstSnapshot?.text ?? ''} />)).not.toThrow()
    const firstHtml = renderToStaticMarkup(<AgentMarkdown content={firstSnapshot?.text ?? ''} />)
    expect(firstHtml).toBe(renderToStaticMarkup(<AgentMarkdown content={chunkOne} />))
    expect(firstHtml).toContain('流式标题')
    expect(firstHtml).not.toContain('第二段已到达')

    // Split one complete SSE frame inside the UTF-8 bytes for the emoji. The
    // first read must not dispatch a partial JSON frame or partial code point.
    const secondFrame = stream.bytesFor({ type: 'text', text: chunkTwo })
    const emojiOffset = byteIndex(secondFrame, encoder.encode('🧪'))
    expect(emojiOffset).toBeGreaterThanOrEqual(0)
    stream.enqueueBytes(secondFrame.slice(0, emojiOffset + 1))
    await new Promise((resolve) => setTimeout(resolve, 0))
    expect(getStreamSnapshot(sid)?.text).toBe(chunkOne)

    stream.enqueueBytes(secondFrame.slice(emojiOffset + 1))
    const acceptedAnswer = chunkOne + chunkTwo
    await eventually(() => getStreamSnapshot(sid)?.text === acceptedAnswer, 'second text chunk')

    const secondSnapshot = getStreamSnapshot(sid)
    expect(secondSnapshot).toMatchObject({ running: true, text: acceptedAnswer })
    expect(() => renderToStaticMarkup(<AgentMarkdown content={secondSnapshot?.text ?? ''} />)).not.toThrow()
    const secondHtml = renderToStaticMarkup(<AgentMarkdown content={secondSnapshot?.text ?? ''} />)
    expect(secondHtml).toBe(renderToStaticMarkup(<AgentMarkdown content={acceptedAnswer} />))
    expect(secondHtml).toContain('第二段已到达')

    stream.enqueue({
      type: 'complete',
      kind: 'natural',
      final_text: acceptedAnswer,
      usage: { input_tokens: 5, output_tokens: 8 },
      iterations: 1,
      frame_status: 'completed',
    })
    const result = await withTimeout(terminal.promise, 'complete frame')

    expect(result).toEqual({ accepted: true, finalText: acceptedAnswer, error: null })
    expect(stream.cancelled).toBe(true)
    expect(getStreamSnapshot(sid)).toMatchObject({ running: false, text: '', thinking: '' })
    const messages = getMessagesSnapshot(sid)
    expect(messages).toHaveLength(1)
    expect(messages[0]?.role).toBe('assistant')
    expect(messages[0]?.content).toEqual([{ type: 'text', text: acceptedAnswer }])
  })

  test('draft reset is scoped to the active run and the next iteration starts cleanly', async () => {
    installBrowserGlobals()
    const sid = 'sse-controlled-reset-and-run-isolation'
    const oldStream = controlledSSE()
    const newStream = controlledSSE()
    let fetchCount = 0
    globalThis.fetch = async () => (fetchCount++ === 0 ? oldStream.response : newStream.response)

    const oldRunId = startStream(sid)
    const oldTerminal = deferred<TerminalResult>()
    connectSSE(sid, 'old run', workbenchHandlers(sid, oldRunId, oldTerminal), { runId: oldRunId })

    const newRunId = startStream(sid)
    const newTerminal = deferred<TerminalResult>()
    connectSSE(sid, 'new run', workbenchHandlers(sid, newRunId, newTerminal), { runId: newRunId })

    newStream.enqueue({ type: 'iteration', n: 1 })
    newStream.enqueue({ type: 'thinking', text: '将被清理的推理' })
    newStream.enqueue({ type: 'text', text: '将被清理的草稿' })
    newStream.enqueue({
      type: 'tool_calls',
      calls: [{ id: 'kept-tool', name: 'read_file', input: { path: 'notes.md' } }],
    })
    await eventually(
      () => getStreamSnapshot(sid)?.toolCalls[0]?.id === 'kept-tool',
      'first-iteration draft and tool call',
    )

    newStream.enqueue({ type: 'draft_reset', reason: 'reviewer retry' })
    await eventually(
      () => getStreamSnapshot(sid)?.text === '' && getStreamSnapshot(sid)?.thinking === '',
      'draft reset',
    )
    const resetSnapshot = getStreamSnapshot(sid)
    expect(resetSnapshot).toMatchObject({
      runId: newRunId,
      running: true,
      text: '',
      thinking: '',
    })
    expect(resetSnapshot?.toolCalls).toEqual([
      {
        id: 'kept-tool',
        name: 'read_file',
        input: { path: 'notes.md' },
        resolved: false,
      },
    ])

    // Frames from the superseded connection are parsed, but their captured
    // run id cannot mutate or terminate the active stream.
    oldStream.enqueue({ type: 'text', text: 'old-run pollution' })
    oldStream.enqueue({
      type: 'complete',
      kind: 'natural',
      final_text: 'old-run pollution',
      usage: { input_tokens: 1, output_tokens: 1 },
      iterations: 1,
      frame_status: 'completed',
    })
    expect(await withTimeout(oldTerminal.promise, 'old terminal frame')).toMatchObject({ accepted: false })
    expect(getStreamSnapshot(sid)).toMatchObject({ runId: newRunId, running: true, text: '' })

    newStream.enqueue({ type: 'iteration', n: 2 })
    await eventually(() => getStreamSnapshot(sid)?.currentIteration === 2, 'iteration 2')
    expect(getStreamSnapshot(sid)).toMatchObject({ text: '', thinking: '', toolCalls: [] })

    const acceptedAnswer = '第二轮的干净答案 ✅'
    newStream.enqueue({ type: 'text', text: acceptedAnswer })
    await eventually(() => getStreamSnapshot(sid)?.text === acceptedAnswer, 'clean second iteration')
    expect(renderToStaticMarkup(<AgentMarkdown content={getStreamSnapshot(sid)?.text ?? ''} />)).toContain(
      '第二轮的干净答案',
    )

    newStream.enqueue({
      type: 'complete',
      kind: 'natural',
      final_text: acceptedAnswer,
      usage: { input_tokens: 3, output_tokens: 4 },
      iterations: 2,
      frame_status: 'completed',
    })
    expect(await withTimeout(newTerminal.promise, 'new terminal frame')).toEqual({
      accepted: true,
      finalText: acceptedAnswer,
      error: null,
    })

    const messages = getMessagesSnapshot(sid)
    expect(messages).toHaveLength(2)
    expect(messages[0]?.content).toEqual([
      { type: 'tool_use', id: 'kept-tool', name: 'read_file', input: { path: 'notes.md' } },
    ])
    expect(messages[1]?.content).toEqual([{ type: 'text', text: acceptedAnswer }])
    expect(JSON.stringify(messages)).not.toContain('old-run pollution')
    expect(JSON.stringify(messages)).not.toContain('将被清理')
  })
})
