// Settings 弹层：LLM 配置读写后端 /api/settings；MCP server 暂存 localStorage。
import { useEffect, useState } from 'react'
import type { A2aAgentSettings, AppSettings, AppSettingsProvider, McpServer } from '../types'
import { getSettings, listMcpServers, saveMcpServers, saveSettings } from '../api/client'
import {
  backendReflectsSettings,
  buildSettingsPayload,
  createA2aAgentDraft,
  sanitizeSettings,
  settingsSaveNotice,
  type SettingsSaveNotice,
} from '../api/settingsState'
import { useApp } from '../App'
import Modal from './Modal'
import Toggle from './Toggle'
import { IconCpu, IconKey, IconSettings } from './icons'

type SectionId = 'llm' | 'a2a' | 'mcp' | 'general'

const SECTIONS: { id: SectionId; label: string; icon: React.ReactNode }[] = [
  { id: 'llm', label: 'LLM Providers', icon: <IconKey width={14} height={14} /> },
  { id: 'a2a', label: 'A2A Agents', icon: <IconCpu width={14} height={14} /> },
  { id: 'mcp', label: 'MCP Servers', icon: <IconCpu width={14} height={14} /> },
  { id: 'general', label: 'General', icon: <IconSettings width={14} height={14} /> },
]

export default function SettingsModal({ onClose }: { onClose: () => void }) {
  const [section, setSection] = useState<SectionId>('llm')
  return (
    <Modal title="Settings" onClose={onClose} width="max-w-2xl">
      <div className="flex min-h-[380px] max-h-[72vh] overflow-hidden">
        {/* 左侧导航 */}
        <nav className="w-44 shrink-0 space-y-0.5 border-r border-border p-2">
          {SECTIONS.map((s) => (
            <button
              key={s.id}
              onClick={() => setSection(s.id)}
              className={`flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-[13px] ${
                section === s.id ? 'bg-brandSoft font-medium text-brand' : 'text-ink2 hover:bg-surface2 hover:text-ink'
              }`}
            >
              {s.icon}
              {s.label}
            </button>
          ))}
        </nav>
        <div className="min-w-0 flex-1 overflow-y-auto p-4">
          {section === 'llm' && <LlmSection />}
          {section === 'a2a' && <A2aSection />}
          {section === 'mcp' && <McpSection />}
          {section === 'general' && <GeneralSection />}
        </div>
      </div>
    </Modal>
  )
}

// ---------- LLM Providers ----------
function LlmSection() {
  const { refreshBackend } = useApp()
  const [settings, setSettings] = useState<AppSettings | null>(null)
  const [keyDrafts, setKeyDrafts] = useState<Record<string, string>>({})
  const [loadError, setLoadError] = useState<string | null>(null)
  const [saveError, setSaveError] = useState<string | null>(null)
  const [refreshError, setRefreshError] = useState<string | null>(null)
  const [saveNotice, setSaveNotice] = useState<SettingsSaveNotice | null>(null)
  const [saving, setSaving] = useState(false)

  const load = async () => {
    setLoadError(null)
    try {
      const next = sanitizeSettings(await getSettings())
      setSettings(next)
      setKeyDrafts({})
    } catch (error) {
      setLoadError(errorMessage(error))
    }
  }

  useEffect(() => {
    void load()
  }, [])

  if (!settings) {
    if (loadError) {
      return (
        <div className="card space-y-3 p-4 text-[12px]">
          <p role="alert" className="text-danger">
            设置加载失败：{loadError}
          </p>
          <button className="btn-outline" onClick={() => void load()}>
            重试
          </button>
        </div>
      )
    }
    return <div className="py-8 text-center text-[12px] text-ink3">加载中…</div>
  }

  const clearSaveFeedback = () => {
    setSaveError(null)
    setRefreshError(null)
    setSaveNotice(null)
  }

  const patch = (name: string, p: Partial<AppSettingsProvider>) => {
    clearSaveFeedback()
    setSettings((current) =>
      current
        ? {
            ...current,
            providers: current.providers.map((provider) =>
              provider.name === name ? { ...provider, ...p } : provider,
            ),
          }
        : current,
    )
  }

  const patchKeyDraft = (name: string, value: string) => {
    clearSaveFeedback()
    setKeyDrafts((drafts) => ({ ...drafts, [name]: value }))
  }

  const save = async () => {
    setSaving(true)
    setSaveError(null)
    setRefreshError(null)
    setSaveNotice(null)

    const payload = buildSettingsPayload(settings, keyDrafts)

    try {
      const saved = sanitizeSettings(await saveSettings(payload))
      setSettings(saved)
      setKeyDrafts({})

      try {
        const refreshed = await refreshBackend()
        if (backendReflectsSettings(saved, refreshed)) {
          setSaveNotice(settingsSaveNotice(saved, refreshed))
          return
        }
        setRefreshError('设置已保存，但运行状态刷新失败。请检查后端连接后重试。')
      } catch {
        setRefreshError('设置已保存，但运行状态刷新失败。请检查后端连接后重试。')
      }
    } catch (error) {
      setSaveError(errorMessage(error))
    } finally {
      setSaving(false)
    }
  }

  return (
    <div className="space-y-4">
      {settings.providers.map((p) => (
        <div key={p.name} className="card p-3">
          <div className="flex items-center justify-between">
            <span className="text-[13px] font-medium text-ink">{p.name}</span>
            <Toggle checked={p.enabled} onChange={(v) => patch(p.name, { enabled: v })} />
          </div>
          <div className="mt-3 space-y-2">
            <label className="block">
              <span className="mb-1 block text-[11px] text-ink3">Base URL</span>
              <input className="input py-1.5" value={p.base_url} onChange={(e) => patch(p.name, { base_url: e.target.value })} />
            </label>
            <label className="block">
              <span className="mb-1 block text-[11px] text-ink3">Model</span>
              <input
                className="input py-1.5"
                value={p.model ?? ''}
                onChange={(e) => patch(p.name, { model: e.target.value })}
              />
            </label>
            <label className="block">
              <span className="mb-1 block text-[11px] text-ink3">API Key</span>
              <input
                className="input py-1.5 font-mono"
                type="password"
                placeholder={p.api_key_masked || 'sk-…'}
                value={keyDrafts[p.name] ?? ''}
                onChange={(e) => patchKeyDraft(p.name, e.target.value)}
                autoComplete="new-password"
              />
              <span className="mt-1 block text-[11px] text-ink3">
                留空会保留已保存的密钥；后端不会返回密钥明文。
              </span>
            </label>
          </div>
        </div>
      ))}
      {settings.providers.length === 0 && (
        <div className="card p-4 text-center text-[12px] text-ink3">后端未返回可配置的 LLM provider。</div>
      )}
      {saveError && (
        <p role="alert" className="rounded-md border border-danger/30 bg-danger/5 px-3 py-2 text-[12px] text-danger">
          保存失败：{saveError}
        </p>
      )}
      {refreshError && (
        <p role="alert" className="rounded-md border border-danger/30 bg-danger/5 px-3 py-2 text-[12px] text-danger">
          {refreshError}
        </p>
      )}
      {saveNotice && (
        <p
          role="status"
          className={`rounded-md border px-3 py-2 text-[12px] ${
            saveNotice.kind === 'warning'
              ? 'border-amber-500/30 bg-amber-500/5 text-amber-500'
              : 'border-success/30 bg-success/5 text-success'
          }`}
        >
          {saveNotice.message}
        </p>
      )}
      <div className="flex items-center justify-end gap-2">
        <button
          className="btn-primary"
          disabled={saving || settings.providers.length === 0}
          onClick={() => void save()}
        >
          {saving ? '保存中…' : '保存'}
        </button>
      </div>
    </div>
  )
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

// ---------- A2A Agents ----------
function A2aSection() {
  const [settings, setSettings] = useState<AppSettings | null>(null)
  const [tokenDrafts, setTokenDrafts] = useState<Record<string, string>>({})
  const [tokenClears, setTokenClears] = useState<Set<string>>(new Set())
  const [loadError, setLoadError] = useState<string | null>(null)
  const [saveError, setSaveError] = useState<string | null>(null)
  const [saveMessage, setSaveMessage] = useState<string | null>(null)
  const [saving, setSaving] = useState(false)

  const load = async () => {
    setLoadError(null)
    try {
      setSettings(sanitizeSettings(await getSettings()))
      setTokenDrafts({})
      setTokenClears(new Set())
    } catch (error) {
      setLoadError(errorMessage(error))
    }
  }

  useEffect(() => {
    void load()
  }, [])

  if (!settings) {
    if (loadError) {
      return (
        <div className="card space-y-3 p-4 text-[12px]">
          <p role="alert" className="text-danger">A2A 设置加载失败：{loadError}</p>
          <button className="btn-outline" onClick={() => void load()}>重试</button>
        </div>
      )
    }
    return <div className="py-8 text-center text-[12px] text-ink3">加载中…</div>
  }

  const agents = settings.a2a_agents ?? []
  const clearFeedback = () => {
    setSaveError(null)
    setSaveMessage(null)
  }

  const patchAgent = (id: string, patch: Partial<A2aAgentSettings>) => {
    clearFeedback()
    setSettings((current) => {
      if (!current) return current
      return {
        ...current,
        a2a_agents: (current.a2a_agents ?? []).map((agent) => {
          if (agent.id !== id) return agent
          const next = { ...agent, ...patch }
          return 'endpoint' in patch
            ? {
                ...next,
                // The backend only preserves credentials while the exact endpoint is unchanged.
                // Clear the stale mask so the UI does not imply otherwise.
                bearer_token_masked: '',
                status: 'unchecked',
                last_error: null,
                last_refreshed_at: null,
                card_summary: null,
              }
            : next
        }),
      }
    })
  }

  const addAgent = () => {
    if (agents.length >= 16) return
    clearFeedback()
    setSettings((current) => current
      ? { ...current, a2a_agents: [...(current.a2a_agents ?? []), createA2aAgentDraft()] }
      : current)
  }

  const removeAgent = (id: string) => {
    clearFeedback()
    setSettings((current) => current
      ? { ...current, a2a_agents: (current.a2a_agents ?? []).filter((agent) => agent.id !== id) }
      : current)
    setTokenDrafts((current) => {
      const { [id]: _removed, ...rest } = current
      return rest
    })
    setTokenClears((current) => {
      const next = new Set(current)
      next.delete(id)
      return next
    })
  }

  const validationError = validateA2aDrafts(agents)

  const save = async () => {
    if (validationError) {
      setSaveError(validationError)
      return
    }
    setSaving(true)
    setSaveError(null)
    setSaveMessage(null)
    try {
      const payload = buildSettingsPayload(settings, {}, tokenDrafts, tokenClears)
      const saved = sanitizeSettings(await saveSettings(payload))
      setSettings(saved)
      setTokenDrafts({})
      setTokenClears(new Set())
      const enabled = (saved.a2a_agents ?? []).filter((agent) => agent.enabled).length
      const ready = (saved.a2a_agents ?? []).filter(
        (agent) => agent.enabled && agent.status === 'ready',
      ).length
      const unavailable = (saved.a2a_agents ?? []).filter(
        (agent) => agent.enabled && agent.status !== 'ready',
      ).length
      setSaveMessage(
        enabled === 0
          ? '已保存。当前没有启用的 A2A Agent，后续请求不会加载远程 Agent 工具。'
          : unavailable > 0
          ? `已保存。${ready} 个 Agent 可用，${unavailable} 个暂不可用；后续调用仍会先刷新 Agent Card。`
          : `已保存。${ready} 个 Agent 已进入后续新请求的工具集；每次调用前都会刷新 Agent Card。`,
      )
    } catch (error) {
      setSaveError(errorMessage(error))
    } finally {
      setSaving(false)
    }
  }

  const hasPlainHttp = agents.some((agent) => /^http:\/\//i.test(agent.endpoint.trim()))

  return (
    <div className="space-y-3" data-settings-section="a2a">
      <div className="rounded-md border border-brand/20 bg-brandSoft px-3 py-2 text-[11px] leading-relaxed text-ink2">
        启用后，每个远程 Agent 会作为独立工具进入 Agent harness。保存时会探测 Agent Card，
        实际调用前还会强制刷新一次；Deepseek Science 仅作为 A2A client，不对外提供 A2A 服务。
      </div>

      {agents.map((agent, index) => {
        const appearance = a2aStatusAppearance(agent.status)
        return (
          <div key={agent.id} className="card space-y-3 p-3" data-a2a-agent-id={agent.id}>
            <div className="flex items-start justify-between gap-3">
              <div className="min-w-0">
                <div className="flex items-center gap-2">
                  <span className="text-[13px] font-medium text-ink">
                    {agent.name.trim() || `新 Agent ${index + 1}`}
                  </span>
                  <span className={`rounded-full px-2 py-0.5 text-[10px] font-medium ${appearance.className}`}>
                    {appearance.label}
                  </span>
                </div>
                <code className="mt-1 block truncate text-[10px] text-ink3">
                  {agent.tool_name || `a2a_agent_${agent.id}`}
                </code>
              </div>
              <div className="flex shrink-0 items-center gap-2">
                <Toggle checked={agent.enabled} onChange={(enabled) => patchAgent(agent.id, { enabled })} />
                <button
                  type="button"
                  className="rounded border border-danger/30 px-2 py-1 text-[11px] text-danger hover:bg-dangerSoft"
                  aria-label={`移除 A2A Agent ${agent.name || index + 1}`}
                  onClick={() => removeAgent(agent.id)}
                >
                  移除
                </button>
              </div>
            </div>

            <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
              <label className="block">
                <span className="mb-1 block text-[11px] text-ink3">本地名称</span>
                <input
                  className="input py-1.5"
                  aria-label={`A2A Agent ${index + 1} 名称`}
                  placeholder="例如 fast-reactor-specialist"
                  value={agent.name}
                  onChange={(event) => patchAgent(agent.id, { name: event.target.value })}
                />
              </label>
              <label className="block">
                <span className="mb-1 block text-[11px] text-ink3">调用超时（秒）</span>
                <input
                  className="input py-1.5"
                  aria-label={`A2A Agent ${index + 1} 调用超时`}
                  type="number"
                  min={5}
                  max={300}
                  step={1}
                  value={agent.timeout_seconds}
                  onChange={(event) => patchAgent(agent.id, { timeout_seconds: Number(event.target.value) })}
                />
              </label>
            </div>
            <label className="block">
              <span className="mb-1 block text-[11px] text-ink3">Agent endpoint</span>
              <input
                className="input py-1.5 font-mono"
                aria-label={`A2A Agent ${index + 1} endpoint`}
                placeholder="http://127.0.0.1:9901"
                value={agent.endpoint}
                onChange={(event) => patchAgent(agent.id, { endpoint: event.target.value })}
              />
            </label>
            <label className="block">
              <span className="mb-1 block text-[11px] text-ink3">Bearer token（可选）</span>
              <span className="flex gap-2">
                <input
                  className="input min-w-0 flex-1 py-1.5 font-mono"
                  aria-label={`A2A Agent ${index + 1} Bearer token`}
                  type="password"
                  autoComplete="new-password"
                  disabled={tokenClears.has(agent.id)}
                  placeholder={tokenClears.has(agent.id) ? '保存后清除' : agent.bearer_token_masked || '未配置'}
                  value={tokenDrafts[agent.id] ?? ''}
                  onChange={(event) => {
                    clearFeedback()
                    setTokenClears((current) => {
                      const next = new Set(current)
                      next.delete(agent.id)
                      return next
                    })
                    setTokenDrafts((current) => ({ ...current, [agent.id]: event.target.value }))
                  }}
                />
                {(agent.bearer_token_masked || tokenClears.has(agent.id)) && (
                  <button
                    type="button"
                    className={tokenClears.has(agent.id) ? 'btn-outline shrink-0' : 'shrink-0 rounded border border-danger/30 px-2 text-[11px] text-danger hover:bg-dangerSoft'}
                    aria-label={`${tokenClears.has(agent.id) ? '撤销清除' : '清除'} A2A Agent ${index + 1} Bearer token`}
                    onClick={() => {
                      clearFeedback()
                      setTokenDrafts((current) => ({ ...current, [agent.id]: '' }))
                      setTokenClears((current) => {
                        const next = new Set(current)
                        if (next.has(agent.id)) next.delete(agent.id)
                        else next.add(agent.id)
                        return next
                      })
                    }}
                  >
                    {tokenClears.has(agent.id) ? '撤销清除' : '清除'}
                  </button>
                )}
              </span>
              <span className="mt-1 block text-[10px] text-ink3">
                {tokenClears.has(agent.id)
                  ? '保存后会删除已保存 token；可在保存前撤销。'
                  : '留空保留已保存 token；明文只会出现在本次保存请求中，后端不会返回。'}
              </span>
            </label>

            {agent.card_summary && (
              <div className="rounded-md border border-border bg-surface/60 p-2.5 text-[11px]" data-a2a-card-summary>
                <div className="font-medium text-ink">{agent.card_summary.name}</div>
                {agent.card_summary.description && (
                  <p className="mt-1 whitespace-pre-wrap text-ink2">{agent.card_summary.description}</p>
                )}
                <div className="mt-1.5 flex flex-wrap gap-x-3 gap-y-1 text-ink3">
                  <span>Agent {agent.card_summary.version || '未知版本'}</span>
                  <span>A2A {agent.card_summary.protocol_version || '未知版本'}</span>
                  <span>
                    已选接口：{agent.card_summary.supported_interfaces[0]?.protocol_binding || '未知'}
                  </span>
                </div>
                {agent.card_summary.skills.length > 0 && (
                  <div className="mt-2 flex flex-wrap gap-1">
                    {agent.card_summary.skills.map((skill) => (
                      <span key={skill} className="rounded bg-bg px-1.5 py-0.5 text-[10px] text-ink2">{skill}</span>
                    ))}
                  </div>
                )}
              </div>
            )}
            {agent.last_error && (
              <p role="alert" className="whitespace-pre-wrap rounded-md border border-danger/30 bg-dangerSoft px-2.5 py-2 text-[11px] text-danger">
                {agent.last_error}
              </p>
            )}
            {agent.last_refreshed_at && (
              <div className="text-[10px] text-ink3">最近刷新：{formatDateTime(agent.last_refreshed_at)}</div>
            )}
          </div>
        )
      })}

      {agents.length === 0 && (
        <div className="card px-3 py-8 text-center text-[12px] text-ink3">
          尚未配置 A2A Agent。添加后，启用的 Agent 会进入后续新请求的工具集。
        </div>
      )}

      {hasPlainHttp && (
        <p className="rounded-md border border-amber-500/30 bg-amber-500/5 px-3 py-2 text-[11px] text-amber-600">
          当前包含未加密的 HTTP endpoint。局域网/本机调试可用，但任务内容和 Bearer token 可能被网络观察者读取。
        </p>
      )}
      {saveError && (
        <p role="alert" className="rounded-md border border-danger/30 bg-dangerSoft px-3 py-2 text-[12px] text-danger">
          保存失败：{saveError}
        </p>
      )}
      {saveMessage && (
        <p role="status" className="rounded-md border border-success/30 bg-success/10 px-3 py-2 text-[12px] text-success">
          {saveMessage}
        </p>
      )}

      <div className="flex items-center justify-between gap-2">
        <button className="btn-outline" disabled={agents.length >= 16 || saving} onClick={addAgent}>
          添加 A2A Agent{agents.length >= 16 ? '（已达 16 个上限）' : ''}
        </button>
        <button className="btn-primary" disabled={saving} onClick={() => void save()}>
          {saving ? '保存并刷新 Agent Card…' : '保存 A2A 设置'}
        </button>
      </div>
    </div>
  )
}

function validateA2aDrafts(agents: A2aAgentSettings[]): string | null {
  if (agents.length > 16) return '最多只能配置 16 个 A2A Agent。'
  const ids = new Set<string>()
  const names = new Set<string>()
  for (const agent of agents) {
    const name = agent.name.trim()
    if (!name) return '每个 A2A Agent 都需要本地名称。'
    if (!agent.endpoint.trim()) return `${name} 缺少 endpoint。`
    if (!Number.isInteger(agent.timeout_seconds) || agent.timeout_seconds < 5 || agent.timeout_seconds > 300) {
      return `${name} 的调用超时必须是 5–300 秒之间的整数。`
    }
    if (ids.has(agent.id)) return 'A2A Agent id 重复，请移除重复项后再保存。'
    const normalizedName = name.toLocaleLowerCase()
    if (names.has(normalizedName)) return `A2A Agent 名称重复：${name}。`
    ids.add(agent.id)
    names.add(normalizedName)
  }
  return null
}

function a2aStatusAppearance(status: string): { label: string; className: string } {
  switch (status) {
    case 'ready':
      return { label: '可用', className: 'bg-success/10 text-success' }
    case 'disabled':
      return { label: '已停用', className: 'bg-surface text-ink3' }
    case 'unreachable':
      return { label: '无法连接', className: 'bg-dangerSoft text-danger' }
    case 'invalid':
      return { label: 'Card 无效', className: 'bg-dangerSoft text-danger' }
    case 'unsupported':
      return { label: '协议不支持', className: 'bg-amber-500/10 text-amber-600' }
    default:
      return { label: '待检查', className: 'bg-brandSoft text-brand' }
  }
}

function formatDateTime(value: string): string {
  const date = new Date(value)
  return Number.isNaN(date.valueOf()) ? value : date.toLocaleString()
}

// ---------- MCP Servers ----------
function McpSection() {
  const [servers, setServers] = useState<McpServer[] | null>(null)
  const [name, setName] = useState('')
  const [url, setUrl] = useState('')

  useEffect(() => {
    void listMcpServers().then(setServers)
  }, [])

  if (!servers) return <div className="py-8 text-center text-[12px] text-ink3">加载中…</div>

  const persist = (next: McpServer[]) => {
    setServers(next)
    void saveMcpServers(next) // TODO: 接后端
  }

  const add = () => {
    const n = name.trim()
    const u = url.trim()
    if (!n || !u) return
    persist([...servers, { name: n, url: u, enabled: true, connected: false }])
    setName('')
    setUrl('')
  }

  return (
    <div className="space-y-3">
      <div className="card divide-y divide-border">
        {servers.map((s) => (
          <div key={s.name} className="flex items-center gap-3 px-3 py-2.5">
            <span
              className={`h-1.5 w-1.5 shrink-0 rounded-full ${s.connected ? 'bg-success' : 'bg-borderStrong'}`}
              title={s.connected ? 'connected' : 'disconnected'}
            />
            <div className="min-w-0 flex-1">
              <div className="truncate text-[13px] font-medium text-ink">{s.name}</div>
              <div className="truncate font-mono text-[11px] text-ink3">{s.url}</div>
            </div>
            <span className="text-[11px] text-ink3">{s.connected ? 'connected' : 'disconnected'}</span>
            <Toggle
              checked={s.enabled}
              onChange={(v) => persist(servers.map((x) => (x.name === s.name ? { ...x, enabled: v } : x)))}
            />
          </div>
        ))}
        {servers.length === 0 && <div className="px-3 py-6 text-center text-[12px] text-ink3">暂无 MCP server</div>}
      </div>

      {/* 添加表单（前端态） */}
      <div className="card space-y-2 p-3">
        <div className="text-[12px] font-medium text-ink2">Add server</div>
        <input className="input py-1.5" placeholder="名称，如 literature-search" value={name} onChange={(e) => setName(e.target.value)} />
        <input
          className="input py-1.5 font-mono"
          placeholder="URL，如 http://127.0.0.1:8901/sse"
          value={url}
          onChange={(e) => setUrl(e.target.value)}
        />
        <div className="flex justify-end">
          <button className="btn-outline" disabled={!name.trim() || !url.trim()} onClick={add}>
            添加
          </button>
        </div>
      </div>
    </div>
  )
}

// ---------- General ----------
function GeneralSection() {
  const { theme, toggleTheme } = useApp()
  const [workspace, setWorkspace] = useState<string | null>(null)
  const [workspaceError, setWorkspaceError] = useState<string | null>(null)

  useEffect(() => {
    let cancelled = false
    void getSettings()
      .then((settings) => {
        if (!cancelled) setWorkspace(settings.default_workspace)
      })
      .catch((error) => {
        if (!cancelled) setWorkspaceError(errorMessage(error))
      })
    return () => {
      cancelled = true
    }
  }, [])

  return (
    <div className="space-y-4">
      <div className="card p-3">
        <div className="text-[13px] font-medium text-ink">主题</div>
        <div className="mt-2 flex gap-2">
          {(['light', 'dark'] as const).map((t) => (
            <button
              key={t}
              onClick={() => theme !== t && toggleTheme()}
              className={`rounded-md border px-3 py-1.5 text-[12px] ${
                theme === t ? 'border-brand bg-brandSoft font-medium text-brand' : 'border-border text-ink2 hover:bg-surface2'
              }`}
            >
              {t === 'light' ? '亮色' : '深色'}
            </button>
          ))}
        </div>
      </div>
      <div className="card p-3">
        <div className="text-[13px] font-medium text-ink">默认工作区</div>
        <input
          className="input mt-2 py-1.5 font-mono"
          value={workspace ?? (workspaceError ? '读取失败' : '加载中…')}
          readOnly
        />
        {workspaceError ? (
          <p className="mt-1 text-[11px] text-danger">设置加载失败：{workspaceError}</p>
        ) : (
          <p className="mt-1 text-[11px] text-ink3">当前后端配置的会话工作区根目录（只读展示）。</p>
        )}
      </div>
    </div>
  )
}
