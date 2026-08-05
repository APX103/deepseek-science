// 工作台左侧栏：项目名 / New / Search / Customize / Files / Compute / 会话列表（Today 分组）/ 底部主题切换。
import { useState } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import { createSessionApi } from '../../api/client'
import { createSession, useProjects, useSessions } from '../../store'
import { useApp } from '../../App'
import {
  IconCpu,
  IconFile,
  IconMoon,
  IconPlus,
  IconSearch,
  IconSettings,
  IconSliders,
  IconSun,
} from '../icons'

interface Props {
  pid: string
  sid: string
  width: number
  onOpenSkills: () => void
  onOpenFiles: () => void
}

export default function Sidebar({ pid, sid, width, onOpenSkills, onOpenFiles }: Props) {
  const { theme, toggleTheme, openCommandPalette, openSettings, backend } = useApp()
  const navigate = useNavigate()
  const allSessions = useSessions()
  const projects = useProjects()
  const project = projects.find((p) => p.id === pid) ?? projects[0]
  const sessions = allSessions.filter((s) => s.project_id === project?.id)
  const [creating, setCreating] = useState(false)
  const [createError, setCreateError] = useState<string | null>(null)

  const handleNew = async () => {
    if (!project || !backend.online || creating) return
    setCreating(true)
    setCreateError(null)
    try {
      const { id } = await createSessionApi(project.id)
      const s = createSession(project.id, { id })
      navigate(`/p/${project.id}/s/${s.id}`)
    } catch (error) {
      setCreateError(
        `新建会话失败：${error instanceof Error ? error.message : String(error)}。请检查后端日志后重试。`,
      )
    } finally {
      setCreating(false)
    }
  }

  if (!project) return <aside className="shrink-0 border-r border-border bg-surface" style={{ width }} />

  return (
    <aside className="flex shrink-0 flex-col border-r border-border bg-surface" style={{ width }}>
      {/* 项目名 */}
      <div className="border-b border-border px-3 py-2.5">
        <Link to="/" className="block truncate text-[13px] font-semibold text-ink hover:text-brand">
          {project.name}
        </Link>
      </div>

      {/* 主导航 */}
      <nav className="space-y-0.5 p-2">
        <NavBtn
          icon={<IconPlus width={14} height={14} />}
          label={creating ? 'Creating…' : 'New'}
          onClick={handleNew}
          disabled={!backend.online || creating}
        />
        <NavBtn icon={<IconSearch width={14} height={14} />} label="Search" onClick={openCommandPalette} />
        <NavBtn icon={<IconSliders width={14} height={14} />} label="Customize" onClick={onOpenSkills} />
        <NavBtn icon={<IconFile width={14} height={14} />} label="Files" onClick={onOpenFiles} />
        <NavBtn icon={<IconCpu width={14} height={14} />} label="Compute（尚未开放）" disabled />
        {!backend.online && (
          <p className="px-2 pt-1 text-[11px] leading-relaxed text-danger">后端离线，无法新建会话。</p>
        )}
        {createError && <p className="px-2 pt-1 text-[11px] leading-relaxed text-danger">{createError}</p>}
      </nav>

      {/* 会话列表（Today 分组，新的在顶部） */}
      <div className="min-h-0 flex-1 overflow-y-auto border-t border-border p-2">
        <div className="px-2 pb-1 pt-1 text-[11px] font-medium text-ink3">Today</div>
        {sessions.map((s) => (
          <Link
            key={s.id}
            to={`/p/${project.id}/s/${s.id}`}
            className={`block truncate rounded px-2 py-1.5 text-[12px] ${
              s.id === sid ? 'bg-brandSoft font-medium text-brand' : 'text-ink2 hover:bg-surface2 hover:text-ink'
            }`}
          >
            {s.title}
          </Link>
        ))}
      </div>

      {/* 底部：设置 + 主题切换 */}
      <div className="flex items-center gap-1 border-t border-border p-2">
        <button className="btn-ghost rounded p-1.5" title="设置" onClick={openSettings}>
          <IconSettings width={14} height={14} />
        </button>
        <button
          className="btn-ghost rounded p-1.5"
          title={theme === 'light' ? '切换到深色' : '切换到亮色'}
          onClick={toggleTheme}
        >
          {theme === 'light' ? <IconMoon width={14} height={14} /> : <IconSun width={14} height={14} />}
        </button>
      </div>
    </aside>
  )
}

function NavBtn({
  icon,
  label,
  onClick,
  disabled,
}: {
  icon: React.ReactNode
  label: string
  onClick?: () => void
  disabled?: boolean
}) {
  return (
    <button
      onClick={onClick}
      disabled={disabled}
      className="flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-[13px] text-ink2 hover:bg-surface2 hover:text-ink disabled:cursor-not-allowed disabled:opacity-45"
    >
      {icon}
      {label}
    </button>
  )
}
