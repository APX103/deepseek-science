// 首页：项目列表 + 最近会话 + 恢复会话横幅 + New Project 弹窗。
import { useEffect, useMemo, useState } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import {
  addProject,
  loadFromBackend,
  removeProject,
  updateProject,
  useBackendOnline,
  useProjects,
  useSessions,
} from '../store'
import { useApp } from '../App'
import * as api from '../api/client'
import Dropdown from '../components/Dropdown'
import NewProjectModal from '../components/NewProjectModal'
import {
  IconArchive,
  IconClock,
  IconDots,
  IconFile,
  IconFolder,
  IconMoon,
  IconPin,
  IconPlus,
  IconSearch,
  IconSettings,
  IconSun,
  IconTrash,
  IconX,
} from '../components/icons'

function timeAgo(iso: string): string {
  const h = Math.max(0, (Date.now() - new Date(iso).getTime()) / 3_600_000)
  if (h < 1) return `${Math.max(1, Math.round(h * 60))}m ago`
  if (h < 24) return `${Math.round(h)}h ago`
  return `${Math.round(h / 24)}d ago`
}

export default function HomePage() {
  const { theme, toggleTheme, openCommandPalette, openSettings } = useApp()
  const navigate = useNavigate()
  const projects = useProjects()
  const [showNew, setShowNew] = useState(false)
  const sessions = useSessions()
  const online = useBackendOnline()

  // 挂载时从后端拉取 projects/sessions。
  useEffect(() => {
    void loadFromBackend()
  }, [])

  const failedSession = useMemo(() => sessions.find((s) => s.status === 'failed'), [sessions])
  const failedProject = failedSession && projects.find((p) => p.id === failedSession.project_id)

  return (
    <div className="mx-auto max-w-3xl px-6 py-10">
      {/* 顶栏 */}
      <header className="flex items-center">
        <div>
          <h1 className="text-[20px] font-semibold leading-[1.3] tracking-tight text-brand">
            DeepSeek Science
          </h1>
          <span className="text-[11px] text-ink3">Beta</span>
        </div>
        <div className="ml-auto flex items-center gap-1">
          <button className="btn-ghost rounded p-2" title="搜索（⌘K）" onClick={openCommandPalette}>
            <IconSearch width={16} height={16} />
          </button>
          <button className="btn-ghost rounded p-2" title="日志" onClick={() => navigate('/logs')}>
            <IconFile width={16} height={16} />
          </button>
          <button className="btn-ghost rounded p-2" title="设置" onClick={openSettings}>
            <IconSettings width={16} height={16} />
          </button>
          <button
            className="btn-ghost rounded p-2"
            title={theme === 'light' ? '切换到深色' : '切换到亮色'}
            onClick={toggleTheme}
          >
            {theme === 'light' ? <IconMoon width={16} height={16} /> : <IconSun width={16} height={16} />}
          </button>
          <button className="btn-outline ml-2" onClick={() => setShowNew(true)}>
            <IconPlus width={14} height={14} /> New project
          </button>
        </div>
      </header>

      {/* 恢复会话横幅 */}
      {failedSession && failedProject && (
        <Link
          to={`/p/${failedProject.id}/s/${failedSession.id}`}
          className="card mt-6 block p-4 hover:bg-surface"
        >
          <div className="text-[13px] font-medium text-ink">{failedSession.title}</div>
          <div className="mt-0.5 text-[12px] text-ink2">{failedProject.name}</div>
          <div className="mt-2 flex items-center justify-between">
            <span className="inline-flex items-center gap-1 rounded bg-dangerSoft px-1.5 py-0.5 text-[11px] font-medium text-danger">
              <IconX width={10} height={10} /> Failed
            </span>
            <span className="text-[11px] text-ink3">{timeAgo(failedSession.updated_at)}</span>
          </div>
        </Link>
      )}

      {/* 两栏：Projects / Recent sessions */}
      <div className="mt-8 grid grid-cols-1 gap-8 md:grid-cols-2">
        <section>
          <h2 className="flex items-center gap-1.5 text-[13px] font-semibold text-ink">
            <IconFolder width={14} height={14} className="text-ink3" /> Projects
          </h2>
          <div className="card mt-2 divide-y divide-border">
            {projects.map((p) => (
              <div
                key={p.id}
                className="group flex cursor-pointer items-center gap-2 px-4 py-3 hover:bg-surface"
                onClick={() => {
                  const s = sessions.find((x) => x.project_id === p.id)
                  if (s) navigate(`/p/${p.id}/s/${s.id}`)
                }}
              >
                <div className="min-w-0 flex-1">
                  <span className="text-[13px] font-medium text-ink">{p.name}</span>
                  {p.name === 'Example project' && (
                    <span className="ml-1.5 rounded bg-surface2 px-1 py-0.5 text-[10px] text-ink3">Example</span>
                  )}
                </div>
                <span className="shrink-0 text-[11px] text-ink3">
                  {p.session_count} session{p.session_count > 1 ? 's' : ''}
                </span>
                <span className="w-9 shrink-0 text-right text-[11px] text-ink3">{timeAgo(p.updated_at)}</span>
                <div onClick={(e) => e.stopPropagation()}>
                  <Dropdown
                    trigger={
                      <button className="btn-ghost rounded p-1 opacity-0 group-hover:opacity-100" aria-label="更多">
                        <IconDots width={14} height={14} />
                      </button>
                    }
                    items={[
                      {
                        label: 'Pin project',
                        icon: <IconPin width={13} height={13} />,
                        onClick: () => updateProject(p.id, { pinned: !p.pinned }),
                      },
                      { label: 'Settings', icon: <IconSettings width={13} height={13} />, onClick: openSettings },
                      {
                        label: 'Archive project',
                        icon: <IconArchive width={13} height={13} />,
                        onClick: async () => {
                          try {
                            await api.archiveProject(p.id)
                            void loadFromBackend()
                          } catch {
                            /* ignore */
                          }
                        },
                      },
                      {
                        label: 'Delete project',
                        icon: <IconTrash width={13} height={13} />,
                        danger: true,
                        onClick: async () => {
                          try {
                            await api.deleteProject(p.id, true)
                            removeProject(p.id)
                          } catch {
                            /* ignore */
                          }
                        },
                      },
                    ]}
                  />
                </div>
              </div>
            ))}
            {projects.length === 0 && (
              <div className="px-4 py-6 text-center text-[12px] text-ink3">
                {online ? '暂无项目，点击右上角 New project 创建' : '后端未连接 — 请先启动后端（dss-backend serve）'}
              </div>
            )}
          </div>
        </section>

        <section>
          <h2 className="flex items-center gap-1.5 text-[13px] font-semibold text-ink">
            <IconClock width={14} height={14} className="text-ink3" /> Recent sessions
          </h2>
          <div className="card mt-2 divide-y divide-border">
            {sessions.map((s) => {
              const proj = projects.find((p) => p.id === s.project_id)
              return (
                <Link key={s.id} to={`/p/${s.project_id}/s/${s.id}`} className="flex items-start gap-2.5 px-4 py-3 hover:bg-surface">
                  <span
                    className={`mt-[7px] h-1.5 w-1.5 shrink-0 rounded-full ${
                      s.status === 'failed' ? 'bg-danger' : s.status === 'completed' ? 'bg-success' : 'bg-brand'
                    }`}
                  />
                  <div className="min-w-0 flex-1">
                    <div className={`truncate text-[13px] ${s.status === 'failed' ? 'text-danger' : 'text-ink'}`}>
                      {s.title}
                    </div>
                    <div className="truncate text-[11px] text-ink3">{proj?.name}</div>
                  </div>
                  <span className="shrink-0 text-[11px] text-ink3">{timeAgo(s.updated_at)}</span>
                </Link>
              )
            })}
          </div>
        </section>
      </div>

      {showNew && <NewProjectModal onClose={() => setShowNew(false)} onCreated={addProject} />}
    </div>
  )
}
