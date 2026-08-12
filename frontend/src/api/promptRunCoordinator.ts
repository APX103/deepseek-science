import { composerSendIntent, currentAwaitingKind, type ComposerSendIntent } from './planExecution'
import type { AwaitingKind, Plan, RunKind, SessionRun } from '../types'
import type { PromptQueueItemRef, QueuedPrompt } from './promptQueue'

export interface QueueTerminalPolicy {
  kind: RunKind
  awaiting: AwaitingKind
  /** Stop already owned the run when the terminal frame arrived. */
  wasStopping: boolean
  /** A cancel-and-restart steer owns the next launch. */
  steering: PromptQueueItemRef | null
}

/** Only an ordinary completion (or an ask_user answer boundary) drains FIFO. */
export function shouldAutoDrainPromptQueue(policy: QueueTerminalPolicy): boolean {
  if (policy.wasStopping || policy.steering) return false
  if (policy.kind === 'natural') return true
  return policy.kind === 'awaiting' && policy.awaiting === 'user_response'
}

export interface PromptRunLease {
  sid: string
  token: number
  ownerRunId: string
  purpose: 'terminal-restore' | 'manual-stop' | 'steer'
}

export interface PromptRunGateStore {
  acquire(
    sid: string,
    ownerRunId: string,
    purpose: PromptRunLease['purpose'],
  ): PromptRunLease | null
  isCurrent(lease: PromptRunLease): boolean
  isBlocked(sid: string): boolean
  requestDrain(sid: string): boolean
  isDrainRequested(lease: PromptRunLease): boolean
  release(lease: PromptRunLease): boolean
}

/** Per-session synchronous ownership for restore -> claim -> local run start. */
export function createPromptRunGateStore(): PromptRunGateStore {
  const active = new Map<string, { lease: PromptRunLease; drainRequested: boolean }>()
  let token = 0
  return {
    acquire(sid, ownerRunId, purpose) {
      if (!sid || !ownerRunId || active.has(sid)) return null
      const lease = { sid, ownerRunId, purpose, token: ++token }
      active.set(sid, { lease, drainRequested: false })
      return lease
    },
    isCurrent(lease) {
      return active.get(lease.sid)?.lease.token === lease.token
    },
    isBlocked(sid) {
      return active.has(sid)
    },
    requestDrain(sid) {
      const entry = active.get(sid)
      if (!entry || entry.lease.purpose !== 'terminal-restore') return false
      entry.drainRequested = true
      return true
    },
    isDrainRequested(lease) {
      const entry = active.get(lease.sid)
      return entry?.lease.token === lease.token && entry.drainRequested
    },
    release(lease) {
      if (active.get(lease.sid)?.lease.token !== lease.token) return false
      active.delete(lease.sid)
      return true
    },
  }
}

export const promptRunGate = createPromptRunGateStore()

export interface PromptIntentLiveSnapshot {
  running: boolean
  kind: RunKind | null
  awaiting: AwaitingKind
  plan: Plan | null
}

export interface PromptIntentRestoredSnapshot {
  plan: Plan | null
  runs: readonly SessionRun[]
}

/** Resolve intent from the newest classified state, preserving explicit null. */
export function resolvePromptRunIntent(
  live: PromptIntentLiveSnapshot | undefined,
  restored: PromptIntentRestoredSnapshot | undefined,
  requestedPlanMode: boolean,
): ComposerSendIntent {
  // cancel=false may retire a local SSE before its terminal frame reaches the
  // renderer. That unclassified shell must yield to the history restored after
  // the backend's persistence/mutex boundary. A real live or accepted terminal
  // still owns its plan even when that plan is explicitly null.
  const classifiedLive = live && (live.running || live.kind !== null) ? live : undefined
  const latestPlan = classifiedLive ? classifiedLive.plan : restored?.plan ?? null
  const latestAwaiting = currentAwaitingKind(classifiedLive, restored?.runs)
  return composerSendIntent(latestAwaiting, latestPlan, requestedPlanMode)
}

export interface PromptManualStopDependencies {
  /** Defaults to the production per-session gate; injectable for race tests. */
  gate?: PromptRunGateStore
  cancelRun: (sid: string, runId: string) => Promise<{ cancelled: boolean }>
  finishCancelledRun: (sid: string, runId: string) => boolean
  retireNormallyFinishedRun: (sid: string, runId: string) => boolean
  failStop: (sid: string, runId: string, error: string) => void
  loadMessages: (sid: string) => Promise<boolean>
}

export type PromptManualStopResult = 'cancelled' | 'finished' | 'stale' | 'failed'

/**
 * Settle a user-initiated Stop without ever handing queue ownership back to a
 * late natural terminal. A `cancelled=false` acknowledgement proves that the
 * backend finished, but the local stream remains active/classified until the
 * authoritative GET succeeds. This prevents an unknown retired shell and a
 * direct user launch from racing the restore.
 */
export async function coordinateManualPromptStop(
  sid: string,
  expectedRunId: string,
  dependencies: PromptManualStopDependencies,
): Promise<PromptManualStopResult> {
  const gate = dependencies.gate ?? promptRunGate
  const lease = gate.acquire(sid, expectedRunId, 'manual-stop')
  if (!lease) {
    dependencies.failStop(sid, expectedRunId, '会话正在完成上一项操作，请重试停止。')
    return 'stale'
  }

  try {
    const result = await dependencies.cancelRun(sid, expectedRunId)
    const cancelledLocally = result.cancelled
      ? dependencies.finishCancelledRun(sid, expectedRunId)
      : false

    const restored = await dependencies.loadMessages(sid)
    if (!restored || !gate.isCurrent(lease)) {
      if (!result.cancelled) {
        dependencies.failStop(
          sid,
          expectedRunId,
          '无法恢复上一轮的权威历史；当前运行已保留，可重试停止。',
        )
      }
      return 'failed'
    }

    if (result.cancelled) return cancelledLocally ? 'cancelled' : 'stale'
    return dependencies.retireNormallyFinishedRun(sid, expectedRunId)
      ? 'finished'
      : 'stale'
  } catch (cause) {
    const error = cause instanceof Error ? cause.message : String(cause)
    dependencies.failStop(sid, expectedRunId, error)
    return 'failed'
  } finally {
    gate.release(lease)
  }
}

export interface PromptDrainDependencies {
  gate: PromptRunGateStore
  loadMessages: (sid: string) => Promise<boolean>
  isRunActive: (sid: string) => boolean
  /** One synchronous critical section: validate lease, claim, register run. */
  claimAndLaunchNext: (sid: string, lease: PromptRunLease) => QueuedPrompt | null
}

/**
 * Restore the just-finished canonical transcript before claiming the next
 * prompt. `launch()` is synchronous through local stream registration, so no
 * other browser callback can interleave between the claim and the new run id.
 */
export async function restoreAndMaybeDrainPromptQueue(
  sid: string,
  completedRunId: string,
  policy: QueueTerminalPolicy,
  dependencies: PromptDrainDependencies,
): Promise<QueuedPrompt | null> {
  const lease = dependencies.gate.acquire(sid, completedRunId, 'terminal-restore')
  if (!lease) return null
  try {
    const restored = await dependencies.loadMessages(sid)
    if (!restored || !dependencies.gate.isCurrent(lease)) return null
    const shouldDrain = shouldAutoDrainPromptQueue(policy) || dependencies.gate.isDrainRequested(lease)
    if (!shouldDrain || dependencies.isRunActive(sid)) return null
    return dependencies.claimAndLaunchNext(sid, lease)
  } finally {
    dependencies.gate.release(lease)
  }
}

export interface PromptSteerDependencies {
  gate: PromptRunGateStore
  beginStop: (sid: string, expectedRunId: string) => string | null
  cancelRun: (sid: string, runId: string) => Promise<{ cancelled: boolean }>
  finishCancelledRun: (sid: string, runId: string) => void
  retireNormallyFinishedRun: (sid: string, runId: string) => void
  failStop: (sid: string, runId: string, error: string) => void
  loadMessages: (sid: string) => Promise<boolean>
  claimAndLaunchSelected: (
    sid: string,
    itemId: string,
    expectedRevision: number,
    lease: PromptRunLease,
  ) => QueuedPrompt | null
  clearSteering: (sid: string, itemId: string, expectedRevision: number) => void
}

export type PromptSteerResult =
  | { status: 'launched'; prompt: QueuedPrompt; cancelledPreviousRun: boolean }
  | { status: 'stale' }
  | { status: 'failed'; error: string }

/**
 * Implement the currently honest steer boundary: cancel the exact active run,
 * wait for the backend acknowledgement (which follows persistence + mutex
 * release), restore history, then claim and launch the reserved prompt.
 */
export async function coordinateQueuedPromptSteer(
  sid: string,
  expectedRunId: string,
  selected: PromptQueueItemRef,
  dependencies: PromptSteerDependencies,
): Promise<PromptSteerResult> {
  const lease = dependencies.gate.acquire(sid, expectedRunId, 'steer')
  if (!lease) {
    dependencies.clearSteering(sid, selected.itemId, selected.revision)
    return { status: 'stale' }
  }
  const stoppingRunId = dependencies.beginStop(sid, expectedRunId)
  if (stoppingRunId !== expectedRunId) {
    dependencies.clearSteering(sid, selected.itemId, selected.revision)
    dependencies.gate.release(lease)
    return { status: 'stale' }
  }

  try {
    const result = await dependencies.cancelRun(sid, expectedRunId)
    if (result.cancelled) {
      dependencies.finishCancelledRun(sid, expectedRunId)
    }

    const restored = await dependencies.loadMessages(sid)
    if (!restored || !dependencies.gate.isCurrent(lease)) {
      if (!result.cancelled) {
        dependencies.failStop(
          sid,
          expectedRunId,
          '无法恢复上一轮的权威历史；当前运行已保留，可重试调整方向。',
        )
      }
      dependencies.clearSteering(sid, selected.itemId, selected.revision)
      return {
        status: 'failed',
        error: '无法恢复上一轮的权威历史，队列消息已保留。',
      }
    }
    if (!result.cancelled) {
      // The backend finish acknowledgement is not enough to expose an idle
      // unknown local stream. Retire only after canonical history is present.
      dependencies.retireNormallyFinishedRun(sid, expectedRunId)
    }
    const prompt = dependencies.claimAndLaunchSelected(
      sid,
      selected.itemId,
      selected.revision,
      lease,
    )
    if (!prompt) {
      dependencies.clearSteering(sid, selected.itemId, selected.revision)
      return { status: 'stale' }
    }
    return {
      status: 'launched',
      prompt,
      cancelledPreviousRun: result.cancelled,
    }
  } catch (cause) {
    const error = cause instanceof Error ? cause.message : String(cause)
    dependencies.failStop(sid, expectedRunId, error)
    dependencies.clearSteering(sid, selected.itemId, selected.revision)
    return { status: 'failed', error }
  } finally {
    dependencies.gate.release(lease)
  }
}
