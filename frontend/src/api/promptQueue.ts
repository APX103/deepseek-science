/**
 * A prompt that exists only in the frontend process until it is claimed.
 *
 * `requestedPlanMode` is the user's enqueue-time preference. Whether the
 * eventual request executes an existing plan is deliberately resolved later,
 * when the item is claimed against the latest session state.
 */
export interface QueuedPrompt {
  id: string
  revision: number
  text: string
  createdAt: string
  requestedPlanMode: boolean
}

export interface PromptQueueItemRef {
  itemId: string
  revision: number
}

export interface PromptQueueState {
  readonly items: readonly QueuedPrompt[]
  /** Exact item identity reserved by an in-flight cancel-and-restart steer. */
  readonly steering: PromptQueueItemRef | null
}

export const EMPTY_PROMPT_QUEUE: PromptQueueState = Object.freeze({
  items: Object.freeze([]) as readonly QueuedPrompt[],
  steering: null,
})

export type PromptQueueAction =
  | { type: 'enqueue'; item: QueuedPrompt }
  | { type: 'edit'; itemId: string; expectedRevision: number; text: string }
  | { type: 'delete'; itemId: string; expectedRevision: number }
  | { type: 'reorder'; itemId: string; targetId: string }
  | { type: 'claim-next' }
  | { type: 'claim-item'; itemId: string; expectedRevision: number }
  | { type: 'begin-steering'; itemId: string; expectedRevision: number }
  | { type: 'clear-steering'; itemId: string; expectedRevision: number }

export interface PromptQueueTransition {
  state: PromptQueueState
  /** Non-null only for a successful, removing claim. */
  claimed: QueuedPrompt | null
}

export interface CreateQueuedPromptInput {
  id: string
  text: string
  createdAt: string
  requestedPlanMode: boolean
}

/** Validate and normalize an enqueue payload without generating identity. */
export function createQueuedPrompt(input: CreateQueuedPromptInput): QueuedPrompt | null {
  const id = input.id.trim()
  const text = input.text.trim()
  if (!id || !text || !input.createdAt) return null

  return {
    id,
    revision: 1,
    text,
    createdAt: input.createdAt,
    requestedPlanMode: input.requestedPlanMode,
  }
}

export function selectQueuedPrompt(
  state: PromptQueueState,
  itemId: string,
): QueuedPrompt | undefined {
  return state.items.find((item) => item.id === itemId)
}

export function selectIsSteeringItem(state: PromptQueueState, itemId: string): boolean {
  return state.steering?.itemId === itemId
}

/** Resolve an adjacent target; both keyboard and drag then dispatch `reorder`. */
export function selectReorderTargetId(
  state: PromptQueueState,
  itemId: string,
  direction: 'up' | 'down',
): string | null {
  const index = state.items.findIndex((item) => item.id === itemId)
  if (index < 0) return null
  const targetIndex = index + (direction === 'up' ? -1 : 1)
  return state.items[targetIndex]?.id ?? null
}

function sameItemRef(item: QueuedPrompt, itemId: string, revision: number): boolean {
  return item.id === itemId && item.revision === revision
}

function transitionClaim(
  state: PromptQueueState,
  itemId?: string,
  expectedRevision?: number,
): PromptQueueTransition {
  if (state.items.length === 0) return { state, claimed: null }

  // A steering reservation may only be consumed by its exact id + revision.
  if (state.steering) {
    if (
      itemId !== state.steering.itemId ||
      expectedRevision !== state.steering.revision
    ) {
      return { state, claimed: null }
    }
  }

  const index = itemId === undefined
    ? 0
    : state.items.findIndex(
        (item) => sameItemRef(item, itemId, expectedRevision ?? Number.NaN),
      )
  if (index < 0) return { state, claimed: null }

  const claimed = state.items[index]
  const items = [...state.items]
  items.splice(index, 1)
  return {
    state: {
      items,
      steering: state.steering?.itemId === claimed.id ? null : state.steering,
    },
    claimed,
  }
}

/**
 * Apply one queue operation. Claims expose the removed value alongside state,
 * so an imperative caller can atomically decide whether it owns a send.
 */
export function transitionPromptQueue(
  state: PromptQueueState,
  action: PromptQueueAction,
): PromptQueueTransition {
  switch (action.type) {
    case 'enqueue': {
      const text = action.item.text.trim()
      if (
        !action.item.id ||
        !text ||
        !action.item.createdAt ||
        !Number.isSafeInteger(action.item.revision) ||
        action.item.revision < 1 ||
        state.items.some((item) => item.id === action.item.id)
      ) {
        return { state, claimed: null }
      }
      return {
        state: {
          ...state,
          items: [...state.items, { ...action.item, text }],
        },
        claimed: null,
      }
    }

    case 'edit': {
      if (selectIsSteeringItem(state, action.itemId)) return { state, claimed: null }
      const index = state.items.findIndex((item) =>
        sameItemRef(item, action.itemId, action.expectedRevision),
      )
      const text = action.text.trim()
      if (index < 0 || !text || state.items[index].text === text) {
        return { state, claimed: null }
      }
      const current = state.items[index]
      if (!Number.isSafeInteger(current.revision + 1)) return { state, claimed: null }
      const items = [...state.items]
      items[index] = { ...current, text, revision: current.revision + 1 }
      return { state: { ...state, items }, claimed: null }
    }

    case 'delete': {
      if (selectIsSteeringItem(state, action.itemId)) return { state, claimed: null }
      const index = state.items.findIndex((item) =>
        sameItemRef(item, action.itemId, action.expectedRevision),
      )
      if (index < 0) return { state, claimed: null }
      const items = [...state.items]
      items.splice(index, 1)
      return { state: { ...state, items }, claimed: null }
    }

    case 'reorder': {
      if (
        action.itemId === action.targetId ||
        selectIsSteeringItem(state, action.itemId) ||
        selectIsSteeringItem(state, action.targetId)
      ) {
        return { state, claimed: null }
      }
      const fromIndex = state.items.findIndex((item) => item.id === action.itemId)
      const targetIndex = state.items.findIndex((item) => item.id === action.targetId)
      if (fromIndex < 0 || targetIndex < 0) return { state, claimed: null }

      const items = [...state.items]
      const [moved] = items.splice(fromIndex, 1)
      // targetIndex is the desired index in the original ordering. This is the
      // usual arrayMove behavior and makes adjacent keyboard movement exact.
      items.splice(targetIndex, 0, moved)
      return { state: { ...state, items }, claimed: null }
    }

    case 'claim-next':
      return transitionClaim(state)

    case 'claim-item':
      return transitionClaim(state, action.itemId, action.expectedRevision)

    case 'begin-steering': {
      if (state.steering) return { state, claimed: null }
      const item = state.items.find((candidate) =>
        sameItemRef(candidate, action.itemId, action.expectedRevision),
      )
      if (!item) return { state, claimed: null }
      return {
        state: {
          ...state,
          steering: { itemId: item.id, revision: item.revision },
        },
        claimed: null,
      }
    }

    case 'clear-steering': {
      if (
        state.steering?.itemId !== action.itemId ||
        state.steering.revision !== action.expectedRevision
      ) {
        return { state, claimed: null }
      }
      return { state: { ...state, steering: null }, claimed: null }
    }
  }
}

/** Conventional reducer adapter for React callers that do not need claim data. */
export function promptQueueReducer(
  state: PromptQueueState,
  action: PromptQueueAction,
): PromptQueueState {
  return transitionPromptQueue(state, action).state
}
