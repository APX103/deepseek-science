import { describe, expect, test } from 'bun:test'
import {
  coordinateQueuedPromptSteer,
  createPromptRunGateStore,
  restoreAndMaybeDrainPromptQueue,
  shouldAutoDrainPromptQueue,
  type PromptSteerDependencies,
} from '../src/api/promptRunCoordinator'
import type { QueuedPrompt } from '../src/api/promptQueue'

function prompt(id = 'queued'): QueuedPrompt {
  return {
    id,
    revision: 1,
    text: `text:${id}`,
    createdAt: '2026-08-11T00:00:00Z',
    requestedPlanMode: false,
  }
}

describe('prompt queue terminal policy', () => {
  test('only natural and ask_user boundaries auto-drain', () => {
    expect(shouldAutoDrainPromptQueue({
      kind: 'natural', awaiting: null, wasStopping: false, steering: null,
    })).toBe(true)
    expect(shouldAutoDrainPromptQueue({
      kind: 'awaiting', awaiting: 'user_response', wasStopping: false, steering: null,
    })).toBe(true)

    for (const [kind, awaiting] of [
      ['awaiting', 'plan_approval'],
      ['awaiting', 'plan_execution'],
      ['error', null],
      ['cancelled', null],
      ['max_iters', null],
    ] as const) {
      expect(shouldAutoDrainPromptQueue({
        kind, awaiting, wasStopping: false, steering: null,
      })).toBe(false)
    }
  })

  test('manual Stop and a reserved steer both suppress ordinary draining', () => {
    expect(shouldAutoDrainPromptQueue({
      kind: 'natural', awaiting: null, wasStopping: true, steering: null,
    })).toBe(false)
    expect(shouldAutoDrainPromptQueue({
      kind: 'natural',
      awaiting: null,
      wasStopping: false,
      steering: { itemId: 'steer', revision: 1 },
    })).toBe(false)
  })

  test('restores canonical history before claim and launch', async () => {
    const calls: string[] = []
    let releaseLoad: (() => void) | undefined
    const loading = new Promise<void>((resolve) => { releaseLoad = resolve })
    const queued = prompt()

    const resultPromise = restoreAndMaybeDrainPromptQueue(
      'sid',
      'run-1',
      { kind: 'natural', awaiting: null, wasStopping: false, steering: null },
      {
        gate: createPromptRunGateStore(),
        loadMessages: async () => {
          calls.push('load:start')
          await loading
          calls.push('load:end')
          return true
        },
        isRunActive: () => false,
        claimAndLaunchNext: () => {
          calls.push('claim')
          calls.push(`launch:${queued.id}`)
          return queued
        },
      },
    )

    await Promise.resolve()
    expect(calls).toEqual(['load:start'])
    releaseLoad?.()
    expect((await resultPromise)?.id).toBe(queued.id)
    expect(calls).toEqual(['load:start', 'load:end', 'claim', 'launch:queued'])
  })

  test('a newer active run blocks a stale terminal from claiming', async () => {
    let claimed = false
    await restoreAndMaybeDrainPromptQueue(
      'sid',
      'run-1',
      { kind: 'natural', awaiting: null, wasStopping: false, steering: null },
      {
        gate: createPromptRunGateStore(),
        loadMessages: async () => true,
        isRunActive: () => true,
        claimAndLaunchNext: () => {
          claimed = true
          return prompt()
        },
      },
    )
    expect(claimed).toBe(false)
  })
})

function steerDependencies(
  calls: string[],
  cancelled: boolean,
  claimed: QueuedPrompt | null = prompt('steer'),
): PromptSteerDependencies {
  return {
    gate: createPromptRunGateStore(),
    beginStop: () => {
      calls.push('begin-stop')
      return 'run-1'
    },
    cancelRun: async () => {
      calls.push('cancel-ack')
      return { cancelled }
    },
    finishCancelledRun: () => { calls.push('finish-cancelled') },
    retireNormallyFinishedRun: () => { calls.push('retire-natural') },
    failStop: () => { calls.push('fail-stop') },
    loadMessages: async () => {
      calls.push('load')
      return true
    },
    claimAndLaunchSelected: () => {
      calls.push('claim-selected')
      if (claimed) calls.push(`launch:${claimed.id}`)
      return claimed
    },
    clearSteering: () => { calls.push('clear-steering') },
  }
}

describe('cancel-and-restart steer coordination', () => {
  test('cancelled=true finishes old run, restores, then claims exactly the selected item', async () => {
    const calls: string[] = []
    const result = await coordinateQueuedPromptSteer(
      'sid',
      'run-1',
      { itemId: 'steer', revision: 1 },
      steerDependencies(calls, true),
    )

    expect(result.status).toBe('launched')
    expect(calls).toEqual([
      'begin-stop',
      'cancel-ack',
      'finish-cancelled',
      'load',
      'claim-selected',
      'launch:steer',
    ])
  })

  test('cancelled=false retires the late local stream only after history restore', async () => {
    const calls: string[] = []
    const result = await coordinateQueuedPromptSteer(
      'sid',
      'run-1',
      { itemId: 'steer', revision: 1 },
      steerDependencies(calls, false),
    )

    expect(result).toMatchObject({ status: 'launched', cancelledPreviousRun: false })
    expect(calls).toEqual([
      'begin-stop',
      'cancel-ack',
      'load',
      'retire-natural',
      'claim-selected',
      'launch:steer',
    ])
  })

  test('a cancel failure clears steering without claiming or launching', async () => {
    const calls: string[] = []
    const dependencies = steerDependencies(calls, true)
    dependencies.cancelRun = async () => {
      calls.push('cancel-failed')
      throw new Error('offline')
    }
    const result = await coordinateQueuedPromptSteer(
      'sid',
      'run-1',
      { itemId: 'steer', revision: 1 },
      dependencies,
    )

    expect(result).toEqual({ status: 'failed', error: 'offline' })
    expect(calls).toEqual(['begin-stop', 'cancel-failed', 'fail-stop', 'clear-steering'])
  })

  test('a stale run id clears only the exact steering reservation', async () => {
    const calls: string[] = []
    const dependencies = steerDependencies(calls, true)
    dependencies.beginStop = () => {
      calls.push('begin-stop:new-run')
      return 'run-2'
    }
    const result = await coordinateQueuedPromptSteer(
      'sid',
      'run-1',
      { itemId: 'steer', revision: 1 },
      dependencies,
    )

    expect(result).toEqual({ status: 'stale' })
    expect(calls).toEqual(['begin-stop:new-run', 'clear-steering'])
  })
})
