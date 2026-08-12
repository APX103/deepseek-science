import { describe, expect, test } from 'bun:test'
import {
  coordinateManualPromptStop,
  coordinateQueuedPromptSteer,
  createPromptRunGateStore,
  resolvePromptRunIntent,
  restoreAndMaybeDrainPromptQueue,
  type PromptRunLease,
  type PromptSteerDependencies,
} from '../src/api/promptRunCoordinator'
import type { QueuedPrompt } from '../src/api/promptQueue'
import { createPromptQueueStore, type PromptQueueStore } from '../src/promptQueueStore'
import {
  completeStream,
  failStream,
  getMessagesSnapshot,
  getStreamSnapshot,
  loadMessages,
  sendUserMessage,
  startStream,
} from '../src/store'
import type { Plan } from '../src/types'

interface Deferred<T> {
  promise: Promise<T>
  resolve: (value: T) => void
  reject: (cause: unknown) => void
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void
  let reject!: (cause: unknown) => void
  const promise = new Promise<T>((accept, decline) => {
    resolve = accept
    reject = decline
  })
  return { promise, resolve, reject }
}

async function nextMicrotask(): Promise<void> {
  await Promise.resolve()
  await Promise.resolve()
}

function queueStore(): PromptQueueStore {
  let id = 0
  return createPromptQueueStore({
    createId: () => `queued-${++id}`,
    now: () => '2026-08-11T00:00:00Z',
  })
}

function enqueue(store: PromptQueueStore, sid: string, text: string, plan = false): QueuedPrompt {
  const prompt = store.enqueue(sid, { text, requestedPlanMode: plan })
  if (!prompt) throw new Error('test enqueue failed')
  return prompt
}

const naturalPolicy = {
  kind: 'natural',
  awaiting: null,
  wasStopping: false,
  steering: null,
} as const

describe('terminal restore and exactly-once draining', () => {
  test('restore lease queues a direct submission until authoritative history is ready', async () => {
    const sid = 'restore-gate'
    const gate = createPromptRunGateStore()
    const store = queueStore()
    const restored = deferred<boolean>()
    const launches: string[] = []

    const draining = restoreAndMaybeDrainPromptQueue(sid, 'run-old', {
      kind: 'error', awaiting: null, wasStopping: false, steering: null,
    }, {
      gate,
      loadMessages: () => restored.promise,
      isRunActive: () => false,
      claimAndLaunchNext: (_sid, lease) => {
        expect(gate.isCurrent(lease)).toBe(true)
        const prompt = store.claimNext(sid)
        if (prompt) launches.push(prompt.text)
        return prompt
      },
    })

    expect(gate.isBlocked(sid)).toBe(true)
    // This models Workbench's submit branch: a blocked restore is never a
    // direct send, even though the just-finished stream is no longer running.
    const directLaunchAllowed = !gate.isBlocked(sid)
    expect(directLaunchAllowed).toBe(false)
    enqueue(store, sid, 'typed while restoring')
    expect(gate.requestDrain(sid)).toBe(true)
    expect(launches).toEqual([])

    restored.resolve(true)
    expect((await draining)?.text).toBe('typed while restoring')
    expect(launches).toEqual(['typed while restoring'])
    expect(store.getSnapshot(sid).items).toEqual([])
    expect(gate.isBlocked(sid)).toBe(false)
  })

  test('a failed restore, a newer run, and error terminals never claim', async () => {
    for (const scenario of ['restore-false', 'new-run', 'error-terminal'] as const) {
      const gate = createPromptRunGateStore()
      const store = queueStore()
      enqueue(store, scenario, 'keep me')
      let claims = 0
      const result = await restoreAndMaybeDrainPromptQueue(
        scenario,
        'run-old',
        scenario === 'error-terminal'
          ? { kind: 'error', awaiting: null, wasStopping: false, steering: null }
          : naturalPolicy,
        {
          gate,
          loadMessages: async () => scenario !== 'restore-false',
          isRunActive: () => scenario === 'new-run',
          claimAndLaunchNext: () => {
            claims += 1
            return store.claimNext(scenario)
          },
        },
      )
      expect(result).toBeNull()
      expect(claims).toBe(0)
      expect(store.getSnapshot(scenario).items.map((item) => item.text)).toEqual(['keep me'])
    }
  })

  test('duplicate and stale terminal callbacks cannot consume the following item', async () => {
    const sid = 'terminal-first-wins'
    const store = queueStore()
    enqueue(store, sid, 'first queued')
    enqueue(store, sid, 'must remain')
    sendUserMessage(sid, 'running prompt')
    const oldRunId = startStream(sid)
    const launches: string[] = []

    const accepted = completeStream(
      sid, null, 1, 'natural', null, null, null, null, undefined, oldRunId,
    )
    expect(accepted).toBe(true)
    if (accepted) {
      await restoreAndMaybeDrainPromptQueue(sid, oldRunId, naturalPolicy, {
        gate: createPromptRunGateStore(),
        loadMessages: async () => true,
        isRunActive: () => !!getStreamSnapshot(sid)?.running,
        claimAndLaunchNext: () => {
          const prompt = store.claimNext(sid)
          if (!prompt) return null
          launches.push(prompt.text)
          sendUserMessage(sid, prompt.text)
          startStream(sid)
          return prompt
        },
      })
    }

    expect(completeStream(
      sid, null, 1, 'natural', null, null, null, null, undefined, oldRunId,
    )).toBe(false)
    expect(failStream(sid, 'late old error', oldRunId)).toBe(false)
    expect(launches).toEqual(['first queued'])
    expect(store.getSnapshot(sid).items.map((item) => item.text)).toEqual(['must remain'])

    const newRunId = getStreamSnapshot(sid)?.runId
    if (!newRunId) throw new Error('new run was not registered')
    expect(failStream(sid, 'new run failed', newRunId)).toBe(true)
    expect(store.getSnapshot(sid).items.map((item) => item.text)).toEqual(['must remain'])
  })
})

describe('manual Stop acknowledgement races', () => {
  test('cancelled=false holds the session gate and active stream until restore succeeds', async () => {
    const gate = createPromptRunGateStore()
    const ack = deferred<{ cancelled: boolean }>()
    const restore = deferred<boolean>()
    const calls: string[] = []
    let active = true
    let stopping = true

    const stoppingPromise = coordinateManualPromptStop('sid', 'run-1', {
      gate,
      cancelRun: () => ack.promise,
      finishCancelledRun: () => false,
      retireNormallyFinishedRun: (_sid, runId) => {
        calls.push(`retire:${runId}`)
        if (!active || runId !== 'run-1') return false
        active = false
        stopping = false
        return true
      },
      failStop: () => { throw new Error('unexpected failure') },
      loadMessages: async () => {
        calls.push('load:start')
        const restored = await restore.promise
        calls.push(`load:end:${restored}`)
        return restored
      },
    })

    expect(gate.isBlocked('sid')).toBe(true)
    ack.resolve({ cancelled: false })
    await nextMicrotask()
    expect(calls).toEqual(['load:start'])
    expect(active).toBe(true)
    expect(stopping).toBe(true)
    expect(gate.isBlocked('sid')).toBe(true)

    restore.resolve(true)
    expect(await stoppingPromise).toBe('finished')
    expect(calls).toEqual(['load:start', 'load:end:true', 'retire:run-1'])
    expect(active).toBe(false)
    expect(gate.isBlocked('sid')).toBe(false)
  })

  test('cancelled=false restore failure keeps the active run retryable and never retires', async () => {
    const gate = createPromptRunGateStore()
    const calls: string[] = []
    let active = true
    let stopping = true
    const result = await coordinateManualPromptStop('sid', 'run-1', {
      gate,
      cancelRun: async () => ({ cancelled: false }),
      finishCancelledRun: () => false,
      retireNormallyFinishedRun: () => {
        calls.push('retire')
        active = false
        return true
      },
      failStop: (_sid, runId, error) => {
        calls.push(`fail:${runId}:${error}`)
        stopping = false
      },
      loadMessages: async () => false,
    })

    expect(result).toBe('failed')
    expect(active).toBe(true)
    expect(stopping).toBe(false)
    expect(calls).toEqual([
      'fail:run-1:无法恢复上一轮的权威历史；当前运行已保留，可重试停止。',
    ])
    expect(gate.isBlocked('sid')).toBe(false)
  })

  test('a terminal arriving during the manual Stop GET stays classified and wins retire', async () => {
    const gate = createPromptRunGateStore()
    const restore = deferred<boolean>()
    const calls: string[] = []
    let active = true
    let classified = false
    const stoppingPromise = coordinateManualPromptStop('sid', 'run-1', {
      gate,
      cancelRun: async () => ({ cancelled: false }),
      finishCancelledRun: () => false,
      retireNormallyFinishedRun: () => {
        calls.push('retire')
        return active
      },
      failStop: () => { calls.push('fail') },
      loadMessages: async () => {
        calls.push('load:start')
        const restored = await restore.promise
        calls.push('load:end')
        return restored
      },
    })

    await nextMicrotask()
    expect(calls).toEqual(['load:start'])
    // The late terminal cannot acquire a second restore owner, but it can
    // classify the exact local stream while the manual GET remains in flight.
    expect(gate.acquire('sid', 'run-1', 'terminal-restore')).toBeNull()
    active = false
    classified = true
    restore.resolve(true)

    expect(await stoppingPromise).toBe('stale')
    expect(classified).toBe(true)
    expect(calls).toEqual(['load:start', 'load:end', 'retire'])
    expect(gate.isBlocked('sid')).toBe(false)
  })

  test('cancelled=true settles and restores once; failure keeps the run retryable', async () => {
    const calls: string[] = []
    expect(await coordinateManualPromptStop('sid', 'run-1', {
      gate: createPromptRunGateStore(),
      cancelRun: async () => ({ cancelled: true }),
      finishCancelledRun: (_sid, runId) => {
        calls.push(`finish:${runId}`)
        return true
      },
      retireNormallyFinishedRun: () => false,
      failStop: () => { throw new Error('unexpected failure') },
      loadMessages: async () => {
        calls.push('load')
        return true
      },
    })).toBe('cancelled')
    expect(calls).toEqual(['finish:run-1', 'load'])

    let failedRun = ''
    expect(await coordinateManualPromptStop('sid', 'run-2', {
      gate: createPromptRunGateStore(),
      cancelRun: async () => { throw new Error('offline') },
      finishCancelledRun: () => false,
      retireNormallyFinishedRun: () => false,
      failStop: (_sid, runId, error) => { failedRun = `${runId}:${error}` },
      loadMessages: async () => true,
    })).toBe('failed')
    expect(failedRun).toBe('run-2:offline')
  })
})

describe('cancel-and-restart steer races', () => {
  test('selected item stays locked and is claimed only after cancel ack plus restore', async () => {
    const sid = 'steer-true'
    const gate = createPromptRunGateStore()
    const store = queueStore()
    const selected = enqueue(store, sid, 'selected')
    const other = enqueue(store, sid, 'other')
    expect(store.beginSteering(sid, selected.id, selected.revision)).toBe(true)
    const ack = deferred<{ cancelled: boolean }>()
    const restored = deferred<boolean>()
    const launches: string[] = []

    const resultPromise = coordinateQueuedPromptSteer(
      sid,
      'run-1',
      { itemId: selected.id, revision: selected.revision },
      {
        gate,
        beginStop: (sessionId, runId) => {
          expect([sessionId, runId]).toEqual([sid, 'run-1'])
          return runId
        },
        cancelRun: () => ack.promise,
        finishCancelledRun: () => undefined,
        retireNormallyFinishedRun: () => { throw new Error('wrong settle path') },
        failStop: () => { throw new Error('unexpected failure') },
        loadMessages: () => restored.promise,
        claimAndLaunchSelected: (_sid, itemId, revision, lease) => {
          expect(gate.isCurrent(lease)).toBe(true)
          const prompt = store.claimItem(sid, itemId, revision)
          if (prompt) launches.push(prompt.text)
          return prompt
        },
        clearSteering: (_sid, itemId, revision) => {
          store.clearSteering(sid, itemId, revision)
        },
      },
    )

    expect(store.edit(sid, selected.id, selected.revision, 'changed')).toBe(false)
    expect(store.delete(sid, selected.id, selected.revision)).toBe(false)
    expect(store.reorder(sid, selected.id, other.id)).toBe(false)
    expect(store.claimNext(sid)).toBeNull()
    expect(launches).toEqual([])

    ack.resolve({ cancelled: true })
    await nextMicrotask()
    expect(launches).toEqual([])
    restored.resolve(true)
    expect(await resultPromise).toMatchObject({ status: 'launched', cancelledPreviousRun: true })
    expect(launches).toEqual(['selected'])
    expect(store.claimItem(sid, selected.id, selected.revision)).toBeNull()
    expect(store.getSnapshot(sid).items.map((item) => item.text)).toEqual(['other'])
  })

  test('a natural terminal on either side of cancelled=false cannot steal auto-drain', async () => {
    for (const timing of ['before-ack', 'after-ack'] as const) {
      const sid = `steer-false-${timing}`
      const gate = createPromptRunGateStore()
      const store = queueStore()
      const ordinaryHead = enqueue(store, sid, 'ordinary head')
      const selected = enqueue(store, sid, 'selected steer')
      store.beginSteering(sid, selected.id, selected.revision)
      const ack = deferred<{ cancelled: boolean }>()
      const restoreSteer = deferred<boolean>()
      const launches: string[] = []
      let retireCount = 0
      let terminalClassified = false

      const dependencies: PromptSteerDependencies = {
        gate,
        beginStop: (_sid, runId) => runId,
        cancelRun: () => ack.promise,
        finishCancelledRun: () => { throw new Error('wrong settle path') },
        retireNormallyFinishedRun: () => { retireCount += 1 },
        failStop: () => { throw new Error('unexpected failure') },
        loadMessages: () => restoreSteer.promise,
        claimAndLaunchSelected: (_sid, itemId, revision, lease) => {
          expect(gate.isCurrent(lease)).toBe(true)
          const prompt = store.claimItem(sid, itemId, revision)
          if (prompt) launches.push(prompt.text)
          return prompt
        },
        clearSteering: (_sid, itemId, revision) => {
          store.clearSteering(sid, itemId, revision)
        },
      }
      const steering = coordinateQueuedPromptSteer(
        sid, 'run-1', { itemId: selected.id, revision: selected.revision }, dependencies,
      )

      const terminal = () => {
        terminalClassified = true
        return restoreAndMaybeDrainPromptQueue(
          sid,
          'run-1',
          { ...naturalPolicy, steering: { itemId: selected.id, revision: selected.revision } },
          {
            gate,
            loadMessages: async () => true,
            isRunActive: () => false,
            claimAndLaunchNext: (_sid, _lease: PromptRunLease) => {
              const prompt = store.claimNext(sid)
              if (prompt) launches.push(prompt.text)
              return prompt
            },
          },
        )
      }

      if (timing === 'before-ack') expect(await terminal()).toBeNull()
      ack.resolve({ cancelled: false })
      await nextMicrotask()
      expect(retireCount).toBe(0)
      if (timing === 'after-ack') expect(await terminal()).toBeNull()
      restoreSteer.resolve(true)
      expect(await steering).toMatchObject({ status: 'launched', cancelledPreviousRun: false })
      expect(retireCount).toBe(1)
      expect(terminalClassified).toBe(true)
      expect(launches).toEqual(['selected steer'])
      expect(store.getSnapshot(sid).items).toEqual([ordinaryHead])
    }
  })

  test('cancelled=false restore failure preserves the active run and selected queue item', async () => {
    const sid = 'steer-false-restore-failure'
    const gate = createPromptRunGateStore()
    const store = queueStore()
    const selected = enqueue(store, sid, 'preserve selected')
    store.beginSteering(sid, selected.id, selected.revision)
    const calls: string[] = []
    let active = true
    let stopping = true

    const result = await coordinateQueuedPromptSteer(
      sid,
      'run-1',
      { itemId: selected.id, revision: selected.revision },
      {
        gate,
        beginStop: (_sid, runId) => runId,
        cancelRun: async () => ({ cancelled: false }),
        finishCancelledRun: () => { throw new Error('wrong settle path') },
        retireNormallyFinishedRun: () => {
          calls.push('retire')
          active = false
        },
        failStop: (_sid, runId, error) => {
          calls.push(`fail:${runId}:${error}`)
          stopping = false
        },
        loadMessages: async () => false,
        claimAndLaunchSelected: () => {
          calls.push('claim')
          return store.claimItem(sid, selected.id, selected.revision)
        },
        clearSteering: (_sid, itemId, revision) => {
          calls.push('clear')
          store.clearSteering(sid, itemId, revision)
        },
      },
    )

    expect(result).toEqual({
      status: 'failed',
      error: '无法恢复上一轮的权威历史，队列消息已保留。',
    })
    expect(calls).toEqual([
      'fail:run-1:无法恢复上一轮的权威历史；当前运行已保留，可重试调整方向。',
      'clear',
    ])
    expect(active).toBe(true)
    expect(stopping).toBe(false)
    expect(store.getSnapshot(sid).items).toEqual([selected])
    expect(store.getSnapshot(sid).steering).toBeNull()
    expect(gate.isBlocked(sid)).toBe(false)
  })

  test('cancel/restore/stale failures preserve the exact item and never launch a replacement', async () => {
    for (const failure of ['cancel', 'restore', 'stale-run', 'stale-item'] as const) {
      const sid = `steer-failure-${failure}`
      const gate = createPromptRunGateStore()
      const store = queueStore()
      const selected = enqueue(store, sid, 'preserve me')
      enqueue(store, sid, 'do not substitute')
      store.beginSteering(sid, selected.id, selected.revision)
      let launches = 0
      const result = await coordinateQueuedPromptSteer(
        sid,
        'run-1',
        { itemId: selected.id, revision: selected.revision },
        {
          gate,
          beginStop: () => failure === 'stale-run' ? 'run-new' : 'run-1',
          cancelRun: async () => {
            if (failure === 'cancel') throw new Error('cancel failed')
            return { cancelled: true }
          },
          finishCancelledRun: () => undefined,
          retireNormallyFinishedRun: () => undefined,
          failStop: () => undefined,
          loadMessages: async () => failure !== 'restore',
          claimAndLaunchSelected: (_sid, itemId, revision) => {
            if (failure === 'stale-item') return null
            const prompt = store.claimItem(sid, itemId, revision)
            if (prompt) launches += 1
            return prompt
          },
          clearSteering: (_sid, itemId, revision) => {
            store.clearSteering(sid, itemId, revision)
          },
        },
      )

      expect(result.status).not.toBe('launched')
      expect(launches).toBe(0)
      expect(store.getSnapshot(sid).items.map((item) => item.text)).toEqual([
        'preserve me', 'do not substitute',
      ])
      expect(store.getSnapshot(sid).steering).toBeNull()
    }
  })

  test('beginStop is exact and a stale run never reaches cancel', async () => {
    const gate = createPromptRunGateStore()
    let cancelCalls = 0
    const seen: string[] = []
    const result = await coordinateQueuedPromptSteer(
      'sid',
      'run-old',
      { itemId: 'queued', revision: 3 },
      {
        gate,
        beginStop: (sid, runId) => {
          seen.push(`${sid}:${runId}`)
          return 'run-new'
        },
        cancelRun: async () => {
          cancelCalls += 1
          return { cancelled: true }
        },
        finishCancelledRun: () => undefined,
        retireNormallyFinishedRun: () => undefined,
        failStop: () => undefined,
        loadMessages: async () => true,
        claimAndLaunchSelected: () => { throw new Error('must not claim') },
        clearSteering: (_sid, itemId, revision) => seen.push(`clear:${itemId}:${revision}`),
      },
    )
    expect(result).toEqual({ status: 'stale' })
    expect(seen).toEqual(['sid:run-old', 'clear:queued:3'])
    expect(cancelCalls).toBe(0)
  })
})

describe('session and launch isolation', () => {
  test('one session restore lease neither blocks nor claims another session', async () => {
    const gate = createPromptRunGateStore()
    const store = queueStore()
    enqueue(store, 'sid-a', 'a')
    enqueue(store, 'sid-b', 'b')
    const restoreA = deferred<boolean>()
    const launched: string[] = []
    const drain = (sid: string, load: () => Promise<boolean>) =>
      restoreAndMaybeDrainPromptQueue(sid, `run-${sid}`, naturalPolicy, {
        gate,
        loadMessages: load,
        isRunActive: () => false,
        claimAndLaunchNext: () => {
          const prompt = store.claimNext(sid)
          if (prompt) launched.push(`${sid}:${prompt.text}`)
          return prompt
        },
      })

    const pendingA = drain('sid-a', () => restoreA.promise)
    expect(gate.isBlocked('sid-a')).toBe(true)
    expect(gate.isBlocked('sid-b')).toBe(false)
    expect((await drain('sid-b', async () => true))?.text).toBe('b')
    expect(launched).toEqual(['sid-b:b'])
    expect(store.getSnapshot('sid-a').items.map((item) => item.text)).toEqual(['a'])

    restoreA.resolve(true)
    expect((await pendingA)?.text).toBe('a')
    expect(launched).toEqual(['sid-b:b', 'sid-a:a'])
  })

  test('retired unknown state yields to restore, while classified null never falls back', () => {
    const restoredPlan: Plan = {
      approved: true,
      steps: [{ title: 'old', status: 'pending' }],
    }
    const restoredRuns = [{
      run_id: 'old', ordinal: 1, frame_id: 'f', task_summary: '', plan_mode: false,
      status: 'awaiting' as const, kind: 'awaiting' as const,
      awaiting: 'user_response' as const, pending_ask: null, error: null,
      usage: { input_tokens: 0, output_tokens: 0 }, iterations: 1, plan: restoredPlan,
      start_seq: 1, end_seq: 2, started_at: '', completed_at: '',
    }]

    expect(resolvePromptRunIntent(
      { running: false, kind: null, awaiting: null, plan: null },
      { plan: restoredPlan, runs: restoredRuns },
      false,
    )).toEqual({ planMode: false, executePlan: true })

    expect(resolvePromptRunIntent(
      { running: false, kind: 'natural', awaiting: null, plan: null },
      { plan: restoredPlan, runs: restoredRuns },
      true,
    )).toEqual({
      planMode: true,
      executePlan: false,
    })
  })

  test('a claimed launch failure is one explicit attempt and is never retried implicitly', async () => {
    const sid = 'launch-409'
    const gate = createPromptRunGateStore()
    const store = queueStore()
    enqueue(store, sid, 'one attempt')
    let starts = 0

    const launched = await restoreAndMaybeDrainPromptQueue(sid, 'run-old', naturalPolicy, {
      gate,
      loadMessages: async () => true,
      isRunActive: () => false,
      claimAndLaunchNext: () => {
        const prompt = store.claimNext(sid)
        if (prompt) starts += 1 // The later HTTP 409/config error belongs to this attempt.
        return prompt
      },
    })
    expect(launched?.text).toBe('one attempt')
    expect(starts).toBe(1)
    expect(store.getSnapshot(sid).items).toEqual([])

    await restoreAndMaybeDrainPromptQueue(
      sid,
      'run-failed',
      { kind: 'error', awaiting: null, wasStopping: false, steering: null },
      {
        gate,
        loadMessages: async () => true,
        isRunActive: () => false,
        claimAndLaunchNext: () => {
          starts += 1
          return null
        },
      },
    )
    expect(starts).toBe(1)
  })

  test('a newer user turn invalidates an older GET before it can overwrite the bubble', async () => {
    const sid = 'load-vs-user-turn'
    const oldFetch = globalThis.fetch
    const oldWindow = (globalThis as { window?: unknown }).window
    const oldStorage = (globalThis as { localStorage?: unknown }).localStorage
    const response = deferred<Response>()
    try {
      ;(globalThis as { window?: unknown }).window = {}
      ;(globalThis as { localStorage?: unknown }).localStorage = {
        getItem: () => null,
        setItem: () => undefined,
      }
      globalThis.fetch = async () => response.promise

      const loading = loadMessages(sid)
      await nextMicrotask()
      sendUserMessage(sid, 'new optimistic turn')
      response.resolve(new Response(JSON.stringify({
        id: sid,
        title: 'old',
        workspace: '/tmp',
        model: null,
        status: 'completed',
        plan_mode: false,
        plan: null,
        project_id: null,
        messages: [{ role: 'user', content: { role: 'user', content: 'stale backend turn' } }],
        runs: [],
      }), { status: 200, headers: { 'Content-Type': 'application/json' } }))

      expect(await loading).toBe(false)
      expect(getMessagesSnapshot(sid)).toEqual([{ role: 'user', content: 'new optimistic turn' }])
    } finally {
      globalThis.fetch = oldFetch
      if (oldWindow === undefined) delete (globalThis as { window?: unknown }).window
      else (globalThis as { window?: unknown }).window = oldWindow
      if (oldStorage === undefined) delete (globalThis as { localStorage?: unknown }).localStorage
      else (globalThis as { localStorage?: unknown }).localStorage = oldStorage
    }
  })
})
