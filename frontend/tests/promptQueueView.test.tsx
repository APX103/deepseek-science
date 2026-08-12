import { describe, expect, test } from 'bun:test'
import { readFileSync } from 'node:fs'
import { renderToStaticMarkup } from 'react-dom/server'
import type { PromptQueueState, QueuedPrompt } from '../src/api/promptQueue'
import {
  createPromptQueueStore,
  type PromptQueueStore,
} from '../src/promptQueueStore'
import { Composer, shouldSubmitComposerKey, submitComposerDraft } from '../src/components/workbench/ChatArea'
import PromptQueue, {
  createQueuePointerDragController,
  queueItemIdAtPoint,
  queueKeyboardTarget,
} from '../src/components/workbench/PromptQueue'

function item(id: string, text = `message:${id}`): QueuedPrompt {
  return {
    id,
    revision: 1,
    text,
    createdAt: '2026-08-11T00:00:00Z',
    requestedPlanMode: false,
  }
}

function queue(
  items: readonly QueuedPrompt[],
  steering: PromptQueueState['steering'] = null,
): PromptQueueState {
  return { items, steering }
}

const noOpHandlers = {
  onReorder: () => true,
  onEdit: () => true,
  onDelete: () => true,
  onActivate: () => undefined,
}

function renderQueue(state: PromptQueueState, running: boolean, error?: string) {
  return renderToStaticMarkup(
    <PromptQueue
      queue={state}
      running={running}
      stopping={false}
      disabled={false}
      error={error}
      {...noOpHandlers}
    />,
  )
}

function filledPointerStore(): PromptQueueStore {
  let sequence = 0
  const store = createPromptQueueStore({
    createId: () => ['a', 'b', 'c'][sequence++] ?? `extra-${sequence}`,
    now: () => '2026-08-11T00:00:00Z',
  })
  store.enqueue('sid', { text: 'short A', requestedPlanMode: false })
  store.enqueue('sid', { text: 'multiline\nB\nrow', requestedPlanMode: false })
  store.enqueue('sid', { text: 'short C', requestedPlanMode: false })
  return store
}

function multilineHitTestSource(): Pick<Document, 'elementFromPoint'> {
  return {
    elementFromPoint(_x: number, y: number) {
      const targetId = y >= 40 && y < 120 ? 'b' : y >= 120 ? 'c' : 'a'
      // Model a nested text child: hit-testing must climb to the stable row id.
      return {
        closest: (selector: string) => selector === '[data-queue-item-id]'
          ? { dataset: { queueItemId: targetId } }
          : null,
      } as unknown as Element
    },
  }
}

describe('pointer-capture queue reorder lifecycle', () => {
  test('pointermove and pointerup at the same multiline target commit exactly once on release', () => {
    const store = filledPointerStore()
    const drag = createQueuePointerDragController()
    const hitTest = multilineHitTestSource()
    const calls: string[] = []
    const reorder = (itemId: string, targetId: string) => {
      calls.push(`${itemId}->${targetId}`)
      return store.reorder('sid', itemId, targetId)
    }

    // Real captured lifecycle: pointerdown(a) -> pointermove(row B Y) ->
    // pointerup(the same row B Y). Hover does not mutate the queue; release is
    // the gesture's single commit point.
    drag.begin('a', 17)
    expect(drag.current()).toEqual({ itemId: 'a', pointerId: 17 })
    const moveTarget = queueItemIdAtPoint(hitTest, 8, 80)
    expect(moveTarget).toBe('b')
    expect(store.getSnapshot('sid').items.map((entry) => entry.id)).toEqual(['a', 'b', 'c'])

    const upTarget = queueItemIdAtPoint(hitTest, 8, 80)
    expect(upTarget).toBe('b')
    expect(drag.finish(17, upTarget, reorder)).toEqual({ handled: true, reordered: true })
    expect(calls).toEqual(['a->b'])
    expect(store.getSnapshot('sid').items.map((entry) => entry.id)).toEqual(['b', 'a', 'c'])
    expect(drag.current()).toBeNull()
  })

  test('pointerup commits when the WebView coalesces all pointermove events', () => {
    const store = filledPointerStore()
    const drag = createQueuePointerDragController()
    const target = queueItemIdAtPoint(multilineHitTestSource(), 8, 80)
    const calls: string[] = []

    drag.begin('a', 23)
    const finished = drag.finish(23, target, (itemId, targetId) => {
      calls.push(`${itemId}->${targetId}`)
      return store.reorder('sid', itemId, targetId)
    })

    expect(finished).toEqual({ handled: true, reordered: true })
    expect(calls).toEqual(['a->b'])
    expect(store.getSnapshot('sid').items.map((entry) => entry.id)).toEqual(['b', 'a', 'c'])
    expect(drag.current()).toBeNull()
  })

  test('pointer cancel clears only the matching captured gesture', () => {
    const drag = createQueuePointerDragController()
    drag.begin('a', 31)
    expect(drag.cancel(99)).toBe(false)
    expect(drag.current()).toEqual({ itemId: 'a', pointerId: 31 })
    expect(drag.cancel(31)).toBe(true)
    expect(drag.current()).toBeNull()
  })
})

describe('prompt queue structure and accessibility', () => {
  test('renders stable queue ids, full multiline text, draggable grips, and live announcements', () => {
    const html = renderQueue(
      queue([
        item('stable-a', '第一行\n第二行 **Markdown** 🚀'),
        item('stable-b'),
      ]),
      true,
    )

    expect(html).toContain('aria-label="待发送消息队列"')
    expect(html).toContain('data-queue-item-id="stable-a"')
    expect(html).toContain('data-queue-item-id="stable-b"')
    expect(html).toContain('draggable="false"')
    expect(html).toContain('aria-grabbed="false"')
    expect(html).toContain('按上、下方向键调整顺序')
    expect(html).toContain('aria-live="polite"')
    expect(html).toContain('第一行\n第二行 **Markdown** 🚀')
    expect(html).toContain('编辑队列消息：')
    expect(html).toContain('删除队列消息：')
  })

  test('uses honest cancel-and-restart steering copy while running and immediate-run copy while idle', () => {
    const running = renderQueue(queue([item('a')]), true)
    expect(running).toContain('调整方向')
    expect(running).toContain('调整方向会取消当前运行并重新启动')
    expect(running).toContain('不会注入当前 provider 请求')
    expect(running).not.toContain('立即执行：')

    const idle = renderQueue(queue([item('a')]), false)
    expect(idle).toContain('立即执行')
    expect(idle).toContain('立即从队列中执行这条消息')
    expect(idle).not.toContain('调整方向会取消当前运行并重新启动')
  })

  test('locks every identity-changing control for the exact steering item and exposes failures', () => {
    const html = renderQueue(
      queue([item('locked')], { itemId: 'locked', revision: 1 }),
      true,
      '调整方向失败：offline',
    )

    expect(html).toContain('draggable="false"')
    expect(html).toContain('调整中…')
    expect(html).toContain('aria-label="正在调整方向"')
    expect(html.match(/disabled=""/g)?.length).toBeGreaterThanOrEqual(4)
    expect(html).toContain('role="alert"')
    expect(html).toContain('调整方向失败：offline')
  })

  test('keeps an existing queue visible but read-only while unavailable', () => {
    const html = renderToStaticMarkup(
      <PromptQueue
        queue={queue([item('offline')])}
        running={false}
        stopping={false}
        disabled
        {...noOpHandlers}
      />,
    )
    expect(html).toContain('message:offline')
    expect(html).toContain('draggable="false"')
    expect(html.match(/disabled=""/g)?.length).toBeGreaterThanOrEqual(4)
  })

  test('keeps pointer drag and keyboard moves on the same id-target reducer path', () => {
    const createStore = (): PromptQueueStore => {
      let sequence = 0
      return createPromptQueueStore({
        createId: () => ['a', 'b', 'c'][sequence++] ?? `extra-${sequence}`,
        now: () => '2026-08-11T00:00:00Z',
      })
    }
    const fill = (store: PromptQueueStore) => {
      store.enqueue('sid', { text: 'A', requestedPlanMode: false })
      store.enqueue('sid', { text: 'B', requestedPlanMode: false })
      store.enqueue('sid', { text: 'C', requestedPlanMode: false })
    }
    const dragStore = createStore()
    const keyboardStore = createStore()
    fill(dragStore)
    fill(keyboardStore)

    expect(queueKeyboardTarget(keyboardStore.getSnapshot('sid').items, 'b', 'ArrowUp')).toBe('a')
    expect(queueKeyboardTarget(keyboardStore.getSnapshot('sid').items, 'a', 'ArrowUp')).toBeNull()
    expect(queueKeyboardTarget(keyboardStore.getSnapshot('sid').items, 'b', 'Enter')).toBeNull()

    dragStore.reorder('sid', 'b', 'a')
    const keyboardTarget = queueKeyboardTarget(
      keyboardStore.getSnapshot('sid').items,
      'b',
      'ArrowUp',
    )
    expect(keyboardTarget).not.toBeNull()
    keyboardStore.reorder('sid', 'b', keyboardTarget!)
    expect(keyboardStore.getSnapshot('sid').items).toEqual(dragStore.getSnapshot('sid').items)
  })

  test('keeps queue interaction surfaces outside Tauri window drag regions and before Composer', () => {
    const promptQueueSource = readFileSync(
      new URL('../src/components/workbench/PromptQueue.tsx', import.meta.url),
      'utf8',
    )
    const chatAreaSource = readFileSync(
      new URL('../src/components/workbench/ChatArea.tsx', import.meta.url),
      'utf8',
    )
    const workbenchSource = readFileSync(
      new URL('../src/pages/WorkbenchPage.tsx', import.meta.url),
      'utf8',
    )

    expect(promptQueueSource).toContain('data-prompt-queue-interaction')
    expect(promptQueueSource).not.toContain('data-tauri-drag-region')
    expect(promptQueueSource).toContain('setPointerCapture(event.pointerId)')
    expect(promptQueueSource).toContain(
      'queueItemIdAtPoint(document, event.clientX, event.clientY)',
    )
    expect(promptQueueSource).toContain('committed once on pointerup')
    expect(promptQueueSource).toContain('key={item.id}')
    expect(promptQueueSource).toContain('max-h-56')
    expect(promptQueueSource).toContain("event.key === 'Escape'")
    expect(promptQueueSource).toContain('onClick={saveEditing}')
    expect(promptQueueSource).toContain('onClick={cancelEditing}')
    expect(chatAreaSource.match(/<PromptQueue[\s\S]*?\/>\s*<Composer/g)).toHaveLength(2)
    expect(workbenchSource).toContain('const promptQueue = usePromptQueue(sid)')
    expect(workbenchSource).toContain('coordinateQueuedPromptSteer(targetSid, live.runId')
    expect(workbenchSource).toContain('beginStreamStop(sessionId, runId)')
    expect(workbenchSource).toContain('queueError={queueErrors[sid] ?? null}')
    expect(workbenchSource).toContain('const [planModes, setPlanModes]')
    expect(workbenchSource).toContain('const [approvingPlans, setApprovingPlans]')
    expect(workbenchSource).toContain('const targetSid = sid')
    expect(workbenchSource).toContain('stream && (stream.running || stream.kind !== null)')
    expect(chatAreaSource).toContain('<p role="alert" className="mt-2 text-[11px] text-danger">')
  })
})

describe('running composer queue behavior', () => {
  test('submits and clears non-empty input without a running-state gate', () => {
    const calls: string[] = []
    expect(submitComposerDraft('  queued text  ', true, (text) => calls.push(`send:${text}`), () => calls.push('clear'))).toBe(true)
    expect(calls).toEqual(['send:  queued text  ', 'clear'])

    expect(submitComposerDraft('   ', true, () => calls.push('unexpected'), () => calls.push('unexpected-clear'))).toBe(false)
    expect(submitComposerDraft('offline', false, () => calls.push('unexpected'), () => calls.push('unexpected-clear'))).toBe(false)
    expect(calls).toEqual(['send:  queued text  ', 'clear'])
  })

  test('preserves IME Enter and Shift+Enter while accepting plain Enter', () => {
    expect(shouldSubmitComposerKey({ key: 'Enter', shiftKey: false, isComposing: false })).toBe(true)
    expect(shouldSubmitComposerKey({ key: 'Enter', shiftKey: true, isComposing: false })).toBe(false)
    expect(shouldSubmitComposerKey({ key: 'Enter', shiftKey: false, isComposing: true })).toBe(false)
    expect(shouldSubmitComposerKey({ key: 'a', shiftKey: false, isComposing: false })).toBe(false)
  })

  test('leaves textarea, Plan, and Stop usable during a run', () => {
    const html = renderToStaticMarkup(
      <Composer
        ready
        running
        stopping={false}
        planMode={false}
        onPlanModeChange={() => undefined}
        onSend={() => undefined}
        onStop={() => undefined}
      />,
    )

    const textarea = html.match(/<textarea[\s\S]*?<\/textarea>/)?.[0]
    expect(textarea).toBeDefined()
    expect(textarea).not.toContain(' disabled=""')
    expect(textarea).toContain('按 Enter 加入队列')
    expect(html).toContain('title="仅影响新加入队列的消息"')
    expect(html).toContain('aria-label="停止当前运行"')
    expect(html).not.toContain('aria-label="发送消息"')
  })

  test('still disables new input when the backend is unavailable', () => {
    const html = renderToStaticMarkup(
      <Composer
        ready={false}
        running={false}
        stopping={false}
        planMode={false}
        onPlanModeChange={() => undefined}
        onSend={() => undefined}
        onStop={() => undefined}
      />,
    )
    const textarea = html.match(/<textarea[\s\S]*?<\/textarea>/)?.[0]
    expect(textarea).toContain(' disabled=""')
    expect(html).toContain('后端未连接…')
    expect(html).toContain('aria-label="发送消息"')
  })
})
