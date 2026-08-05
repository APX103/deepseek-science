// ⌘K 全局搜索弹层：只搜索后端已加载的项目与会话。
import { useMemo, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { useProjects, useSessions } from '../store'
import { IconChevronRight, IconFile, IconMessage, IconSearch } from './icons'

interface Props {
  onClose: () => void
}

export default function CommandPalette({ onClose }: Props) {
  const [q, setQ] = useState('')
  const navigate = useNavigate()
  const allSessions = useSessions()
  const projects = useProjects()

  const results = useMemo(() => {
    const needle = q.trim().toLowerCase()
    const match = (s: string) => !needle || s.toLowerCase().includes(needle)
    return {
      sessions: allSessions.filter((s) => match(s.title)),
      projects: projects.filter((p) => match(p.name)),
    }
  }, [q, allSessions, projects])

  const goSession = (pid: string, sid: string) => {
    onClose()
    navigate(`/p/${pid}/s/${sid}`)
  }

  const goLogs = () => {
    onClose()
    navigate('/logs')
  }

  return (
    <div className="fixed inset-0 z-50 flex items-start justify-center bg-black/20 pt-[14vh]" onMouseDown={onClose}>
      <div
        className="w-full max-w-md overflow-hidden rounded-xl border border-border bg-bg shadow-overlay"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <div className="flex items-center gap-2 border-b border-border px-3">
          <IconSearch className="text-ink3" width={14} height={14} />
          <input
            autoFocus
            value={q}
            onChange={(e) => setQ(e.target.value)}
            onKeyDown={(e) => e.key === 'Escape' && onClose()}
            placeholder="Search this project…"
            className="w-full bg-transparent py-2.5 text-[13px] outline-none placeholder:text-ink3"
          />
          <kbd className="rounded border border-border bg-surface px-1.5 py-0.5 text-[11px] text-ink3">esc</kbd>
        </div>

        <div className="max-h-[50vh] overflow-y-auto py-1">
          {results.sessions.length > 0 && (
            <Section label="Recent sessions">
              {results.sessions.map((s) => (
                <Row
                  key={s.id}
                  icon={<IconMessage width={14} height={14} />}
                  title={s.title}
                  sub={projects.find((p) => p.id === s.project_id)?.name}
                  onClick={() => goSession(s.project_id, s.id)}
                />
              ))}
            </Section>
          )}
          {results.projects.length > 0 && (
            <Section label="Projects">
              {results.projects.map((p) => (
                <Row
                  key={p.id}
                  icon={<IconChevronRight width={14} height={14} />}
                  title={p.name}
                  sub={`${p.session_count} session${p.session_count > 1 ? 's' : ''}`}
                  onClick={onClose}
                />
              ))}
            </Section>
          )}
          <Section label="Commands">
            <Row icon={<IconFile width={14} height={14} />} title="View logs" sub="Command" onClick={goLogs} />
          </Section>
        </div>
      </div>
    </div>
  )
}

function Section({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="py-1">
      <div className="px-3 py-1 text-[11px] font-medium text-ink3">{label}</div>
      {children}
    </div>
  )
}

function Row({
  icon,
  title,
  sub,
  onClick,
}: {
  icon: React.ReactNode
  title: string
  sub?: string
  onClick: () => void
}) {
  return (
    <button
      className="flex w-full items-center gap-2.5 px-3 py-1.5 text-left hover:bg-surface2"
      onClick={onClick}
    >
      <span className="shrink-0 text-ink3">{icon}</span>
      <span className="min-w-0 flex-1">
        <span className="block truncate text-[13px] text-ink">{title}</span>
        {sub && <span className="block truncate text-[11px] text-ink3">{sub}</span>}
      </span>
    </button>
  )
}
