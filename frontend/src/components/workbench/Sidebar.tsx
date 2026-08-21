// 工作台左侧栏：项目名 / New / Search / Customize / Files / Compute / 会话列表（Today 分组）/ 底部主题切换。
import { useEffect, useRef, useState } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import { createSessionApi, deleteSession as deleteSessionApi } from '../../api/client'
import {
  createSession,
  removeSession,
  renameSession,
  useBots,
  useProjects,
  useSessions,
} from '../../store'
import { useApp } from '../../App'
import Modal from '../Modal'
import {
  IconCpu,
  IconBot,
  IconDots,
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
  const bots = useBots()
  const project = projects.find((p) => p.id === pid) ?? projects[0]
  const sessions = allSessions.filter((s) => s.project_id === project?.id)
  const [creating, setCreating] = useState(false)
  const [createError, setCreateError] = useState<string | null>(null)
  const [menuSid, setMenuSid] = useState<string | null>(null)
  const [renameSid, setRenameSid] = useState<string | null>(null)
  const [renameValue, setRenameValue] = useState('')
  const [renameError, setRenameError] = useState<string | null>(null)
  const [deleteSid, setDeleteSid] = useState<string | null>(null)
  const [deleteError, setDeleteError] = useState<string | null>(null)
  const [deleting, setDeleting] = useState(false)
  const menuRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    function onClick(e: MouseEvent) {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        setMenuSid(null)
      }
    }
    if (menuSid) {
      document.addEventListener('mousedown', onClick)
      return () => document.removeEventListener('mousedown', onClick)
    }
  }, [menuSid])

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
      {/* 顶部：为 macOS 红黄绿三点让出安全区（可拖拽），其下放品牌文字 logo */}
      <div className="shrink-0">
        <div data-tauri-drag-region className="h-10" />
        <Link
          to="/"
          className="block truncate px-3 pb-2 pt-0.5 text-[13px] font-semibold text-brand hover:text-brandHover"
        >
          DeepSeek Science
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
        <NavBtn icon={<IconBot width={14} height={14} />} label="Bots" onClick={() => navigate('/bots')} />
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
        {sessions.map((s) => {
          const isActive = s.id === sid
          return (
            <div
              key={s.id}
              className={`group relative flex items-center justify-between rounded text-[12px] ${
                isActive ? 'bg-brandSoft' : 'hover:bg-surface2'
              }`}
            >
              <Link
                to={`/p/${project.id}/s/${s.id}`}
                className={`block min-w-0 flex-1 truncate px-2 py-1.5 ${
                  isActive ? 'font-medium text-brand' : 'text-ink2 group-hover:text-ink'
                }`}
                onClick={() => setMenuSid(null)}
              >
                {s.bot_id ? `${bots.find((bot) => bot.id === s.bot_id)?.avatar ?? '🤖'} ${s.title}` : s.title}
              </Link>
              <button
                className={`shrink-0 rounded p-1 text-ink3 ${
                  isActive ? 'opacity-100' : 'opacity-0 group-hover:opacity-100'
                } hover:bg-surface2 hover:text-ink`}
                title="会话选项"
                onClick={(e) => {
                  e.stopPropagation()
                  setMenuSid(menuSid === s.id ? null : s.id)
                }}
              >
                <IconDots width={14} height={14} />
              </button>
              {menuSid === s.id && (
                <div
                  ref={menuRef}
                  className="absolute right-1 top-7 z-20 w-28 rounded-md border border-border bg-bg shadow-overlay"
                >
                  <button
                    className="w-full px-3 py-1.5 text-left text-[12px] text-ink hover:bg-surface2"
                    onClick={() => {
                      setRenameSid(s.id)
                      setRenameValue(s.title)
                      setRenameError(null)
                      setMenuSid(null)
                    }}
                  >
                    重命名
                  </button>
                  <button
                    className="w-full px-3 py-1.5 text-left text-[12px] text-danger hover:bg-surface2"
                    onClick={() => {
                      setMenuSid(null)
                      setDeleteSid(s.id)
                      setDeleteError(null)
                    }}
                  >
                    删除
                  </button>
                </div>
              )}
            </div>
          )
        })}
      </div>

      {deleteSid && (
        <Modal
          title="删除会话"
          onClose={() => {
            if (deleting) return
            setDeleteSid(null)
          }}
          width="max-w-sm"
        >
          {(() => {
            const s = sessions.find((x) => x.id === deleteSid)
            return (
              <div className="p-4">
                <p className="text-[13px] text-ink">
                  确定删除会话“<span className="font-medium">{s?.title}</span>”？此操作不可恢复。
                </p>
                {deleteError && <p className="mt-2 text-[11px] text-danger">{deleteError}</p>}
                <div className="mt-4 flex justify-end gap-2">
                  <button
                    type="button"
                    className="rounded-md px-3 py-1.5 text-[12px] text-ink2 hover:bg-surface2 disabled:opacity-50"
                    onClick={() => setDeleteSid(null)}
                    disabled={deleting}
                  >
                    取消
                  </button>
                  <button
                    type="button"
                    className="rounded-md bg-danger px-3 py-1.5 text-[12px] font-medium text-white hover:bg-dangerSoft disabled:opacity-50"
                    disabled={deleting}
                    onClick={async () => {
                      if (!s) return
                      setDeleting(true)
                      setDeleteError(null)
                      try {
                        await deleteSessionApi(s.id)
                        removeSession(s.id)
                        setDeleteSid(null)
                        if (s.id === sid) {
                          navigate(`/p/${project.id}`)
                        }
                      } catch (error) {
                        setDeleteError(
                          `删除失败：${error instanceof Error ? error.message : String(error)}`,
                        )
                      } finally {
                        setDeleting(false)
                      }
                    }}
                  >
                    {deleting ? '删除中…' : '删除'}
                  </button>
                </div>
              </div>
            )
          })()}
        </Modal>
      )}

      {renameSid && (
        <Modal title="重命名会话" onClose={() => setRenameSid(null)} width="max-w-sm">
          <form
            className="p-4"
            onSubmit={async (e) => {
              e.preventDefault()
              setRenameError(null)
              try {
                await renameSession(renameSid, renameValue)
                setRenameSid(null)
              } catch (error) {
                setRenameError(`重命名失败：${error instanceof Error ? error.message : String(error)}`)
              }
            }}
          >
            <input
              type="text"
              value={renameValue}
              onChange={(e) => setRenameValue(e.target.value)}
              className="w-full rounded-md border border-border bg-surface px-3 py-2 text-[13px] outline-none focus:border-brand"
              autoFocus
            />
            {renameError && <p className="mt-2 text-[11px] text-danger">{renameError}</p>}
            <div className="mt-4 flex justify-end gap-2">
              <button
                type="button"
                className="rounded-md px-3 py-1.5 text-[12px] text-ink2 hover:bg-surface2"
                onClick={() => setRenameSid(null)}
              >
                取消
              </button>
              <button
                type="submit"
                className="rounded-md bg-brand px-3 py-1.5 text-[12px] font-medium text-white hover:bg-brandHover"
                disabled={!renameValue.trim()}
              >
                保存
              </button>
            </div>
          </form>
        </Modal>
      )}

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
