import { useSyncExternalStore } from 'react'
import {
  createQueuedPrompt,
  EMPTY_PROMPT_QUEUE,
  transitionPromptQueue,
  type PromptQueueAction,
  type PromptQueueState,
  type QueuedPrompt,
} from './api/promptQueue'

export interface EnqueuePromptInput {
  text: string
  requestedPlanMode: boolean
}

export interface PromptQueueStoreDependencies {
  createId?: () => string
  now?: () => string
}

export interface PromptQueueStore {
  getSnapshot(sid: string): PromptQueueState
  subscribe(sid: string, listener: () => void): () => void
  dispatch(sid: string, action: PromptQueueAction): QueuedPrompt | null
  enqueue(sid: string, input: EnqueuePromptInput): QueuedPrompt | null
  edit(sid: string, itemId: string, expectedRevision: number, text: string): boolean
  delete(sid: string, itemId: string, expectedRevision: number): boolean
  reorder(sid: string, itemId: string, targetId: string): boolean
  claimNext(sid: string): QueuedPrompt | null
  claimItem(sid: string, itemId: string, expectedRevision: number): QueuedPrompt | null
  beginSteering(sid: string, itemId: string, expectedRevision: number): boolean
  clearSteering(sid: string, itemId: string, expectedRevision: number): boolean
}

let fallbackIdSequence = 0

function defaultCreateId(): string {
  if (typeof globalThis.crypto?.randomUUID === 'function') {
    return globalThis.crypto.randomUUID()
  }
  fallbackIdSequence += 1
  return `prompt-${Date.now().toString(36)}-${fallbackIdSequence.toString(36)}`
}

function validSid(sid: string): boolean {
  return sid.trim().length > 0
}

export function createPromptQueueStore(
  dependencies: PromptQueueStoreDependencies = {},
): PromptQueueStore {
  const createId = dependencies.createId ?? defaultCreateId
  const now = dependencies.now ?? (() => new Date().toISOString())
  const queues = new Map<string, PromptQueueState>()
  const listeners = new Map<string, Set<() => void>>()

  const getSnapshot = (sid: string): PromptQueueState =>
    validSid(sid) ? (queues.get(sid) ?? EMPTY_PROMPT_QUEUE) : EMPTY_PROMPT_QUEUE

  const notify = (sid: string) => {
    listeners.get(sid)?.forEach((listener) => listener())
  }

  const dispatch = (sid: string, action: PromptQueueAction): QueuedPrompt | null => {
    if (!validSid(sid)) return null
    const current = getSnapshot(sid)
    const transition = transitionPromptQueue(current, action)
    if (transition.state !== current) {
      queues.set(sid, transition.state)
      notify(sid)
    }
    return transition.claimed
  }

  const changedBy = (sid: string, action: PromptQueueAction): boolean => {
    const before = getSnapshot(sid)
    dispatch(sid, action)
    return getSnapshot(sid) !== before
  }

  return {
    getSnapshot,
    subscribe(sid, listener) {
      if (!validSid(sid)) return () => undefined
      let sidListeners = listeners.get(sid)
      if (!sidListeners) {
        sidListeners = new Set()
        listeners.set(sid, sidListeners)
      }
      sidListeners.add(listener)
      return () => {
        sidListeners?.delete(listener)
        if (sidListeners?.size === 0) listeners.delete(sid)
      }
    },
    dispatch,
    enqueue(sid, input) {
      const item = createQueuedPrompt({
        id: createId(),
        text: input.text,
        createdAt: now(),
        requestedPlanMode: input.requestedPlanMode,
      })
      if (!item || !validSid(sid)) return null
      const before = getSnapshot(sid)
      dispatch(sid, { type: 'enqueue', item })
      return getSnapshot(sid) === before ? null : item
    },
    edit: (sid, itemId, expectedRevision, text) =>
      changedBy(sid, { type: 'edit', itemId, expectedRevision, text }),
    delete: (sid, itemId, expectedRevision) =>
      changedBy(sid, { type: 'delete', itemId, expectedRevision }),
    reorder: (sid, itemId, targetId) =>
      changedBy(sid, { type: 'reorder', itemId, targetId }),
    claimNext: (sid) => dispatch(sid, { type: 'claim-next' }),
    claimItem: (sid, itemId, expectedRevision) =>
      dispatch(sid, { type: 'claim-item', itemId, expectedRevision }),
    beginSteering: (sid, itemId, expectedRevision) =>
      changedBy(sid, { type: 'begin-steering', itemId, expectedRevision }),
    clearSteering: (sid, itemId, expectedRevision) =>
      changedBy(sid, { type: 'clear-steering', itemId, expectedRevision }),
  }
}

export const promptQueueStore = createPromptQueueStore()

export function usePromptQueue(sid: string): PromptQueueState {
  return useSyncExternalStore(
    (listener) => promptQueueStore.subscribe(sid, listener),
    () => promptQueueStore.getSnapshot(sid),
    () => promptQueueStore.getSnapshot(sid),
  )
}

export const getPromptQueue = (sid: string) => promptQueueStore.getSnapshot(sid)
export const subscribePromptQueue = (sid: string, listener: () => void) =>
  promptQueueStore.subscribe(sid, listener)
export const enqueuePrompt = (sid: string, input: EnqueuePromptInput) =>
  promptQueueStore.enqueue(sid, input)
export const editQueuedPrompt = (
  sid: string,
  itemId: string,
  expectedRevision: number,
  text: string,
) => promptQueueStore.edit(sid, itemId, expectedRevision, text)
export const deleteQueuedPrompt = (sid: string, itemId: string, expectedRevision: number) =>
  promptQueueStore.delete(sid, itemId, expectedRevision)
export const reorderQueuedPrompt = (sid: string, itemId: string, targetId: string) =>
  promptQueueStore.reorder(sid, itemId, targetId)
export const claimNextQueuedPrompt = (sid: string) => promptQueueStore.claimNext(sid)
export const claimQueuedPrompt = (sid: string, itemId: string, expectedRevision: number) =>
  promptQueueStore.claimItem(sid, itemId, expectedRevision)
export const beginQueuedPromptSteering = (
  sid: string,
  itemId: string,
  expectedRevision: number,
) => promptQueueStore.beginSteering(sid, itemId, expectedRevision)
export const clearQueuedPromptSteering = (
  sid: string,
  itemId: string,
  expectedRevision: number,
) => promptQueueStore.clearSteering(sid, itemId, expectedRevision)
