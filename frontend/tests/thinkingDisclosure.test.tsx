import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import {
  HIDDEN_ASSISTANT_PROTOCOL_NOTICE,
  MAX_ASSISTANT_DISPLAY_TEXT_LENGTH,
} from '../src/api/assistantProtocol'
import {
  resolveThinkingDisclosureOpen,
  restoredThinkingDisclosureId,
  discardThinkingDisclosureBlock,
  reconcileThinkingDisclosureHistory,
  registerThinkingDisclosureBlock,
  registerThinkingDisclosureMessage,
  setThinkingDisclosurePreference,
  thinkingDisclosureButtonId,
  thinkingDisclosureId,
  thinkingDisclosureIdsForMessage,
  thinkingDisclosurePanelId,
} from '../src/api/thinkingDisclosure'
import { ThinkingBlock } from '../src/components/workbench/ChatArea'
import type { Message } from '../src/types'
import {
  advanceStreamIteration,
  appendStreamThinking,
  completeStream,
  getMessagesSnapshot,
  getStreamSnapshot,
  resetStreamDraft,
  startStream,
} from '../src/store'

describe('Thinking disclosure preferences', () => {
  test('keeps an explicit choice across chunks and the live-to-history transition', () => {
    const disclosureId = thinkingDisclosureId('run-choice', 1, 0)
    const initial = {}

    expect(resolveThinkingDisclosureOpen(initial, disclosureId, true)).toBe(true)
    const closed = setThinkingDisclosurePreference(initial, disclosureId, false)
    expect(resolveThinkingDisclosureOpen(closed, disclosureId, true)).toBe(false)
    // The canonical history block uses the same identity, so the explicit choice wins
    // over history's default-closed state rather than being reset by the remount.
    expect(resolveThinkingDisclosureOpen(closed, disclosureId, false)).toBe(false)

    const reopened = setThinkingDisclosurePreference(closed, disclosureId, true)
    expect(resolveThinkingDisclosureOpen(reopened, disclosureId, true)).toBe(true)
    expect(resolveThinkingDisclosureOpen(reopened, disclosureId, false)).toBe(true)
  })

  test('keeps iterations, reset drafts, and multiple historical blocks independent', () => {
    const first = thinkingDisclosureId('run-independent', 1, 0)
    const nextIteration = thinkingDisclosureId('run-independent', 2, 0)
    const resetDraft = thinkingDisclosureId('run-independent', 1, 1)
    const historyA = restoredThinkingDisclosureId('message-a', 0)
    const historyB = restoredThinkingDisclosureId('message-a', 1)
    const preferences = setThinkingDisclosurePreference({}, first, false)

    expect(new Set([first, nextIteration, resetDraft, historyA, historyB]).size).toBe(5)
    expect(resolveThinkingDisclosureOpen(preferences, first, true)).toBe(false)
    expect(resolveThinkingDisclosureOpen(preferences, nextIteration, true)).toBe(true)
    expect(resolveThinkingDisclosureOpen(preferences, resetDraft, true)).toBe(true)
    expect(resolveThinkingDisclosureOpen(preferences, historyA, false)).toBe(false)
    expect(resolveThinkingDisclosureOpen(preferences, historyB, false)).toBe(false)
  })
})

describe('Thinking disclosure markup', () => {
  test('opens live content by default with complete disclosure semantics', () => {
    const disclosureId = thinkingDisclosureId('run-live', 3, 0)
    const panelId = thinkingDisclosurePanelId(disclosureId)
    const buttonId = thinkingDisclosureButtonId(disclosureId)
    const html = renderToStaticMarkup(
      <ThinkingBlock text={'first line\n  indented'} running disclosureId={disclosureId} />,
    )

    expect(html).toContain('aria-busy="true"')
    expect(html).toContain('type="button"')
    expect(html).toContain('aria-expanded="true"')
    expect(html).toContain(`aria-controls="${panelId}"`)
    expect(html).toContain(`id="${panelId}"`)
    expect(html).toContain('role="region"')
    expect(html).toContain(`aria-labelledby="${buttonId}"`)
    expect(html).toContain('focus-visible:ring-2')
    expect(html).toContain('max-h-64')
    expect(html).toContain('overflow-auto')
    expect(html).toContain('whitespace-pre-wrap')
    expect(html).toContain('first line\n  indented')
  })

  test('starts restored history closed and excludes its text from the DOM', () => {
    const disclosureId = restoredThinkingDisclosureId('restored-message', 0)
    const html = renderToStaticMarkup(
      <ThinkingBlock text="historical private display" running={false} disclosureId={disclosureId} />,
    )

    expect(html).toContain('aria-busy="false"')
    expect(html).toContain('aria-expanded="false"')
    expect(html).toContain(`aria-controls="${thinkingDisclosurePanelId(disclosureId)}"`)
    expect(html).not.toContain('role="region"')
    expect(html).not.toContain('historical private display')
  })

  test('sanitizes protocol text and bounds oversized open content', () => {
    const unsafe = '<｜DSML｜tool_calls>private thought</｜DSML｜tool_calls>'
    const unsafeHtml = renderToStaticMarkup(
      <ThinkingBlock
        text={unsafe}
        running={false}
        disclosureId="unsafe"
        open
      />,
    )
    expect(unsafeHtml).toContain(HIDDEN_ASSISTANT_PROTOCOL_NOTICE.slice(2))
    expect(unsafeHtml).not.toContain('DSML')
    expect(unsafeHtml).not.toContain('private thought')

    const oversizedHtml = renderToStaticMarkup(
      <ThinkingBlock
        text={'x'.repeat(MAX_ASSISTANT_DISPLAY_TEXT_LENGTH + 1)}
        running={false}
        disclosureId="oversized"
        open
      />,
    )
    expect(oversizedHtml).toContain(HIDDEN_ASSISTANT_PROTOCOL_NOTICE.slice(2))
    expect(oversizedHtml.length).toBeLessThan(4_000)
  })
})

describe('Thinking disclosure stream identity', () => {
  test('carries the live identity into the committed transcript without changing content', () => {
    const sid = 'thinking-disclosure-live-to-history'
    const runId = startStream(sid)
    advanceStreamIteration(sid, 1, runId)
    appendStreamThinking(sid, 'visible reasoning', runId)

    const live = getStreamSnapshot(sid)!
    const expectedId = thinkingDisclosureId(
      live.runId,
      live.currentIteration,
      live.draftRevision,
    )
    expect(completeStream(sid, null, 1, 'natural', null, null, null, null, undefined, runId)).toBe(true)

    const committed = getMessagesSnapshot(sid).at(-1)
    expect(committed && thinkingDisclosureIdsForMessage(committed)).toEqual([expectedId])
    expect(committed?.content).toEqual([{ type: 'thinking', thinking: 'visible reasoning' }])
  })

  test('assigns a fresh identity after draft reset so rejected choice cannot leak', () => {
    const sid = 'thinking-disclosure-draft-reset'
    const runId = startStream(sid)
    advanceStreamIteration(sid, 1, runId)
    appendStreamThinking(sid, 'rejected reasoning', runId)
    const rejectedId = thinkingDisclosureId(runId, 1, 0)

    resetStreamDraft(sid, runId)
    appendStreamThinking(sid, 'accepted reasoning', runId)
    const accepted = getStreamSnapshot(sid)!
    const acceptedId = thinkingDisclosureId(
      accepted.runId,
      accepted.currentIteration,
      accepted.draftRevision,
    )
    expect(acceptedId).not.toBe(rejectedId)

    completeStream(sid, null, 1, 'natural', null, null, null, null, undefined, runId)
    const committed = getMessagesSnapshot(sid).at(-1)
    expect(committed && thinkingDisclosureIdsForMessage(committed)).toEqual([acceptedId])
    expect(JSON.stringify(committed)).not.toContain('rejected reasoning')
    expect(JSON.stringify(committed)).toContain('accepted reasoning')
  })

  test('reconciles an optimistic choice onto exact canonical history without message metadata', () => {
    const runId = 'thinking-disclosure-canonical'
    const disclosureId = thinkingDisclosureId(runId, 1, 0)
    const optimistic: Message = {
      role: 'assistant',
      content: [{ type: 'thinking', thinking: 'canonical reasoning' }],
    }
    registerThinkingDisclosureMessage(optimistic, runId, [disclosureId])

    const canonical: Message = {
      role: 'assistant',
      content: [{ type: 'thinking', thinking: 'canonical reasoning' }],
      source_run_id: runId,
      source_seq: 42,
    }
    reconcileThinkingDisclosureHistory([canonical])

    expect(thinkingDisclosureIdsForMessage(canonical)).toEqual([disclosureId])
    const opened = setThinkingDisclosurePreference({}, disclosureId, true)
    expect(resolveThinkingDisclosureOpen(opened, thinkingDisclosureIdsForMessage(canonical)![0]!, false)).toBe(true)
    expect(Object.keys(canonical)).not.toContain('thinking_disclosure_ids')
  })

  test('fails closed instead of transferring a choice to mismatched canonical reasoning', () => {
    const runId = 'thinking-disclosure-mismatch'
    const disclosureId = thinkingDisclosureId(runId, 1, 0)
    const optimistic: Message = {
      role: 'assistant',
      content: [{ type: 'thinking', thinking: 'accepted reasoning' }],
    }
    registerThinkingDisclosureMessage(optimistic, runId, [disclosureId])
    const canonical: Message = {
      role: 'assistant',
      content: [{ type: 'thinking', thinking: 'different reasoning' }],
      source_run_id: runId,
      source_seq: 43,
    }

    reconcileThinkingDisclosureHistory([canonical])

    expect(thinkingDisclosureIdsForMessage(canonical)).toBeUndefined()
    expect(resolveThinkingDisclosureOpen({}, restoredThinkingDisclosureId('fallback', 0), false)).toBe(false)
  })

  test('fails the whole run closed after a leading canonical mismatch', () => {
    const runId = 'thinking-disclosure-leading-mismatch'
    const firstId = thinkingDisclosureId(runId, 1, 0)
    const secondId = thinkingDisclosureId(runId, 2, 0)
    registerThinkingDisclosureBlock(runId, firstId, 'A')
    registerThinkingDisclosureBlock(runId, secondId, 'B')
    const canonical = ['X', 'A', 'B'].map<Message>((thinking, index) => ({
      role: 'assistant',
      content: [{ type: 'thinking', thinking }],
      source_run_id: runId,
      source_seq: 60 + index,
    }))

    reconcileThinkingDisclosureHistory(canonical)

    expect(canonical.map(thinkingDisclosureIdsForMessage)).toEqual([
      undefined,
      undefined,
      undefined,
    ])
  })

  test('transfers a live choice when authoritative GET wins before optimistic commit', () => {
    const runId = 'thinking-disclosure-get-wins'
    const disclosureId = thinkingDisclosureId(runId, 1, 0)
    registerThinkingDisclosureBlock(runId, disclosureId, 'live reasoning')
    const canonical: Message = {
      role: 'assistant',
      content: [{ type: 'thinking', thinking: 'live reasoning' }],
      source_run_id: runId,
      source_seq: 49,
    }

    reconcileThinkingDisclosureHistory([canonical])

    expect(thinkingDisclosureIdsForMessage(canonical)).toEqual([disclosureId])
  })

  test('discards a rejected live candidate before canonical reconciliation', () => {
    const runId = 'thinking-disclosure-discarded-live'
    const rejectedId = thinkingDisclosureId(runId, 1, 0)
    const acceptedId = thinkingDisclosureId(runId, 1, 1)
    registerThinkingDisclosureBlock(runId, rejectedId, 'rejected')
    discardThinkingDisclosureBlock(runId, rejectedId)
    registerThinkingDisclosureBlock(runId, acceptedId, 'accepted')
    const canonical: Message = {
      role: 'assistant',
      content: [{ type: 'thinking', thinking: 'accepted' }],
      source_run_id: runId,
      source_seq: 48,
    }

    reconcileThinkingDisclosureHistory([canonical])

    expect(thinkingDisclosureIdsForMessage(canonical)).toEqual([acceptedId])
  })

  test('keeps canonical run order across checkpoint and terminal GET replacements', () => {
    const runId = 'thinking-disclosure-repeated-get'
    const firstId = thinkingDisclosureId(runId, 1, 0)
    const secondId = thinkingDisclosureId(runId, 2, 0)
    const firstOptimistic: Message = {
      role: 'assistant',
      content: [{ type: 'thinking', thinking: 'same reasoning' }],
    }
    const secondOptimistic: Message = {
      role: 'assistant',
      content: [{ type: 'thinking', thinking: 'same reasoning' }],
    }
    registerThinkingDisclosureMessage(firstOptimistic, runId, [firstId])
    registerThinkingDisclosureMessage(secondOptimistic, runId, [secondId])

    const checkpoint: Message = {
      role: 'assistant',
      content: [{ type: 'thinking', thinking: 'same reasoning' }],
      source_run_id: runId,
      source_seq: 50,
    }
    reconcileThinkingDisclosureHistory([checkpoint])
    expect(thinkingDisclosureIdsForMessage(checkpoint)).toEqual([firstId])

    const finalFirst: Message = {
      role: 'assistant',
      content: [{ type: 'thinking', thinking: 'same reasoning' }],
      source_run_id: runId,
      source_seq: 50,
    }
    const finalSecond: Message = {
      role: 'assistant',
      content: [{ type: 'thinking', thinking: 'same reasoning' }],
      source_run_id: runId,
      source_seq: 51,
    }
    reconcileThinkingDisclosureHistory([finalFirst, finalSecond])

    expect(thinkingDisclosureIdsForMessage(finalFirst)).toEqual([firstId])
    expect(thinkingDisclosureIdsForMessage(finalSecond)).toEqual([secondId])
  })
})
