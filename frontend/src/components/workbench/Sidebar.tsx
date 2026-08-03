// 工作台左侧栏：项目名 / New / Search / Customize / Files / Compute / 会话列表（Today 分组）/ 底部主题切换。
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

  const handleNew = async () => {
    if (!project) return
    // 后端在线：先 POST /api/sessions 拿真实 sid；失败/离线回退 mock 行为
    if (backend.online) {
      try {
        const { id } = await createSessionApi(project.id)
        const s = createSession(project.id, { id })
        navigate(`/p/${project.id}/s/${s.id}`)
        return
      } catch {
        // fall through：后端建会话失败时按 mock 建，保证 UI 演示不崩
      }
    }
    const s = createSession(project.id) // TODO: 接后端 POST /api/sessions
    navigate(`/p/${project.id}/s/${s.id}`)
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
        <NavBtn icon={<IconPlus width={14} height={14} />} label="New" onClick={handleNew} />
        <NavBtn icon={<IconSearch width={14} height={14} />} label="Search" onClick={openCommandPalette} />
        <NavBtn icon={<IconSliders width={14} height={14} />} label="Customize" onClick={onOpenSkills} />
        <NavBtn icon={<IconFile width={14} height={14} />} label="Files" onClick={onOpenFiles} />
        <NavBtn icon={<IconCpu width={14} height={14} />} label="Compute" />
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
}: {
  icon: React.ReactNode
  label: string
  onClick?: () => void
}) {
  return (
    <button
      onClick={onClick}
      className="flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-[13px] text-ink2 hover:bg-surface2 hover:text-ink"
    >
      {icon}
      {label}
    </button>
  )
}
