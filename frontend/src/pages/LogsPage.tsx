// 日志页：统一真实后端日志视图（system + agent 同列表，按多维度过滤）。
import { useEffect, useMemo, useState } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import { clearLogs, listLogs } from '../api/client'
import type { LogEntry, LogLevel, LogSource } from '../types'
import { useSessions } from '../store'
import { IconChevronRight, IconTrash } from '../components/icons'

const LEVELS: LogLevel[] = ['debug', 'info', 'warn', 'error']

/** 级别徽章配色（error 红 / warn 黄 / info 灰 / debug 淡）。 */
const LEVEL_STYLE: Record<LogLevel, string> = {
  error: 'bg-dangerSoft text-danger',
  warn: 'bg-amber-500/10 text-amber-500',
  info: 'bg-surface2 text-ink2',
  debug: 'bg-surface2 text-ink3',
}

const RANGES = [
  { id: 'all', label: '全部时间' },
  { id: '1h', label: '近 1 小时' },
  { id: '24h', label: '近 24 小时' },
  { id: '7d', label: '近 7 天' },
] as const

type RangeId = (typeof RANGES)[number]['id']

function sinceOf(range: RangeId): string | null {
  if (range === 'all') return null
  const ms = range === '1h' ? 3_600_000 : range === '24h' ? 86_400_000 : 7 * 86_400_000
  return new Date(Date.now() - ms).toISOString()
}

function fmtTs(ts: string): string {
  const date = new Date(ts)
  if (Number.isNaN(date.getTime())) return ts
  const pad = (value: number) => String(value).padStart(2, '0')
  return `${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(
    date.getMinutes(),
  )}:${pad(date.getSeconds())}`
}

export default function LogsPage() {
  const navigate = useNavigate()
  const sessions = useSessions()
  const [logs, setLogs] = useState<LogEntry[]>([])
  const [levels, setLevels] = useState<Set<LogLevel>>(new Set(LEVELS))
  const [source, setSource] = useState<'all' | LogSource>('all')
  const [kind, setKind] = useState('')
  const [sessionId, setSessionId] = useState('all')
  const [range, setRange] = useState<RangeId>('all')
  const [expanded, setExpanded] = useState<number | null>(null)
  const [confirmClear, setConfirmClear] = useState(false)

  useEffect(() => {
    void listLogs().then((r) => setLogs(r.logs))
  }, [])

  const kinds = useMemo(() => [...new Set(logs.map((l) => l.kind))].sort(), [logs])

  const filtered = useMemo(() => {
    const since = sinceOf(range)
    return logs.filter(
      (l) =>
        levels.has(l.level) &&
        (source === 'all' || l.source === source) &&
        (!kind || l.kind.includes(kind.trim())) &&
        (sessionId === 'all' || l.session_id === sessionId) &&
        (!since || l.ts >= since),
    )
  }, [logs, levels, source, kind, sessionId, range])

  const toggleLevel = (lv: LogLevel) =>
    setLevels((prev) => {
      const next = new Set(prev)
      if (next.has(lv)) next.delete(lv)
      else next.add(lv)
      return next
    })

  const doClear = async () => {
    await clearLogs()
    setLogs([])
    setConfirmClear(false)
    setExpanded(null)
  }

  const goSession = (sid: string) => {
    const s = sessions.find((x) => x.id === sid)
    if (s) navigate(`/p/${s.project_id}/s/${s.id}`)
  }

  return (
    <div className="mx-auto max-w-5xl px-6 py-8">
      {/* 无原生标题栏时的顶部窗口拖拽区 */}
      <div data-tauri-drag-region className="fixed inset-x-0 top-0 z-30 h-7" />
      {/* 顶栏 */}
      <header className="flex items-center gap-3">
        <Link to="/" className="text-[13px] text-ink3 hover:text-brand">
          ← 首页
        </Link>
        <h1 className="text-[18px] font-semibold tracking-tight text-ink">日志</h1>
        <span className="text-[12px] text-ink3">
          {filtered.length} / {logs.length} 条
        </span>
        <div className="ml-auto">
          {confirmClear ? (
            <span className="flex items-center gap-2 text-[12px] text-ink2">
              确认清空全部日志？
              <button
                className="btn bg-danger px-2 py-1 text-[12px] text-white"
                onClick={() => void doClear()}
              >
                清空
              </button>
              <button className="btn-outline px-2 py-1 text-[12px]" onClick={() => setConfirmClear(false)}>
                取消
              </button>
            </span>
          ) : (
            <button className="btn-outline" onClick={() => setConfirmClear(true)}>
              <IconTrash width={13} height={13} /> 清理日志
            </button>
          )}
        </div>
      </header>

      {/* 过滤栏 */}
      <div className="card mt-4 flex flex-wrap items-center gap-x-4 gap-y-2 px-4 py-3">
        <div className="flex items-center gap-1">
          {LEVELS.map((lv) => (
            <button
              key={lv}
              onClick={() => toggleLevel(lv)}
              className={`rounded px-2 py-0.5 text-[11px] font-medium ${
                levels.has(lv) ? LEVEL_STYLE[lv] : 'text-ink3 opacity-40'
              }`}
            >
              {lv}
            </button>
          ))}
        </div>
        <select className="input w-auto py-1 text-[12px]" value={source} onChange={(e) => setSource(e.target.value as 'all' | LogSource)}>
          <option value="all">全部来源</option>
          <option value="system">system</option>
          <option value="agent">agent</option>
        </select>
        <input
          className="input w-36 py-1 text-[12px]"
          placeholder="kind 过滤…"
          list="log-kinds"
          value={kind}
          onChange={(e) => setKind(e.target.value)}
        />
        <datalist id="log-kinds">
          {kinds.map((k) => (
            <option key={k} value={k} />
          ))}
        </datalist>
        <select className="input w-auto max-w-56 py-1 text-[12px]" value={sessionId} onChange={(e) => setSessionId(e.target.value)}>
          <option value="all">全部会话</option>
          {sessions.map((s) => (
            <option key={s.id} value={s.id}>
              {s.title.slice(0, 24)}
            </option>
          ))}
        </select>
        <select className="input w-auto py-1 text-[12px]" value={range} onChange={(e) => setRange(e.target.value as RangeId)}>
          {RANGES.map((r) => (
            <option key={r.id} value={r.id}>
              {r.label}
            </option>
          ))}
        </select>
      </div>

      {/* 列表 */}
      <div className="card mt-3 divide-y divide-border">
        {filtered.map((l) => (
          <div key={l.id}>
            <button
              className="flex w-full items-center gap-3 px-4 py-2 text-left hover:bg-surface"
              onClick={() => setExpanded((cur) => (cur === l.id ? null : l.id))}
            >
              <span className="w-32 shrink-0 font-mono text-[11px] text-ink3">{fmtTs(l.ts)}</span>
              <span className={`w-12 shrink-0 rounded px-1.5 py-0.5 text-center text-[11px] font-medium ${LEVEL_STYLE[l.level]}`}>
                {l.level}
              </span>
              <span className="w-14 shrink-0 text-[11px] text-ink2">{l.source}</span>
              <span className="w-28 shrink-0 truncate font-mono text-[11px] text-ink3">{l.kind}</span>
              <span className="min-w-0 flex-1 truncate text-[13px] text-ink">{l.message}</span>
              <IconChevronRight
                width={12}
                height={12}
                className={`shrink-0 text-ink3 transition-transform ${expanded === l.id ? 'rotate-90' : ''}`}
              />
            </button>
            {expanded === l.id && (
              <div className="border-t border-border bg-surface px-4 py-3">
                <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-[11px] text-ink3">
                  {l.session_id && <span>session: {l.session_id}</span>}
                  {l.frame_id && <span>frame: {l.frame_id}</span>}
                  {l.iteration !== undefined && <span>iteration: {l.iteration}</span>}
                  {l.session_id && sessions.some((s) => s.id === l.session_id) && (
                    <button className="font-medium text-brand hover:underline" onClick={() => goSession(l.session_id!)}>
                      跳转会话 →
                    </button>
                  )}
                </div>
                {l.detail && (
                  <pre className="mt-2 overflow-x-auto rounded-md border border-border bg-bg p-3 font-mono text-[12px] leading-relaxed text-ink2">
                    {JSON.stringify(l.detail, null, 2)}
                  </pre>
                )}
              </div>
            )}
          </div>
        ))}
        {filtered.length === 0 && (
          <div className="px-4 py-10 text-center text-[12px] text-ink3">没有匹配的日志</div>
        )}
      </div>
    </div>
  )
}
