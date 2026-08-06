// 记忆面板：Claim Store 治理 UI（列表 / 审批 / 搜索 / 显式 remember / 时间线）。
// 仿 SettingsModal.McpSection 的列表+操作模式。
import { useEffect, useState } from 'react'
import {
  approveMemory,
  createMemory,
  deleteMemory,
  editMemory,
  getMemoryHistory,
  listMemories,
  rejectMemory,
} from '../api/client'
import type { Memory, MemoryEvent } from '../types'
import { IconPlus, IconSearch } from './icons'

type StatusFilter = 'all' | 'active' | 'candidate' | 'superseded' | 'deleted'

const STATUS_META: Record<string, { label: string; cls: string }> = {
  active: { label: '生效', cls: 'bg-success/10 text-success' },
  candidate: { label: '待审', cls: 'bg-amber-400/15 text-amber-600 dark:text-amber-400' },
  superseded: { label: '已替代', cls: 'bg-ink3/15 text-ink3' },
  expired: { label: '已过期', cls: 'bg-ink3/15 text-ink3' },
  deleted: { label: '已删除', cls: 'bg-danger/10 text-danger' },
}

const TYPE_LABEL: Record<string, string> = {
  fact: '事实',
  preference: '偏好',
  decision: '决策',
  procedure: '步骤',
  repo: '仓库',
  note: '笔记',
}

export default function MemoryPanel() {
  const [memories, setMemories] = useState<Memory[] | null>(null)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [statusFilter, setStatusFilter] = useState<StatusFilter>('active')
  const [q, setQ] = useState('')
  // 显式 remember 输入。
  const [newBody, setNewBody] = useState('')
  const [newType, setNewType] = useState('note')
  const [creating, setCreating] = useState(false)
  const [createError, setCreateError] = useState<string | null>(null)
  // 展开的记忆（看时间线）。
  const [expanded, setExpanded] = useState<string | null>(null)
  const [events, setEvents] = useState<MemoryEvent[] | null>(null)

  const load = async () => {
    setMemories(null)
    setLoadError(null)
    try {
      const status = statusFilter === 'all' ? undefined : statusFilter
      setMemories(await listMemories({ status }))
    } catch (error) {
      setLoadError(error instanceof Error ? error.message : String(error))
    }
  }

  useEffect(() => {
    void load()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [statusFilter])

  const filtered = (memories ?? []).filter((m) =>
    m.body.toLowerCase().includes(q.trim().toLowerCase()),
  )

  const handleCreate = async () => {
    if (!newBody.trim()) return
    setCreating(true)
    setCreateError(null)
    try {
      await createMemory({ body: newBody.trim(), claim_type: newType, scope: 'profile' })
      setNewBody('')
      await load()
    } catch (error) {
      setCreateError(error instanceof Error ? error.message : String(error))
    } finally {
      setCreating(false)
    }
  }

  const handleApprove = async (id: string) => {
    try {
      await approveMemory(id)
      await load()
    } catch (e) {
      setLoadError(e instanceof Error ? e.message : String(e))
    }
  }

  const handleReject = async (id: string) => {
    try {
      await rejectMemory(id)
      await load()
    } catch (e) {
      setLoadError(e instanceof Error ? e.message : String(e))
    }
  }

  const handleDelete = async (id: string) => {
    if (!window.confirm('软删除这条记忆？保留审计记录，可从"已删除"筛选查看。')) return
    try {
      await deleteMemory(id)
      await load()
    } catch (e) {
      setLoadError(e instanceof Error ? e.message : String(e))
    }
  }

  const handleEdit = async (m: Memory) => {
    const next = window.prompt('编辑记忆内容：', m.body)
    if (next === null || next === m.body) return
    try {
      await editMemory(m.id, next)
      await load()
    } catch (e) {
      setLoadError(e instanceof Error ? e.message : String(e))
    }
  }

  const toggleHistory = async (id: string) => {
    if (expanded === id) {
      setExpanded(null)
      setEvents(null)
      return
    }
    setExpanded(id)
    setEvents(null)
    try {
      setEvents(await getMemoryHistory(id))
    } catch (e) {
      setEvents([])
      setLoadError(e instanceof Error ? e.message : String(e))
    }
  }

  return (
    <div className="flex min-w-0 flex-1 flex-col">
      {/* 顶部：状态筛选 + 搜索 */}
      <div className="flex items-center gap-2 border-b border-border px-4 py-2.5">
        <select
          value={statusFilter}
          onChange={(e) => setStatusFilter(e.target.value as StatusFilter)}
          className="input h-7 w-28 text-[12px]"
        >
          <option value="active">生效中</option>
          <option value="candidate">待审批</option>
          <option value="superseded">已替代</option>
          <option value="deleted">已删除</option>
          <option value="all">全部</option>
        </select>
        <span className="text-[13px] font-medium">({memories?.length ?? '…'})</span>
        <div className="ml-auto flex items-center gap-2 rounded-md border border-border px-2 py-1">
          <IconSearch width={12} height={12} className="text-ink3" />
          <input
            value={q}
            onChange={(e) => setQ(e.target.value)}
            placeholder="搜索记忆…"
            className="w-32 bg-transparent text-[12px] outline-none placeholder:text-ink3"
          />
        </div>
      </div>

      {/* 显式 remember */}
      <div className="border-b border-border bg-surface2 px-4 py-2">
        <div className="flex items-center gap-2">
          <select
            value={newType}
            onChange={(e) => setNewType(e.target.value)}
            className="input h-7 w-20 text-[12px]"
          >
            {Object.entries(TYPE_LABEL).map(([k, v]) => (
              <option key={k} value={k}>
                {v}
              </option>
            ))}
          </select>
          <input
            value={newBody}
            onChange={(e) => setNewBody(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && !creating && handleCreate()}
            placeholder="显式记住一条事实/偏好…（回车提交）"
            className="input h-7 flex-1 text-[12px]"
          />
          <button
            className="btn-primary"
            disabled={!newBody.trim() || creating}
            onClick={() => void handleCreate()}
          >
            <IconPlus width={12} height={12} /> 记住
          </button>
        </div>
        {createError && (
          <p role="alert" className="mt-1 text-[11px] text-danger">
            {createError}
          </p>
        )}
      </div>

      {/* 列表 */}
      <div className="flex-1 overflow-y-auto px-4 py-2">
        {!memories && !loadError && (
          <div className="py-10 text-center text-[12px] text-ink3">加载中…</div>
        )}
        {loadError && (
          <div className="card space-y-3 p-4 text-[12px]">
            <p role="alert" className="text-danger">
              记忆加载失败：{loadError}
            </p>
            <button className="btn-outline" onClick={() => void load()}>
              重试
            </button>
          </div>
        )}
        {memories && (
          <ul className="divide-y divide-border">
            {filtered.map((m) => {
              const meta = STATUS_META[m.status] ?? STATUS_META.note
              const isCandidate = m.status === 'candidate'
              return (
                <li key={m.id} className="py-2.5">
                  <div className="flex items-start gap-3">
                    <div className="min-w-0 flex-1">
                      <div className="flex items-center gap-2">
                        <span
                          className={`rounded px-1.5 py-0.5 text-[10px] font-medium ${meta.cls}`}
                        >
                          {meta.label}
                        </span>
                        <span className="rounded bg-ink3/10 px-1.5 py-0.5 text-[10px] text-ink2">
                          {TYPE_LABEL[m.claim_type] ?? m.claim_type}
                        </span>
                        <span className="rounded bg-ink3/10 px-1.5 py-0.5 text-[10px] text-ink2">
                          {m.scope ?? '—'}
                        </span>
                        <span className="text-[10px] text-ink3">
                          {(m.confidence * 100).toFixed(0)}%
                        </span>
                      </div>
                      <div className="mt-1 text-[13px] text-ink">{m.body}</div>
                      <div className="mt-0.5 text-[10px] text-ink3">
                        {m.origin} · {new Date(m.created_at).toLocaleString()}
                        {m.superseded_by ? ` → ${m.superseded_by.slice(0, 12)}` : ''}
                      </div>
                    </div>
                    <div className="flex shrink-0 items-center gap-1">
                      {isCandidate && (
                        <>
                          <button
                            className="btn-outline h-6 px-2 text-[11px]"
                            onClick={() => void handleApprove(m.id)}
                          >
                            批准
                          </button>
                          <button
                            className="btn-ghost h-6 px-2 text-[11px] text-danger"
                            onClick={() => void handleReject(m.id)}
                          >
                            拒绝
                          </button>
                        </>
                      )}
                      <button
                        className="btn-ghost h-6 px-2 text-[11px]"
                        onClick={() => void handleEdit(m)}
                      >
                        编辑
                      </button>
                      <button
                        className="btn-ghost h-6 px-2 text-[11px] text-danger"
                        onClick={() => void handleDelete(m.id)}
                      >
                        删除
                      </button>
                      <button
                        className="btn-ghost h-6 px-2 text-[11px]"
                        onClick={() => void toggleHistory(m.id)}
                      >
                        {expanded === m.id ? '收起' : '时间线'}
                      </button>
                    </div>
                  </div>
                  {expanded === m.id && (
                    <div className="mt-2 ml-1 rounded border border-border bg-surface2 p-2">
                      <div className="text-[10px] font-medium text-ink3">生命周期</div>
                      {events === null ? (
                        <div className="text-[11px] text-ink3">加载中…</div>
                      ) : events.length === 0 ? (
                        <div className="text-[11px] text-ink3">无事件记录。</div>
                      ) : (
                        <ol className="mt-1 space-y-1">
                          {events.map((ev) => (
                            <li key={ev.id} className="text-[11px] text-ink2">
                              <span className="text-ink3">
                                {new Date(ev.created_at).toLocaleString()}
                              </span>{' '}
                              <span className="font-medium">{ev.event_type}</span>
                              {ev.actor ? ` · ${ev.actor}` : ''}
                              {ev.detail ? (
                                <span className="text-ink3"> {ev.detail}</span>
                              ) : null}
                            </li>
                          ))}
                        </ol>
                      )}
                    </div>
                  )}
                </li>
              )
            })}
            {filtered.length === 0 && (
              <li className="py-10 text-center text-[12px] text-ink3">
                {memories.length === 0
                  ? statusFilter === 'candidate'
                    ? '没有待审批的记忆。'
                    : '暂无记忆。可在上方显式记住一条。'
                  : '没有匹配的记忆。'}
              </li>
            )}
          </ul>
        )}
      </div>
    </div>
  )
}
