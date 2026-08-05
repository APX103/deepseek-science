import { describe, expect, test } from 'bun:test'
import { AsyncVersionGuard } from '../src/api/asyncVersionGuard'

describe('AsyncVersionGuard', () => {
  test('invalidates a pending read when a live mutation starts', () => {
    const guard = new AsyncVersionGuard()
    const pending = guard.begin('session-a')
    expect(guard.isCurrent('session-a', pending)).toBe(true)

    guard.invalidate('session-a')
    expect(guard.isCurrent('session-a', pending)).toBe(false)
  })

  test('only the newest read for one key may commit', () => {
    const guard = new AsyncVersionGuard()
    const first = guard.begin('session-a')
    const other = guard.begin('session-b')
    const second = guard.begin('session-a')

    expect(guard.isCurrent('session-a', first)).toBe(false)
    expect(guard.isCurrent('session-a', second)).toBe(true)
    expect(guard.isCurrent('session-b', other)).toBe(true)
  })

  test('a slower earlier backend refresh cannot overwrite a newer result', async () => {
    const guard = new AsyncVersionGuard()
    const committed: string[] = []
    let resolveFirst!: (value: string) => void
    let resolveSecond!: (value: string) => void
    const firstResult = new Promise<string>((resolve) => { resolveFirst = resolve })
    const secondResult = new Promise<string>((resolve) => { resolveSecond = resolve })

    const refresh = async (result: Promise<string>) => {
      const version = guard.begin('backend')
      const value = await result
      if (guard.isCurrent('backend', version)) committed.push(value)
    }

    const firstRefresh = refresh(firstResult)
    const secondRefresh = refresh(secondResult)
    resolveSecond('revision-2')
    await secondRefresh
    resolveFirst('revision-1')
    await firstRefresh

    expect(committed).toEqual(['revision-2'])
  })
})
