import { describe, expect, test } from 'bun:test'
import {
  createQueuedPrompt,
  EMPTY_PROMPT_QUEUE,
  promptQueueReducer,
  selectReorderTargetId,
  transitionPromptQueue,
  type PromptQueueAction,
  type PromptQueueState,
  type QueuedPrompt,
} from '../src/api/promptQueue'
import { createPromptQueueStore } from '../src/promptQueueStore'

function item(id: string, text = id, requestedPlanMode = false): QueuedPrompt {
  const queued = createQueuedPrompt({
    id,
    text,
    createdAt: `2026-08-11T00:00:0${id.length}Z`,
    requestedPlanMode,
  })
  if (!queued) throw new Error('test item must be valid')
  return queued
}

function state(...items: QueuedPrompt[]): PromptQueueState {
  return { items, steering: null }
}

function reduce(queue: PromptQueueState, action: PromptQueueAction): PromptQueueState {
  return promptQueueReducer(queue, action)
}

describe('pure prompt queue reducer', () => {
  test('normalizes enqueue text and FIFO claims each id exactly once', () => {
    const first = item('first', '  same text  ')
    const second = item('second', 'same text', true)
    let queue = reduce(EMPTY_PROMPT_QUEUE, { type: 'enqueue', item: first })
    queue = reduce(queue, { type: 'enqueue', item: second })

    expect(queue.items.map(({ id, text, requestedPlanMode }) => ({ id, text, requestedPlanMode }))).toEqual([
      { id: 'first', text: 'same text', requestedPlanMode: false },
      { id: 'second', text: 'same text', requestedPlanMode: true },
    ])

    const firstClaim = transitionPromptQueue(queue, { type: 'claim-next' })
    const secondClaim = transitionPromptQueue(firstClaim.state, { type: 'claim-next' })
    const duplicateClaim = transitionPromptQueue(secondClaim.state, {
      type: 'claim-item',
      itemId: second.id,
      expectedRevision: second.revision,
    })

    expect(firstClaim.claimed?.id).toBe('first')
    expect(secondClaim.claimed?.id).toBe('second')
    expect(duplicateClaim.claimed).toBeNull()
    expect(duplicateClaim.state).toBe(secondClaim.state)
  })

  test('can claim any selected item first but rejects stale revision and unknown id', () => {
    const one = item('one')
    const two = item('two')
    const queue = state(one, two)

    const stale = transitionPromptQueue(queue, {
      type: 'claim-item', itemId: 'two', expectedRevision: 2,
    })
    const unknown = transitionPromptQueue(queue, {
      type: 'claim-item', itemId: 'missing', expectedRevision: 1,
    })
    const selected = transitionPromptQueue(queue, {
      type: 'claim-item', itemId: 'two', expectedRevision: 1,
    })

    expect(stale).toEqual({ state: queue, claimed: null })
    expect(unknown).toEqual({ state: queue, claimed: null })
    expect(selected.claimed).toEqual(two)
    expect(selected.state.items.map((entry) => entry.id)).toEqual(['one'])
  })

  test('edit trims text, preserves id and plan snapshot, and increments revision', () => {
    const original = item('edit-me', 'old', true)
    const edited = reduce(state(original), {
      type: 'edit', itemId: original.id, expectedRevision: 1, text: '  new text  ',
    })

    expect(edited.items[0]).toEqual({
      ...original,
      text: 'new text',
      revision: 2,
    })
    expect(
      reduce(edited, {
        type: 'edit', itemId: original.id, expectedRevision: 1, text: 'stale edit',
      }),
    ).toBe(edited)
  })

  test('blank edit/save is rejected and cancelling an edit leaves queue unchanged', () => {
    const original = state(item('draft', 'original'))
    const blankSave = reduce(original, {
      type: 'edit', itemId: 'draft', expectedRevision: 1, text: ' \n ',
    })
    const localDraft = 'changed locally'
    const cancelled = original

    expect(blankSave).toBe(original)
    expect(localDraft).not.toBe(cancelled.items[0].text)
    expect(cancelled).toBe(original)
  })

  test('delete requires exact identity and only removes that queued item', () => {
    const one = item('one', 'duplicate')
    const two = item('two', 'duplicate')
    const queue = state(one, two)

    expect(reduce(queue, { type: 'delete', itemId: 'one', expectedRevision: 9 })).toBe(queue)
    expect(
      reduce(queue, { type: 'delete', itemId: 'one', expectedRevision: 1 }).items,
    ).toEqual([two])
  })

  test('drag and keyboard movement share the same reorder action', () => {
    const original = state(item('a'), item('b'), item('c'))
    const keyboardTarget = selectReorderTargetId(original, 'b', 'down')
    if (!keyboardTarget) throw new Error('expected adjacent keyboard target')

    const keyboardAction: PromptQueueAction = {
      type: 'reorder', itemId: 'b', targetId: keyboardTarget,
    }
    const dragAction: PromptQueueAction = {
      type: 'reorder', itemId: 'b', targetId: 'c',
    }

    expect(reduce(original, keyboardAction)).toEqual(reduce(original, dragAction))
    expect(reduce(original, keyboardAction).items.map((entry) => entry.id)).toEqual(['a', 'c', 'b'])
  })

  test('self, unknown and keyboard boundary reorders are referential no-ops', () => {
    const queue = state(item('a'), item('b'))
    expect(reduce(queue, { type: 'reorder', itemId: 'a', targetId: 'a' })).toBe(queue)
    expect(reduce(queue, { type: 'reorder', itemId: 'missing', targetId: 'a' })).toBe(queue)
    expect(reduce(queue, { type: 'reorder', itemId: 'a', targetId: 'missing' })).toBe(queue)
    expect(selectReorderTargetId(queue, 'a', 'up')).toBeNull()
    expect(selectReorderTargetId(queue, 'b', 'down')).toBeNull()
    expect(selectReorderTargetId(queue, 'missing', 'up')).toBeNull()
  })

  test('a steering reservation locks edit, delete, reorder and FIFO claim', () => {
    const one = item('one')
    const two = item('two')
    const queue = reduce(state(one, two), {
      type: 'begin-steering', itemId: 'two', expectedRevision: 1,
    })

    expect(queue.steering).toEqual({ itemId: 'two', revision: 1 })
    expect(reduce(queue, { type: 'edit', itemId: 'two', expectedRevision: 1, text: 'new' })).toBe(queue)
    expect(reduce(queue, { type: 'delete', itemId: 'two', expectedRevision: 1 })).toBe(queue)
    expect(reduce(queue, { type: 'reorder', itemId: 'two', targetId: 'one' })).toBe(queue)
    expect(reduce(queue, { type: 'reorder', itemId: 'one', targetId: 'two' })).toBe(queue)
    expect(transitionPromptQueue(queue, { type: 'claim-next' }).claimed).toBeNull()

    const claimed = transitionPromptQueue(queue, {
      type: 'claim-item', itemId: 'two', expectedRevision: 1,
    })
    expect(claimed.claimed).toEqual(two)
    expect(claimed.state.steering).toBeNull()
    expect(claimed.state.items).toEqual([one])
  })

  test('stale steering acknowledgements cannot unlock a different reservation', () => {
    const queue = reduce(state(item('one')), {
      type: 'begin-steering', itemId: 'one', expectedRevision: 1,
    })
    expect(
      reduce(queue, { type: 'clear-steering', itemId: 'one', expectedRevision: 2 }),
    ).toBe(queue)
    expect(
      reduce(queue, { type: 'clear-steering', itemId: 'other', expectedRevision: 1 }),
    ).toBe(queue)
    expect(
      reduce(queue, { type: 'clear-steering', itemId: 'one', expectedRevision: 1 }).steering,
    ).toBeNull()
  })
})

describe('per-session prompt queue store', () => {
  test('keeps queues across sid switches without crossing them', () => {
    let id = 0
    const store = createPromptQueueStore({
      createId: () => `id-${++id}`,
      now: () => '2026-08-11T00:00:00Z',
    })
    store.enqueue('sid-a', { text: 'a1', requestedPlanMode: false })
    store.enqueue('sid-b', { text: 'b1', requestedPlanMode: true })
    store.enqueue('sid-a', { text: 'a2', requestedPlanMode: true })

    expect(store.getSnapshot('sid-a').items.map((entry) => entry.text)).toEqual(['a1', 'a2'])
    expect(store.getSnapshot('sid-b').items.map((entry) => entry.text)).toEqual(['b1'])
    expect(store.getSnapshot('sid-missing')).toBe(EMPTY_PROMPT_QUEUE)
    expect(store.claimNext('sid-b')?.text).toBe('b1')
    expect(store.getSnapshot('sid-a').items.map((entry) => entry.text)).toEqual(['a1', 'a2'])
  })

  test('notifies only the changed sid and not for rejected operations', () => {
    const store = createPromptQueueStore({
      createId: () => 'stable-id',
      now: () => '2026-08-11T00:00:00Z',
    })
    let aNotifications = 0
    let bNotifications = 0
    const unsubscribeA = store.subscribe('sid-a', () => { aNotifications += 1 })
    const unsubscribeB = store.subscribe('sid-b', () => { bNotifications += 1 })

    const queued = store.enqueue('sid-a', { text: ' hello ', requestedPlanMode: false })
    expect(queued?.text).toBe('hello')
    expect(aNotifications).toBe(1)
    expect(bNotifications).toBe(0)
    expect(store.edit('sid-a', 'stable-id', 99, 'stale')).toBe(false)
    expect(aNotifications).toBe(1)

    unsubscribeA()
    unsubscribeB()
  })

  test('imperative claim is atomic and returns each stable id at most once', () => {
    let id = 0
    const store = createPromptQueueStore({
      createId: () => `claim-${++id}`,
      now: () => '2026-08-11T00:00:00Z',
    })
    const first = store.enqueue('sid', { text: 'same', requestedPlanMode: false })
    const second = store.enqueue('sid', { text: 'same', requestedPlanMode: false })
    if (!first || !second) throw new Error('enqueue failed')

    expect(store.claimItem('sid', second.id, second.revision)?.id).toBe(second.id)
    expect(store.claimItem('sid', second.id, second.revision)).toBeNull()
    expect(store.claimNext('sid')?.id).toBe(first.id)
    expect(store.claimNext('sid')).toBeNull()
  })

  test('pure queue modules do not import or call transcript/network senders', async () => {
    const queueSource = await Bun.file(new URL('../src/api/promptQueue.ts', import.meta.url)).text()
    const storeSource = await Bun.file(new URL('../src/promptQueueStore.ts', import.meta.url)).text()
    const implementation = `${queueSource}\n${storeSource}`

    expect(implementation).not.toContain('sendUserMessage')
    expect(implementation).not.toContain("./api/client")
    expect(implementation).not.toContain('connectSSE')
    expect(implementation).not.toContain('executePlan')
  })
})
