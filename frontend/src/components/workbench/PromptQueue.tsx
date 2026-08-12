import { useEffect, useRef, useState, type KeyboardEvent, type PointerEvent } from 'react'
import type { PromptQueueState, QueuedPrompt } from '../../api/promptQueue'
import {
  IconCheck,
  IconCornerArrow,
  IconEdit,
  IconGrip,
  IconTrash,
  IconX,
} from '../icons'

export interface PromptQueueProps {
  queue: PromptQueueState
  running: boolean
  stopping: boolean
  disabled: boolean
  error?: string | null
  onReorder: (itemId: string, targetId: string) => boolean
  onEdit: (
    itemId: string,
    expectedRevision: number,
    text: string,
  ) => boolean
  onDelete: (itemId: string, expectedRevision: number) => boolean
  onActivate: (item: QueuedPrompt) => void
}

export interface QueuePointerDragSnapshot {
  itemId: string
  pointerId: number
}

export interface QueuePointerDragController {
  begin(itemId: string, pointerId: number): void
  current(): QueuePointerDragSnapshot | null
  finish(
    pointerId: number,
    fallbackTargetId: string | null,
    reorder: (itemId: string, targetId: string) => boolean,
  ): { handled: boolean; reordered: boolean }
  cancel(pointerId: number): boolean
}

/** One captured pointer gesture commits at most one stable-id reorder on release. */
export function createQueuePointerDragController(): QueuePointerDragController {
  let drag: QueuePointerDragSnapshot | null = null

  return {
    begin(itemId, pointerId) {
      drag = { itemId, pointerId }
    },
    current() {
      return drag
    },
    finish(pointerId, fallbackTargetId, reorder) {
      if (!drag || drag.pointerId !== pointerId) {
        return { handled: false, reordered: false }
      }
      try {
        const reordered = !!fallbackTargetId && fallbackTargetId !== drag.itemId
          ? reorder(drag.itemId, fallbackTargetId)
          : false
        return {
          handled: true,
          reordered,
        }
      } finally {
        drag = null
      }
    },
    cancel(pointerId) {
      if (!drag || drag.pointerId !== pointerId) return false
      drag = null
      return true
    },
  }
}

/** Resolve the stable row id at a pointer coordinate, including nested/multiline children. */
export function queueItemIdAtPoint(
  source: Pick<Document, 'elementFromPoint'>,
  clientX: number,
  clientY: number,
): string | null {
  return source
    .elementFromPoint(clientX, clientY)
    ?.closest<HTMLElement>('[data-queue-item-id]')
    ?.dataset.queueItemId ?? null
}

/** Keyboard targeting shares the same id-to-id reducer path as pointer drag. */
export function queueKeyboardTarget(
  items: readonly QueuedPrompt[],
  itemId: string,
  key: string,
): string | null {
  if (key !== 'ArrowUp' && key !== 'ArrowDown') return null
  const index = items.findIndex((item) => item.id === itemId)
  if (index < 0) return null
  const targetIndex = index + (key === 'ArrowUp' ? -1 : 1)
  return items[targetIndex]?.id ?? null
}

function reorderAnnouncement(
  items: readonly QueuedPrompt[],
  itemId: string,
  targetId: string,
): string {
  const from = items.findIndex((item) => item.id === itemId)
  const to = items.findIndex((item) => item.id === targetId)
  return from >= 0 && to >= 0 ? `已将第 ${from + 1} 条移到第 ${to + 1} 条` : ''
}

export default function PromptQueue({
  queue,
  running,
  stopping,
  disabled,
  error,
  onReorder,
  onEdit,
  onDelete,
  onActivate,
}: PromptQueueProps) {
  const [draggedId, setDraggedId] = useState<string | null>(null)
  const pointerDragRef = useRef(createQueuePointerDragController())
  const [editing, setEditing] = useState<{
    itemId: string
    expectedRevision: number
    draft: string
  } | null>(null)
  const [editError, setEditError] = useState<string | null>(null)
  const [announcement, setAnnouncement] = useState('')

  useEffect(() => {
    if (!editing) return
    const current = queue.items.find((item) => item.id === editing.itemId)
    if (
      !current ||
      current.revision !== editing.expectedRevision ||
      queue.steering?.itemId === editing.itemId
    ) {
      setEditing(null)
      setEditError(null)
    }
  }, [editing, queue.items, queue.steering])

  const applyReorder = (itemId: string, targetId: string) => {
    const message = reorderAnnouncement(queue.items, itemId, targetId)
    if (!message || !onReorder(itemId, targetId)) return false
    setAnnouncement(message)
    return true
  }

  const handleGripKeyDown = (
    event: KeyboardEvent<HTMLButtonElement>,
    item: QueuedPrompt,
  ) => {
    const targetId = queueKeyboardTarget(queue.items, item.id, event.key)
    if (!targetId) return
    event.preventDefault()
    applyReorder(item.id, targetId)
  }

  const handlePointerDown = (
    event: PointerEvent<HTMLButtonElement>,
    itemId: string,
  ) => {
    if (disabled || event.button !== 0) return
    pointerDragRef.current.begin(itemId, event.pointerId)
    setDraggedId(itemId)
    event.currentTarget.focus()
    event.currentTarget.setPointerCapture(event.pointerId)
  }

  const handlePointerMove = (event: PointerEvent<HTMLButtonElement>) => {
    const drag = pointerDragRef.current.current()
    if (!drag || drag.pointerId !== event.pointerId) return
    // Keep the browser from selecting/scrolling while captured. Reorder is
    // committed once on pointerup so a hover move and the release fallback can
    // never replay the same non-idempotent id-to-id transition.
    event.preventDefault()
  }

  const finishPointerDrag = (event: PointerEvent<HTMLButtonElement>) => {
    const drag = pointerDragRef.current.current()
    if (!drag || drag.pointerId !== event.pointerId) return
    try {
      if (event.type === 'pointerup') {
        // Resolve the release row once. This also works when a WebView
        // coalesces the gesture and delivers no intermediate pointermove.
        const targetId = queueItemIdAtPoint(document, event.clientX, event.clientY)
        pointerDragRef.current.finish(event.pointerId, targetId, applyReorder)
      } else {
        pointerDragRef.current.cancel(event.pointerId)
      }
    } finally {
      setDraggedId(null)
      if (event.currentTarget.hasPointerCapture(event.pointerId)) {
        event.currentTarget.releasePointerCapture(event.pointerId)
      }
    }
  }

  const beginEditing = (item: QueuedPrompt) => {
    setEditing({
      itemId: item.id,
      expectedRevision: item.revision,
      draft: item.text,
    })
    setEditError(null)
  }

  const cancelEditing = () => {
    setEditing(null)
    setEditError(null)
  }

  const saveEditing = () => {
    if (!editing) return
    const text = editing.draft.trim()
    if (!text) {
      setEditError('消息不能为空')
      return
    }
    const current = queue.items.find((item) => item.id === editing.itemId)
    if (
      current?.revision === editing.expectedRevision &&
      current.text === text
    ) {
      cancelEditing()
      return
    }
    if (!onEdit(editing.itemId, editing.expectedRevision, editing.draft)) {
      setEditError('消息已变化，请重新编辑')
      return
    }
    cancelEditing()
  }

  if (queue.items.length === 0 && !error) return null

  return (
    <section
      data-prompt-queue-interaction
      aria-label="待发送消息队列"
      className="mb-2 overflow-hidden rounded-lg border border-border bg-surface"
    >
      {queue.items.length > 0 && (
        <div className="flex items-center justify-between border-b border-border px-3 py-1.5">
          <span className="text-[11px] font-medium text-ink2">
            待发送 · {queue.items.length}
          </span>
          <span className="text-[10px] text-ink3">拖动或使用 ↑↓ 调整顺序</span>
        </div>
      )}

      <ol className="max-h-56 divide-y divide-border overflow-y-auto">
        {queue.items.map((item, index) => {
          const isSteering =
            queue.steering?.itemId === item.id &&
            queue.steering.revision === item.revision
          const isEditing = editing?.itemId === item.id
          const actionDisabled = disabled || stopping || isSteering || isEditing
          const actionLabel = running ? '调整方向' : '立即执行'
          const actionTitle = running
            ? '调整方向会取消当前运行并重新启动（不会注入当前 provider 请求）'
            : '立即从队列中执行这条消息'

          return (
            <li
              key={item.id}
              data-queue-item-id={item.id}
              className={`flex items-start gap-2 px-2 py-2 ${
                draggedId === item.id ? 'opacity-60' : ''
              }`}
            >
              <button
                type="button"
                draggable={false}
                disabled={disabled || isSteering}
                onPointerDown={(event) => handlePointerDown(event, item.id)}
                onPointerMove={handlePointerMove}
                onPointerUp={finishPointerDrag}
                onPointerCancel={finishPointerDrag}
                onLostPointerCapture={finishPointerDrag}
                onKeyDown={(event) => handleGripKeyDown(event, item)}
                aria-grabbed={draggedId === item.id}
                className={`mt-0.5 shrink-0 touch-none rounded p-1 text-ink3 hover:bg-surface2 hover:text-ink disabled:cursor-not-allowed disabled:opacity-40 ${
                  draggedId === item.id ? 'cursor-grabbing' : 'cursor-grab'
                }`}
                aria-label={`拖动第 ${index + 1} 条消息；按上、下方向键调整顺序`}
                title="拖动排序；聚焦后按 ↑ 或 ↓"
              >
                <IconGrip width={14} height={14} />
              </button>

              {isEditing && editing ? (
                <div className="min-w-0 flex-1">
                  <textarea
                    autoFocus
                    rows={2}
                    value={editing.draft}
                    disabled={disabled || isSteering}
                    aria-label="编辑队列消息"
                    aria-invalid={!!editError}
                    onChange={(event) => {
                      setEditing({ ...editing, draft: event.target.value })
                      if (editError) setEditError(null)
                    }}
                    onKeyDown={(event) => {
                      if (event.key === 'Escape') {
                        event.preventDefault()
                        cancelEditing()
                      }
                    }}
                    className="w-full resize-y rounded-md border border-border bg-bg px-2 py-1.5 text-[12px] leading-relaxed text-ink outline-none focus:border-brand"
                  />
                  <div className="mt-1 flex items-center gap-1">
                    <button
                      type="button"
                      className="btn-primary px-2 py-1 text-[11px]"
                      disabled={disabled || isSteering || !editing.draft.trim()}
                      onClick={saveEditing}
                      aria-label="保存队列消息"
                    >
                      <IconCheck width={12} height={12} /> 保存
                    </button>
                    <button
                      type="button"
                      className="btn-ghost px-2 py-1 text-[11px]"
                      onClick={cancelEditing}
                      aria-label="取消编辑队列消息"
                    >
                      <IconX width={12} height={12} /> 取消
                    </button>
                    {editError && (
                      <span role="alert" className="ml-1 text-[11px] text-danger">
                        {editError}
                      </span>
                    )}
                  </div>
                </div>
              ) : (
                <p className="min-w-0 flex-1 whitespace-pre-wrap break-words py-0.5 text-[12px] leading-relaxed text-ink">
                  {item.text}
                </p>
              )}

              <div className="flex shrink-0 items-center gap-0.5">
                <button
                  type="button"
                  className="btn-ghost px-1.5 py-1 text-[11px] disabled:opacity-40"
                  disabled={actionDisabled}
                  onClick={() => onActivate(item)}
                  title={isSteering ? '正在取消当前运行并准备重新启动' : actionTitle}
                  aria-label={isSteering ? '正在调整方向' : `${actionLabel}：${item.text}`}
                >
                  <IconCornerArrow width={13} height={13} />
                  {isSteering ? '调整中…' : actionLabel}
                </button>
                <button
                  type="button"
                  className="rounded p-1.5 text-ink3 hover:bg-surface2 hover:text-ink disabled:opacity-40"
                  disabled={disabled || isSteering || isEditing}
                  onClick={() => beginEditing(item)}
                  title="编辑"
                  aria-label={`编辑队列消息：${item.text}`}
                >
                  <IconEdit width={13} height={13} />
                </button>
                <button
                  type="button"
                  className="rounded p-1.5 text-ink3 hover:bg-dangerSoft hover:text-danger disabled:opacity-40"
                  disabled={disabled || isSteering || isEditing}
                  onClick={() => onDelete(item.id, item.revision)}
                  title="删除"
                  aria-label={`删除队列消息：${item.text}`}
                >
                  <IconTrash width={13} height={13} />
                </button>
              </div>
            </li>
          )
        })}
      </ol>

      {error && (
        <p role="alert" className="border-t border-danger/30 bg-dangerSoft px-3 py-1.5 text-[11px] text-danger">
          {error}
        </p>
      )}
      <p className="sr-only" aria-live="polite" aria-atomic="true">
        {announcement}
      </p>
    </section>
  )
}
