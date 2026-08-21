import { useEffect, useMemo, useState, type ReactNode } from 'react'
import { useNavigate } from 'react-router-dom'
import { useApp } from '../App'
import * as api from '../api/client'
import { IconBot, IconChevronRight, IconMoon, IconPlus, IconSettings, IconSun, IconTrash, IconX } from '../components/icons'
import {
  addBot,
  createSession,
  loadFromBackend,
  removeBot,
  replaceBot,
  useBackendOnline,
  useBots,
  useProjects,
  useSessions,
} from '../store'
import type { Bot, SessionSummary } from '../types'

const AVATARS = ['🤖', '🔬', '🔭', '🧬', '📊', '🧠', '🛠️', '🐋']
const COLORS = ['#4D6BFE', '#7C3AED', '#0891B2', '#059669', '#D97706', '#E11D48']

interface BotDraft {
  name: string
  role: string
  instructions: string
  avatar: string
  color: string
  projectId: string
  model: string
  thinkingEnabled: boolean | null
  thinkingEffort: 'low' | 'high' | 'max' | null
}

function emptyDraft(projectId: string): BotDraft {
  return {
    name: '',
    role: 'Research assistant',
    instructions: '',
    avatar: '🤖',
    color: '#4D6BFE',
    projectId,
    model: '',
    thinkingEnabled: null,
    thinkingEffort: null,
  }
}

function draftFromBot(bot: Bot): BotDraft {
  return {
    name: bot.name,
    role: bot.role,
    instructions: bot.instructions,
    avatar: bot.avatar,
    color: bot.color,
    projectId: bot.project_id ?? 'proj_default',
    model: bot.model ?? '',
    thinkingEnabled: bot.thinking_enabled,
    thinkingEffort: bot.thinking_effort,
  }
}

export function botHasActiveWork(
  botId: string,
  sessions: ReadonlyArray<Pick<SessionSummary, 'bot_id' | 'live' | 'status'>>,
): boolean {
  return sessions.some(
    (session) => session.bot_id === botId && session.live && session.status === 'processing',
  )
}

export default function BotsPage() {
  const navigate = useNavigate()
  const { theme, toggleTheme, openSettings } = useApp()
  const bots = useBots()
  const sessions = useSessions()
  const projects = useProjects()
  const online = useBackendOnline()
  const [editing, setEditing] = useState<Bot | null | 'new'>(null)
  const [openingId, setOpeningId] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    void loadFromBackend()
  }, [])

  const enabledBots = useMemo(() => bots.filter((bot) => bot.enabled), [bots])

  const openBot = async (bot: Bot) => {
    if (!online || openingId) return
    setOpeningId(bot.id)
    setError(null)
    try {
      const existing = sessions
        .filter((session) => session.bot_id === bot.id)
        .sort((a, b) => b.updated_at.localeCompare(a.updated_at))[0]
      const projectId = bot.project_id ?? 'proj_default'
      if (existing) {
        navigate(`/p/${existing.project_id}/s/${existing.id}`)
        return
      }
      const created = await api.createSessionApi(projectId, bot.id)
      const session = createSession(projectId, { id: created.id, botId: bot.id })
      navigate(`/p/${projectId}/s/${session.id}`)
    } catch (reason) {
      setError(`打开 Agent 失败：${reason instanceof Error ? reason.message : String(reason)}`)
    } finally {
      setOpeningId(null)
    }
  }

  return (
    <div className="min-h-full bg-canvas px-6 pb-12 pt-10">
      <div data-tauri-drag-region className="fixed inset-x-0 top-0 z-30 h-7" />
      <main className="mx-auto max-w-5xl">
        <header className="flex items-start gap-4">
          <button className="btn-ghost mt-0.5 rounded px-2 py-1 text-[12px]" onClick={() => navigate('/')}>
            ← Projects
          </button>
          <div>
            <div className="flex items-center gap-2">
              <IconBot width={20} height={20} className="text-brand" />
              <h1 className="text-[22px] font-semibold tracking-tight text-ink">Agent Profiles</h1>
            </div>
            <p className="mt-1 max-w-xl text-[12px] leading-relaxed text-ink2">
              Create persistent research teammates with a stable role, memory context, workspace and restart-safe work queue.
            </p>
          </div>
          <div className="ml-auto flex items-center gap-1">
            <button className="btn-ghost rounded p-2" title="Settings" onClick={openSettings}>
              <IconSettings width={16} height={16} />
            </button>
            <button className="btn-ghost rounded p-2" title="Toggle theme" onClick={toggleTheme}>
              {theme === 'light' ? <IconMoon width={16} height={16} /> : <IconSun width={16} height={16} />}
            </button>
            <button className="btn-primary ml-2" onClick={() => setEditing('new')} disabled={!online}>
              <IconPlus width={14} height={14} /> New Agent
            </button>
          </div>
        </header>

        {error && (
          <div role="alert" className="mt-5 flex items-start gap-2 rounded-lg border border-danger/25 bg-dangerSoft px-3 py-2 text-[12px] text-danger">
            <span className="min-w-0 flex-1">{error}</span>
            <button aria-label="Dismiss" onClick={() => setError(null)}><IconX width={14} /></button>
          </div>
        )}

        <section className="mt-8 grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {enabledBots.map((bot) => {
            const botSessions = sessions.filter((session) => session.bot_id === bot.id)
            const active = botHasActiveWork(bot.id, botSessions)
            const queuedLabel = botSessions.length === 0 ? 'Ready for first task' : `${botSessions.length} conversation${botSessions.length === 1 ? '' : 's'}`
            return (
              <article key={bot.id} className="group relative overflow-hidden rounded-xl border border-border bg-surface shadow-sm transition hover:-translate-y-0.5 hover:border-brand/35 hover:shadow-md">
                <div className="h-1" style={{ backgroundColor: bot.color }} />
                <button className="w-full p-4 text-left" onClick={() => void openBot(bot)} disabled={openingId === bot.id}>
                  <div className="flex items-center gap-3">
                    <span className="grid h-11 w-11 place-items-center rounded-xl text-[24px]" style={{ backgroundColor: `${bot.color}18` }}>
                      {bot.avatar}
                    </span>
                    <div className="min-w-0 flex-1">
                      <h2 className="truncate text-[14px] font-semibold text-ink">{bot.name}</h2>
                      <p className="truncate text-[11px] text-ink2">{bot.role}</p>
                    </div>
                    <IconChevronRight className="text-ink3 transition group-hover:translate-x-0.5 group-hover:text-brand" />
                  </div>
                  <p className="mt-4 line-clamp-3 min-h-[54px] text-[12px] leading-[18px] text-ink2">
                    {bot.instructions || 'Uses the app defaults and adapts to the current research conversation.'}
                  </p>
                  <div className="mt-4 flex items-center justify-between border-t border-border pt-3 text-[10px] text-ink3">
                    <span className="inline-flex items-center gap-1.5">
                      <span className={`h-1.5 w-1.5 rounded-full ${active ? 'animate-pulse bg-success' : 'bg-ink3/40'}`} />
                      {active ? 'Working' : queuedLabel}
                    </span>
                    <span>{openingId === bot.id ? 'Opening…' : 'Open →'}</span>
                  </div>
                </button>
                <button
                  className="absolute right-10 top-3 rounded p-1.5 text-ink3 opacity-0 hover:bg-surface2 hover:text-ink group-hover:opacity-100"
                  aria-label={`Edit ${bot.name}`}
                  onClick={() => setEditing(bot)}
                >
                  <IconSettings width={13} />
                </button>
              </article>
            )
          })}

          <button
            className="grid min-h-[230px] place-items-center rounded-xl border border-dashed border-border bg-surface/50 p-6 text-center transition hover:border-brand/50 hover:bg-brandSoft/30"
            onClick={() => setEditing('new')}
            disabled={!online}
          >
            <span>
              <span className="mx-auto grid h-10 w-10 place-items-center rounded-full bg-brandSoft text-brand"><IconPlus /></span>
              <span className="mt-3 block text-[13px] font-medium text-ink">Create a teammate</span>
              <span className="mt-1 block text-[11px] text-ink3">Give it a role, instructions and workspace</span>
            </span>
          </button>
        </section>

        {bots.length === 0 && !online && (
          <p className="mt-8 text-center text-[12px] text-danger">Backend is offline. Start it to create or open Agents.</p>
        )}
      </main>

      {editing && (
        <BotEditor
          bot={editing === 'new' ? null : editing}
          projectId={projects[0]?.id ?? 'proj_default'}
          projects={projects}
          onClose={() => setEditing(null)}
          onSaved={(bot) => {
            if (editing === 'new') addBot(bot)
            else replaceBot(bot)
            setEditing(null)
          }}
          onDeleted={(botId) => {
            removeBot(botId)
            setEditing(null)
          }}
        />
      )}
    </div>
  )
}

function BotEditor({ bot, projectId, projects, onClose, onSaved, onDeleted }: {
  bot: Bot | null
  projectId: string
  projects: ReturnType<typeof useProjects>
  onClose: () => void
  onSaved: (bot: Bot) => void
  onDeleted: (botId: string) => void
}) {
  const [draft, setDraft] = useState<BotDraft>(() => bot ? draftFromBot(bot) : emptyDraft(projectId))
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const valid = draft.name.trim() && draft.role.trim()

  const save = async () => {
    if (!valid || saving) return
    setSaving(true)
    setError(null)
    try {
      const saved = bot
        ? await api.updateBot(bot.id, {
            revision: bot.revision,
            name: draft.name,
            role: draft.role,
            instructions: draft.instructions,
            avatar: draft.avatar,
            color: draft.color,
            project_id: draft.projectId,
            model: draft.model.trim() || null,
            thinking_enabled: draft.thinkingEnabled,
            thinking_effort: draft.thinkingEffort,
            enabled: bot.enabled,
          })
        : await api.createBot({
            name: draft.name,
            role: draft.role,
            instructions: draft.instructions,
            avatar: draft.avatar,
            color: draft.color,
            project_id: draft.projectId,
            model: draft.model.trim() || undefined,
          })
      onSaved(saved)
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason))
    } finally {
      setSaving(false)
    }
  }

  const remove = async () => {
    if (!bot || saving || !window.confirm(`Delete Agent “${bot.name}”? Its conversations and files will be kept.`)) return
    setSaving(true)
    try {
      await api.deleteBot(bot.id)
      onDeleted(bot.id)
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason))
      setSaving(false)
    }
  }

  return (
    <div className="fixed inset-0 z-50 grid place-items-center bg-black/25 p-4" onMouseDown={(event) => event.target === event.currentTarget && onClose()}>
      <section role="dialog" aria-modal="true" aria-label={bot ? 'Edit Agent' : 'Create Agent'} className="w-full max-w-xl rounded-xl border border-border bg-surface shadow-xl">
        <header className="flex items-center border-b border-border px-5 py-4">
          <div>
            <h2 className="text-[15px] font-semibold text-ink">{bot ? 'Edit Agent' : 'Create Agent'}</h2>
            <p className="mt-0.5 text-[11px] text-ink3">This identity is reused across conversations and app restarts.</p>
          </div>
          <button className="btn-ghost ml-auto rounded p-1.5" aria-label="Close" onClick={onClose}><IconX /></button>
        </header>
        <div className="max-h-[72vh] space-y-4 overflow-y-auto p-5">
          <div className="flex gap-4">
            <div className="grid h-16 w-16 shrink-0 place-items-center rounded-2xl text-[34px]" style={{ backgroundColor: `${draft.color}18` }}>{draft.avatar}</div>
            <div className="grid min-w-0 flex-1 grid-cols-2 gap-3">
              <Field label="Name"><input className="input w-full" value={draft.name} maxLength={80} autoFocus onChange={(e) => setDraft({ ...draft, name: e.target.value })} placeholder="Nova" /></Field>
              <Field label="Role"><input className="input w-full" value={draft.role} maxLength={160} onChange={(e) => setDraft({ ...draft, role: e.target.value })} placeholder="Literature scout" /></Field>
            </div>
          </div>
          <Field label="Instructions" hint="Stable behavior and boundaries for every conversation.">
            <textarea className="input min-h-32 w-full resize-y py-2" value={draft.instructions} maxLength={16000} onChange={(e) => setDraft({ ...draft, instructions: e.target.value })} placeholder="Find primary sources, preserve citations, and flag uncertainty…" />
          </Field>
          <div className="grid grid-cols-2 gap-4">
            <Field label="Project workspace">
              <select className="input w-full" value={draft.projectId} onChange={(e) => setDraft({ ...draft, projectId: e.target.value })}>
                {projects.map((project) => <option key={project.id} value={project.id}>{project.name}</option>)}
              </select>
            </Field>
            <Field label="Model override" hint="Leave blank to follow Settings.">
              <input className="input w-full" value={draft.model} onChange={(e) => setDraft({ ...draft, model: e.target.value })} placeholder="App default" />
            </Field>
          </div>
          <div>
            <div className="text-[11px] font-medium text-ink2">Avatar</div>
            <div className="mt-2 flex flex-wrap gap-2">
              {AVATARS.map((avatar) => <button key={avatar} className={`grid h-9 w-9 place-items-center rounded-lg text-xl ${draft.avatar === avatar ? 'ring-2 ring-brand' : 'bg-surface2'}`} onClick={() => setDraft({ ...draft, avatar })}>{avatar}</button>)}
            </div>
          </div>
          <div>
            <div className="text-[11px] font-medium text-ink2">Color</div>
            <div className="mt-2 flex gap-2">
              {COLORS.map((color) => <button key={color} aria-label={`Color ${color}`} className={`h-7 w-7 rounded-full ${draft.color === color ? 'ring-2 ring-offset-2 ring-offset-surface' : ''}`} style={{ backgroundColor: color }} onClick={() => setDraft({ ...draft, color })} />)}
            </div>
          </div>
          {error && <p role="alert" className="rounded bg-dangerSoft px-3 py-2 text-[11px] text-danger">{error}</p>}
        </div>
        <footer className="flex items-center border-t border-border px-5 py-3">
          {bot && <button className="btn-ghost text-danger" onClick={() => void remove()} disabled={saving}><IconTrash width={13} /> Delete</button>}
          <div className="ml-auto flex gap-2">
            <button className="btn-outline" onClick={onClose} disabled={saving}>Cancel</button>
            <button className="btn-primary" onClick={() => void save()} disabled={!valid || saving}>{saving ? 'Saving…' : bot ? 'Save changes' : 'Create Agent'}</button>
          </div>
        </footer>
      </section>
    </div>
  )
}

function Field({ label, hint, children }: { label: string; hint?: string; children: ReactNode }) {
  return <label className="block"><span className="text-[11px] font-medium text-ink2">{label}</span>{hint && <span className="ml-1 text-[10px] text-ink3">{hint}</span>}<span className="mt-1 block">{children}</span></label>
}
