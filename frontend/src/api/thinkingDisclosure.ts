import type { ContentBlock, Message } from '../types'

/**
 * Frontend-only identity and preference helpers for Thinking disclosures.
 *
 * Identities follow streamed drafts into both the optimistic transcript and a
 * same-process canonical GET. They live in WeakMap UI metadata, never in the
 * persisted message or provider reasoning text.
 */

export type ThinkingDisclosurePreferences = Readonly<Record<string, boolean>>

interface CanonicalThinkingBlock {
  disclosureId: string
  fingerprint: string
}

const disclosureIdsByMessage = new WeakMap<Message, readonly string[]>()
const canonicalBlocksByRun = new Map<string, readonly CanonicalThinkingBlock[]>()
const MAX_TRACKED_RUNS = 128

export function thinkingDisclosureId(
  runId: string,
  iteration: number,
  draftRevision: number,
  blockIndex = 0,
): string {
  return `run:${runId}:iteration:${Math.max(1, iteration)}:draft:${draftRevision}:thinking:${blockIndex}`
}

export function restoredThinkingDisclosureId(
  messageIdentity: string,
  blockIndex: number,
): string {
  return `history:${messageIdentity}:thinking:${blockIndex}`
}

export function thinkingDisclosurePanelId(disclosureId: string): string {
  return `thinking-panel-${encodeURIComponent(disclosureId)}`
}

export function thinkingDisclosureButtonId(disclosureId: string): string {
  return `thinking-button-${encodeURIComponent(disclosureId)}`
}

/** An explicit preference wins; otherwise live blocks open and history stays closed. */
export function resolveThinkingDisclosureOpen(
  preferences: ThinkingDisclosurePreferences,
  disclosureId: string,
  running: boolean,
): boolean {
  return preferences[disclosureId] ?? running
}

export function setThinkingDisclosurePreference(
  preferences: ThinkingDisclosurePreferences,
  disclosureId: string,
  open: boolean,
): ThinkingDisclosurePreferences {
  if (preferences[disclosureId] === open) return preferences
  return { ...preferences, [disclosureId]: open }
}

function thinkingBlocks(message: Message): string[] {
  if (!Array.isArray(message.content)) return []
  return message.content.flatMap((block: ContentBlock) =>
    block.type === 'thinking' ? [block.thinking] : [],
  )
}

function thinkingFingerprint(text: string): string {
  let first = 0x811c9dc5
  let second = 0x9e3779b9
  for (let index = 0; index < text.length; index += 1) {
    const unit = text.charCodeAt(index)
    first = Math.imul(first ^ unit, 0x01000193) >>> 0
    second = Math.imul(second ^ (unit + index), 0x85ebca6b) >>> 0
  }
  return `${text.length}:${first}:${second}`
}

/** Track the latest sanitized snapshot of one live block without retaining its text. */
export function registerThinkingDisclosureBlock(
  runId: string,
  disclosureId: string,
  text: string,
): void {
  const current = canonicalBlocksByRun.get(runId) ?? []
  const block = { disclosureId, fingerprint: thinkingFingerprint(text) }
  const existingIndex = current.findIndex((candidate) => candidate.disclosureId === disclosureId)
  const next = existingIndex >= 0
    ? current.map((candidate, index) => index === existingIndex ? block : candidate)
    : [...current, block]
  canonicalBlocksByRun.delete(runId)
  canonicalBlocksByRun.set(runId, next)
  while (canonicalBlocksByRun.size > MAX_TRACKED_RUNS) {
    const oldestRunId = canonicalBlocksByRun.keys().next().value
    if (oldestRunId === undefined) break
    canonicalBlocksByRun.delete(oldestRunId)
  }
}

/** Remove a rejected retry candidate before its replacement starts streaming. */
export function discardThinkingDisclosureBlock(runId: string, disclosureId: string): void {
  const current = canonicalBlocksByRun.get(runId)
  if (!current) return
  const next = current.filter((candidate) => candidate.disclosureId !== disclosureId)
  if (next.length > 0) canonicalBlocksByRun.set(runId, next)
  else canonicalBlocksByRun.delete(runId)
}

/** Attach ids to an optimistic message and remember how to recognize its canonical successor. */
export function registerThinkingDisclosureMessage(
  message: Message,
  runId: string,
  disclosureIds: readonly string[],
): void {
  const text = thinkingBlocks(message)
  if (text.length === 0 || text.length !== disclosureIds.length) return
  disclosureIdsByMessage.set(message, [...disclosureIds])
  text.forEach((blockText, index) =>
    registerThinkingDisclosureBlock(runId, disclosureIds[index]!, blockText),
  )
}

export function thinkingDisclosureIdsForMessage(message: Message): readonly string[] | undefined {
  return disclosureIdsByMessage.get(message)
}

/** Preserve UI metadata when store code immutably clones a message to attach run state. */
export function carryThinkingDisclosureMessageMetadata(source: Message, target: Message): void {
  const disclosureIds = disclosureIdsByMessage.get(source)
  if (disclosureIds) disclosureIdsByMessage.set(target, disclosureIds)
}

/**
 * Transfer ids after authoritative history replaces optimistic message objects.
 * Exact run order and exact sanitized text must both match; ambiguity fails
 * closed to the normal collapsed-history default.
 */
export function reconcileThinkingDisclosureHistory(messages: readonly Message[]): void {
  const cursorByRun = new Map<string, number>()
  const failedRuns = new Set<string>()
  const mappedMessagesByRun = new Map<string, Message[]>()
  for (const message of messages) {
    const runId = message.source_run_id
    if (!runId) continue
    const text = thinkingBlocks(message)
    if (text.length === 0) continue
    const candidates = canonicalBlocksByRun.get(runId)
    if (!candidates) continue
    disclosureIdsByMessage.delete(message)
    if (failedRuns.has(runId)) continue
    let cursor = cursorByRun.get(runId) ?? 0
    const ids: string[] = []
    for (const blockText of text) {
      const candidate = candidates[cursor]
      if (!candidate || candidate.fingerprint !== thinkingFingerprint(blockText)) {
        failedRuns.add(runId)
        for (const mapped of mappedMessagesByRun.get(runId) ?? []) {
          disclosureIdsByMessage.delete(mapped)
        }
        mappedMessagesByRun.delete(runId)
        cursorByRun.delete(runId)
        break
      }
      ids.push(candidate.disclosureId)
      cursor += 1
    }
    if (!failedRuns.has(runId) && ids.length === text.length) {
      disclosureIdsByMessage.set(message, ids)
      cursorByRun.set(runId, cursor)
      const mapped = mappedMessagesByRun.get(runId) ?? []
      mapped.push(message)
      mappedMessagesByRun.set(runId, mapped)
    }
  }
}
