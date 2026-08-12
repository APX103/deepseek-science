// Settings 弹层：LLM / MCP / Skills 配置均读写后端 /api/settings。
import { useEffect, useState } from 'react'
import type {
  A2aAgentSettings,
  AppSettings,
  AppSettingsProvider,
  McpServer,
  Skill,
  SkillSettingsValue,
  ThinkingEffort,
} from '../types'
import { getSettings, listSkills, saveSettings } from '../api/client'
import {
  backendReflectsSettings,
  buildSettingsPayload,
  canSubmitDataSourceKey,
  createA2aAgentDraft,
  DEFAULT_MAX_ITERATIONS,
  DEFAULT_THINKING_EFFORT,
  DEFAULT_THINKING_ENABLED,
  isDataSourceKeyConfigured,
  isThinkingEffort,
  MAX_CONFIGURABLE_ITERATIONS,
  MIN_MAX_ITERATIONS,
  normalizeMcpServers,
  normalizeSkillSettings,
  normalizeMaxIterations,
  normalizeThinkingSettings,
  parseMcpJsonConfig,
  parseMaxIterationsDraft,
  reconcileAgentThinkingSaveResponse,
  sanitizeSettings,
  settingsSaveNotice,
  type AgentThinkingValues,
  type SettingsSaveNotice,
} from '../api/settingsState'
import { useApp } from '../App'
import Modal from './Modal'
import Toggle from './Toggle'
import { IconCpu, IconKey, IconSearch, IconSettings, IconZap } from './icons'

type SectionId = 'llm' | 'a2a' | 'mcp' | 'skills' | 'general'

const SECTIONS: { id: SectionId; label: string; icon: React.ReactNode }[] = [
  { id: 'llm', label: 'LLM Providers', icon: <IconKey width={14} height={14} /> },
  { id: 'a2a', label: 'A2A Agents', icon: <IconCpu width={14} height={14} /> },
  { id: 'mcp', label: 'MCP Servers', icon: <IconCpu width={14} height={14} /> },
  { id: 'skills', label: 'Skills', icon: <IconZap width={14} height={14} /> },
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
          {section === 'skills' && <SkillsSection />}
          {section === 'general' && <GeneralSection />}
        </div>
      </div>
    </Modal>
  )
}

// ---------- LLM Providers ----------
const DEFAULT_PROVIDERS: Omit<AppSettingsProvider, 'id' | 'enabled'>[] = [
  { name: 'DeepSeek', base_url: 'https://api.deepseek.com', model: 'deepseek-v4-flash', api_key_masked: '' },
  { name: 'OpenAI', base_url: 'https://api.openai.com/v1', model: 'gpt-4o', api_key_masked: '' },
  { name: '自定义', base_url: 'https://api.example.com/v1', model: '', api_key_masked: '' },
]

function generateProviderId(): string {
  if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
    return crypto.randomUUID()
  }
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`
}

function createProviderDraft(templateIndex = 2): AppSettingsProvider {
  const template = DEFAULT_PROVIDERS[templateIndex % DEFAULT_PROVIDERS.length]
  return {
    id: generateProviderId(),
    name: template.name,
    base_url: template.base_url,
    model: template.model,
    api_key_masked: '',
    enabled: false,
  }
}

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

  const patch = (id: string, p: Partial<AppSettingsProvider>) => {
    clearSaveFeedback()
    setSettings((current) => {
      if (!current) return current
      const nextProviders = current.providers.map((provider) =>
        provider.id === id ? { ...provider, ...p } : provider,
      )
      // 单选约束：启用一个时自动禁用其它 provider。
      if (p.enabled) {
        for (const provider of nextProviders) {
          if (provider.id !== id) provider.enabled = false
        }
      }
      return { ...current, providers: nextProviders }
    })
  }

  const patchKeyDraft = (id: string, value: string) => {
    clearSaveFeedback()
    setKeyDrafts((drafts) => ({ ...drafts, [id]: value }))
  }

  const addProvider = () => {
    clearSaveFeedback()
    setSettings((current) => {
      if (!current) return current
      const draft = createProviderDraft(current.providers.length)
      // 如果当前没有启用项，新 provider 默认启用。
      const hasEnabled = current.providers.some((p) => p.enabled)
      draft.enabled = !hasEnabled
      return { ...current, providers: [...current.providers, draft] }
    })
  }

  const removeProvider = (id: string) => {
    clearSaveFeedback()
    setSettings((current) => {
      if (!current) return current
      const next = current.providers.filter((p) => p.id !== id)
      // 删除后若没有任何启用项，自动启用第一个。
      if (next.length > 0 && !next.some((p) => p.enabled)) {
        next[0].enabled = true
      }
      return { ...current, providers: next }
    })
    setKeyDrafts((drafts) => {
      const { [id]: _removed, ...rest } = drafts
      return rest
    })
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

  const enabledCount = settings.providers.filter((p) => p.enabled).length

  return (
    <div className="space-y-4">
      {settings.providers.map((p, index) => (
        <div key={p.id} className="card p-3">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-2">
              <span className="text-[13px] font-medium text-ink">{p.name || `Provider ${index + 1}`}</span>
              {p.enabled && (
                <span className="rounded-full bg-success/10 px-2 py-0.5 text-[10px] font-medium text-success">
                  已启用
                </span>
              )}
            </div>
            <div className="flex items-center gap-2">
              <Toggle checked={p.enabled} onChange={(v) => patch(p.id, { enabled: v })} />
              <button
                type="button"
                className="rounded border border-danger/30 px-2 py-1 text-[11px] text-danger hover:bg-dangerSoft"
                aria-label={`移除 provider ${p.name || index + 1}`}
                disabled={settings.providers.length <= 1}
                onClick={() => removeProvider(p.id)}
              >
                移除
              </button>
            </div>
          </div>
          <div className="mt-3 space-y-2">
            <label className="block">
              <span className="mb-1 block text-[11px] text-ink3">名称</span>
              <input
                className="input py-1.5"
                value={p.name}
                onChange={(e) => patch(p.id, { name: e.target.value })}
              />
            </label>
            <label className="block">
              <span className="mb-1 block text-[11px] text-ink3">Base URL</span>
              <input className="input py-1.5 font-mono" value={p.base_url} onChange={(e) => patch(p.id, { base_url: e.target.value })} />
            </label>
            <label className="block">
              <span className="mb-1 block text-[11px] text-ink3">Model</span>
              <input
                className="input py-1.5"
                value={p.model ?? ''}
                onChange={(e) => patch(p.id, { model: e.target.value })}
              />
            </label>
            <label className="block">
              <span className="mb-1 block text-[11px] text-ink3">API Key</span>
              <input
                className="input py-1.5 font-mono"
                type="password"
                placeholder={p.api_key_masked || 'sk-…'}
                value={keyDrafts[p.id] ?? ''}
                onChange={(e) => patchKeyDraft(p.id, e.target.value)}
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
      {enabledCount !== 1 && (
        <p role="alert" className="rounded-md border border-amber-500/30 bg-amber-500/5 px-3 py-2 text-[12px] text-amber-600">
          必须且只能启用一个 provider（当前 {enabledCount} 个）。
        </p>
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
      <div className="flex items-center justify-between gap-2">
        <button
          className="btn-outline"
          disabled={saving || settings.providers.length >= 8}
          onClick={addProvider}
        >
          添加 provider{settings.providers.length >= 8 ? '（已达 8 个上限）' : ''}
        </button>
        <button
          className="btn-primary"
          disabled={saving || settings.providers.length === 0 || enabledCount !== 1}
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
type McpEditorMode = 'form' | 'json'

const EMPTY_MCP_DRAFT = { name: '', url: '', enabled: true }

const JSON_PLACEHOLDER = `支持三种写法：
1. 单个 server：{ "name": "lit", "url": "http://127.0.0.1:8901/sse" }
2. server 数组：[ { "name": …, "url": … }, … ]
3. Claude 风格：{ "mcpServers": { "lit": { "url": "http://…" } } }
仅支持 http(s) 接入；command/stdio 方式暂不支持。`

function McpSection() {
  const [settings, setSettings] = useState<AppSettings | null>(null)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [saveError, setSaveError] = useState<string | null>(null)
  const [saveMessage, setSaveMessage] = useState<string | null>(null)
  const [saving, setSaving] = useState(false)

  // 编辑器状态：form 直接配置 / json 粘贴配置；editing 为正在编辑的原始名称（null = 新增）。
  const [mode, setMode] = useState<McpEditorMode>('form')
  const [editing, setEditing] = useState<string | null>(null)
  const [draft, setDraft] = useState(EMPTY_MCP_DRAFT)
  const [jsonText, setJsonText] = useState('')
  const [editorError, setEditorError] = useState<string | null>(null)

  const load = async () => {
    setLoadError(null)
    try {
      setSettings(sanitizeSettings(await getSettings()))
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
          <p role="alert" className="text-danger">MCP 设置加载失败：{loadError}</p>
          <button className="btn-outline" onClick={() => void load()}>重试</button>
        </div>
      )
    }
    return <div className="py-8 text-center text-[12px] text-ink3">加载中…</div>
  }

  const servers = normalizeMcpServers(settings.mcp_servers)

  const clearFeedback = () => {
    setSaveError(null)
    setSaveMessage(null)
  }

  const patchServers = (next: McpServer[]) => {
    clearFeedback()
    setSettings((current) => (current ? { ...current, mcp_servers: next } : current))
  }

  const resetEditor = () => {
    setEditing(null)
    setDraft(EMPTY_MCP_DRAFT)
    setJsonText('')
    setEditorError(null)
  }

  /** 编辑已有 server：回填表单并进入编辑态。 */
  const startEdit = (server: McpServer) => {
    setMode('form')
    setEditing(server.name)
    setDraft({ name: server.name, url: server.url, enabled: server.enabled })
    setEditorError(null)
  }

  const removeServer = (name: string) => {
    if (editing === name) resetEditor()
    patchServers(servers.filter((s) => s.name !== name))
  }

  const toggleServer = (name: string, enabled: boolean) => {
    patchServers(servers.map((s) => (s.name === name ? { ...s, enabled } : s)))
  }

  /** 保留同名同 URL 条目的连接状态，避免本地编辑后闪断显示。 */
  const keepLiveState = (entry: McpServer, previous?: McpServer): McpServer =>
    previous && previous.url === entry.url
      ? { ...entry, connected: previous.connected, tool_count: previous.tool_count ?? null }
      : entry

  /** 表单模式：校验并写入本地列表（最终保存由「保存 MCP 设置」提交后端）。 */
  const applyForm = () => {
    const name = draft.name.trim()
    const url = draft.url.trim()
    if (!name) return setEditorError('请填写 server 名称')
    if (!url.startsWith('http://') && !url.startsWith('https://')) {
      return setEditorError('URL 必须以 http:// 或 https:// 开头')
    }
    const clash = servers.some(
      (s) => s.name.toLowerCase() === name.toLowerCase() && s.name !== editing,
    )
    if (clash) return setEditorError(`已存在同名 server：${name}`)

    const previous = editing ? servers.find((s) => s.name === editing) : undefined
    const entry = keepLiveState(
      { name, url, enabled: draft.enabled, connected: false, tool_count: null },
      previous,
    )
    patchServers(editing ? servers.map((s) => (s.name === editing ? entry : s)) : [...servers, entry])
    resetEditor()
  }

  /** JSON 模式：解析后按名称 upsert 到本地列表。 */
  const applyJson = () => {
    let parsed: ReturnType<typeof parseMcpJsonConfig>
    try {
      parsed = parseMcpJsonConfig(jsonText)
    } catch (error) {
      return setEditorError(errorMessage(error))
    }
    if (parsed.length === 0) return setEditorError('JSON 中没有可添加的 server')

    const next = [...servers]
    for (const p of parsed) {
      const idx = next.findIndex((s) => s.name.toLowerCase() === p.name.toLowerCase())
      const entry = keepLiveState(
        { ...p, connected: false, tool_count: null },
        idx >= 0 ? next[idx] : undefined,
      )
      if (idx >= 0) next[idx] = entry
      else next.push(entry)
    }
    patchServers(next)
    resetEditor()
  }

  const save = async () => {
    setSaving(true)
    clearFeedback()
    try {
      const payload = buildSettingsPayload(settings, {})
      const saved = sanitizeSettings(await saveSettings(payload))
      setSettings(saved)
      const list = normalizeMcpServers(saved.mcp_servers)
      const connected = list.filter((s) => s.connected).length
      setSaveMessage(
        `已保存并即时生效。${list.length} 个 server 中 ${connected} 个已连接，新请求会使用更新后的工具集。`,
      )
    } catch (error) {
      setSaveError(errorMessage(error))
    } finally {
      setSaving(false)
    }
  }

  return (
    <div className="space-y-3">
      {/* server 列表 */}
      <div className="card divide-y divide-border">
        {servers.map((s) => (
          <div key={s.name} className="flex items-center gap-3 px-3 py-2.5">
            <span
              className={`h-1.5 w-1.5 shrink-0 rounded-full ${s.connected ? 'bg-success' : 'bg-borderStrong'}`}
              title={s.connected ? '已连接' : '未连接'}
            />
            <div className="min-w-0 flex-1">
              <div className="truncate text-[13px] font-medium text-ink">{s.name}</div>
              <div className="truncate font-mono text-[11px] text-ink3">{s.url}</div>
            </div>
            <span className="shrink-0 text-[11px] text-ink3">
              {s.connected ? `已连接 · ${s.tool_count ?? 0} 工具` : '未连接'}
            </span>
            <Toggle checked={s.enabled} onChange={(v) => toggleServer(s.name, v)} />
            <button
              type="button"
              className="btn-ghost shrink-0 rounded px-2 py-1 text-[11px] text-ink2"
              aria-label={`编辑 ${s.name}`}
              onClick={() => startEdit(s)}
            >
              编辑
            </button>
            <button
              type="button"
              className="shrink-0 rounded border border-danger/30 px-2 py-1 text-[11px] text-danger hover:bg-dangerSoft"
              aria-label={`删除 ${s.name}`}
              onClick={() => removeServer(s.name)}
            >
              删除
            </button>
          </div>
        ))}
        {servers.length === 0 && (
          <div className="px-3 py-6 text-center text-[12px] text-ink3">暂无 MCP server</div>
        )}
      </div>

      {/* 添加 / 编辑器 */}
      <div className="card space-y-2 p-3">
        <div className="flex items-center gap-2">
          <span className="text-[12px] font-medium text-ink2">
            {editing ? `编辑 server：${editing}` : '添加 server'}
          </span>
          <div className="ml-auto flex overflow-hidden rounded-md border border-border text-[11px]">
            {(['form', 'json'] as const).map((m) => (
              <button
                key={m}
                type="button"
                className={`px-2.5 py-1 ${mode === m ? 'bg-brandSoft text-brand' : 'text-ink3 hover:text-ink'}`}
                aria-pressed={mode === m}
                onClick={() => {
                  setMode(m)
                  setEditorError(null)
                }}
              >
                {m === 'form' ? '直接配置' : 'JSON 配置'}
              </button>
            ))}
          </div>
        </div>

        {mode === 'form' ? (
          <>
            <input
              className="input py-1.5"
              placeholder="名称，如 literature-search"
              value={draft.name}
              onChange={(e) => setDraft({ ...draft, name: e.target.value })}
            />
            <input
              className="input py-1.5 font-mono"
              placeholder="URL，如 http://127.0.0.1:8901/sse"
              value={draft.url}
              onChange={(e) => setDraft({ ...draft, url: e.target.value })}
            />
            <div className="flex items-center justify-between">
              <label className="flex items-center gap-2 text-[12px] text-ink2">
                <Toggle
                  checked={draft.enabled}
                  onChange={(v) => setDraft({ ...draft, enabled: v })}
                />
                启用
              </label>
              <div className="flex gap-2">
                {editing && (
                  <button type="button" className="btn-outline" onClick={resetEditor}>
                    取消
                  </button>
                )}
                <button
                  type="button"
                  className="btn-outline"
                  disabled={!draft.name.trim() || !draft.url.trim()}
                  onClick={applyForm}
                >
                  {editing ? '应用修改' : '添加到列表'}
                </button>
              </div>
            </div>
          </>
        ) : (
          <>
            <textarea
              className="input h-36 resize-y py-1.5 font-mono text-[12px] leading-relaxed"
              placeholder={JSON_PLACEHOLDER}
              value={jsonText}
              onChange={(e) => setJsonText(e.target.value)}
              spellCheck={false}
            />
            <div className="flex justify-end">
              <button
                type="button"
                className="btn-outline"
                disabled={!jsonText.trim()}
                onClick={applyJson}
              >
                解析并添加到列表
              </button>
            </div>
          </>
        )}

        {editorError && (
          <p role="alert" className="rounded-md border border-danger/30 bg-dangerSoft px-3 py-2 text-[12px] text-danger">
            {editorError}
          </p>
        )}
      </div>

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

      <div className="flex items-center justify-between">
        <p className="text-[11px] text-ink3">改动需保存后生效；保存后后端会重新连接 server。</p>
        <button className="btn-primary" disabled={saving} onClick={() => void save()}>
          {saving ? '保存中…' : '保存 MCP 设置'}
        </button>
      </div>
    </div>
  )
}

// ---------- Skills ----------
const SKILL_SOURCE_LABELS: Record<string, string> = {
  builtin: '内置',
  global: '全局',
  project: '项目',
  claude: 'Claude',
  codex: 'Codex',
  cursor: 'Cursor',
  custom: '自定义',
}

function SkillsSection() {
  const [settings, setSettings] = useState<AppSettings | null>(null)
  const [skills, setSkills] = useState<Skill[] | null>(null)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [saveError, setSaveError] = useState<string | null>(null)
  const [saveMessage, setSaveMessage] = useState<string | null>(null)
  const [saving, setSaving] = useState(false)
  const [q, setQ] = useState('')

  const load = async () => {
    setLoadError(null)
    try {
      const [s, list] = await Promise.all([getSettings(), listSkills()])
      setSettings(sanitizeSettings(s))
      setSkills(list)
    } catch (error) {
      setLoadError(errorMessage(error))
    }
  }

  useEffect(() => {
    void load()
  }, [])

  if (!settings || !skills) {
    if (loadError) {
      return (
        <div className="card space-y-3 p-4 text-[12px]">
          <p role="alert" className="text-danger">Skills 设置加载失败：{loadError}</p>
          <button className="btn-outline" onClick={() => void load()}>重试</button>
        </div>
      )
    }
    return <div className="py-8 text-center text-[12px] text-ink3">加载中…</div>
  }

  const skillCfg = normalizeSkillSettings(settings.skills)
  const disabled = new Set(skillCfg.disabled)

  const clearFeedback = () => {
    setSaveError(null)
    setSaveMessage(null)
  }

  const patchSkills = (patch: Partial<SkillSettingsValue>) => {
    clearFeedback()
    setSettings((current) =>
      current ? { ...current, skills: { ...normalizeSkillSettings(current.skills), ...patch } } : current,
    )
  }

  const toggleSkill = (name: string, enabled: boolean) => {
    const next = new Set(disabled)
    if (enabled) next.delete(name)
    else next.add(name)
    patchSkills({ disabled: [...next] })
  }

  const setCustomDir = (index: number, value: string) => {
    const next = [...skillCfg.custom_dirs]
    next[index] = value
    patchSkills({ custom_dirs: next })
  }

  const removeCustomDir = (index: number) => {
    patchSkills({ custom_dirs: skillCfg.custom_dirs.filter((_, i) => i !== index) })
  }

  const addCustomDir = () => {
    patchSkills({ custom_dirs: [...skillCfg.custom_dirs, ''] })
  }

  const save = async () => {
    setSaving(true)
    clearFeedback()
    try {
      const payload = buildSettingsPayload(settings, {})
      const saved = sanitizeSettings(await saveSettings(payload))
      setSettings(saved)
      // 目录/开关变化后重新拉取列表（可能新增或移除了外部/custom skill）。
      setSkills(await listSkills())
      setSaveMessage('已保存并即时生效。后续新请求会使用更新后的 skill 集合。')
    } catch (error) {
      setSaveError(errorMessage(error))
    } finally {
      setSaving(false)
    }
  }

  const filtered = skills.filter(
    (s) =>
      s.name.toLowerCase().includes(q.trim().toLowerCase()) ||
      s.description.toLowerCase().includes(q.trim().toLowerCase()),
  )
  const enabledCount = skills.filter((s) => !disabled.has(s.name)).length

  return (
    <div className="space-y-4">
      {/* 外部 skill 目录开关 */}
      <div className="card p-3">
        <div className="text-[13px] font-medium text-ink">外部 skills 目录</div>
        <p className="mt-1 text-[11px] text-ink3">
          开启后，skill 查找也会扫描对应工具的 skills 目录（各 skill 的 SKILL.md）。
        </p>
        <div className="mt-3 space-y-2">
          <DirToggleRow
            label="Claude Code"
            hint="~/.claude/skills"
            checked={skillCfg.include_claude}
            onChange={(v) => patchSkills({ include_claude: v })}
          />
          <DirToggleRow
            label="Codex"
            hint="~/.codex/skills"
            checked={skillCfg.include_codex}
            onChange={(v) => patchSkills({ include_codex: v })}
          />
          <DirToggleRow
            label="Cursor"
            hint="~/.cursor/skills-cursor · ~/.cursor/skills"
            checked={skillCfg.include_cursor}
            onChange={(v) => patchSkills({ include_cursor: v })}
          />
        </div>
      </div>

      {/* 自定义目录 */}
      <div className="card p-3">
        <div className="text-[13px] font-medium text-ink">自定义 skills 目录</div>
        <p className="mt-1 text-[11px] text-ink3">额外的 skill 根目录（绝对路径），可添加多个。</p>
        <div className="mt-3 space-y-2">
          {skillCfg.custom_dirs.map((dir, index) => (
            <div key={index} className="flex gap-2">
              <input
                className="input min-w-0 flex-1 py-1.5 font-mono"
                placeholder="/绝对/路径/到/skills"
                value={dir}
                onChange={(e) => setCustomDir(index, e.target.value)}
              />
              <button
                type="button"
                className="shrink-0 rounded border border-danger/30 px-2 text-[11px] text-danger hover:bg-dangerSoft"
                aria-label={`移除自定义目录 ${index + 1}`}
                onClick={() => removeCustomDir(index)}
              >
                移除
              </button>
            </div>
          ))}
          {skillCfg.custom_dirs.length === 0 && (
            <div className="text-[12px] text-ink3">暂无自定义目录。</div>
          )}
          <button className="btn-outline" onClick={addCustomDir}>添加目录</button>
        </div>
      </div>

      {/* skill 列表与开关 */}
      <div className="card">
        <div className="flex items-center gap-2 border-b border-border px-3 py-2.5">
          <span className="text-[13px] font-medium text-ink">
            全部 skills（{enabledCount}/{skills.length} 启用）
          </span>
          <div className="ml-auto flex items-center gap-2 rounded-md border border-border px-2 py-1">
            <IconSearch width={12} height={12} className="text-ink3" />
            <input
              value={q}
              onChange={(e) => setQ(e.target.value)}
              placeholder="搜索 skill…"
              className="w-32 bg-transparent text-[12px] outline-none placeholder:text-ink3"
            />
          </div>
        </div>
        <ul className="divide-y divide-border px-3">
          {filtered.map((s) => (
            <li key={`${s.source}:${s.name}`} className="flex items-center gap-3 py-2.5">
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-2">
                  <span className="truncate text-[13px] font-medium text-ink">{s.name}</span>
                  <span className="shrink-0 rounded bg-surface2 px-1.5 py-0.5 text-[10px] text-ink3">
                    {SKILL_SOURCE_LABELS[s.source] ?? s.source}
                  </span>
                </div>
                <div className="truncate text-[12px] text-ink2">{s.description}</div>
              </div>
              <Toggle checked={!disabled.has(s.name)} onChange={(v) => toggleSkill(s.name, v)} />
            </li>
          ))}
          {filtered.length === 0 && (
            <li className="py-10 text-center text-[12px] text-ink3">
              {skills.length === 0 ? '未发现可用 skill。' : '没有匹配的 skill。'}
            </li>
          )}
        </ul>
      </div>

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

      <div className="flex items-center justify-end gap-2">
        <button className="btn-primary" disabled={saving} onClick={() => void save()}>
          {saving ? '保存中…' : '保存 Skills 设置'}
        </button>
      </div>
    </div>
  )
}

function DirToggleRow({
  label,
  hint,
  checked,
  onChange,
}: {
  label: string
  hint: string
  checked: boolean
  onChange: (v: boolean) => void
}) {
  return (
    <div className="flex items-center gap-3">
      <div className="min-w-0 flex-1">
        <div className="text-[13px] text-ink">{label}</div>
        <code className="block truncate text-[11px] text-ink3">{hint}</code>
      </div>
      <Toggle checked={checked} onChange={onChange} />
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
      <AgentThinkingCard />
      <LogRetentionCard />
      <AcademicKeysCard />
    </div>
  )
}

// ---------- Agent Think / 思考强度 / 最大迭代次数 ----------
interface AgentThinkingEditorProps {
  thinkingEnabled: boolean
  effort: ThinkingEffort
  maxIterationsDraft: string
  saving: boolean
  canSave: boolean
  validationError: string | null
  saveError: string | null
  saveSucceeded: boolean
  onThinkingEnabledChange: (value: boolean) => void
  onEffortChange: (value: ThinkingEffort) => void
  onMaxIterationsDraftChange: (value: string) => void
  onSave: () => void
}

export function AgentThinkingEditor({
  thinkingEnabled,
  effort,
  maxIterationsDraft,
  saving,
  canSave,
  validationError,
  saveError,
  saveSucceeded,
  onThinkingEnabledChange,
  onEffortChange,
  onMaxIterationsDraftChange,
  onSave,
}: AgentThinkingEditorProps) {
  return (
    <div className="card p-3" data-agent-thinking-settings="true">
      <div className="flex items-start justify-between gap-4">
        <div>
          <div id="agent-thinking-enabled-label" className="text-[13px] font-medium text-ink">
            Think
          </div>
          <p id="agent-thinking-enabled-description" className="mt-0.5 text-[11px] leading-relaxed text-ink3">
            为支持该能力的模型请求显式推理。具体效果取决于 provider 和模型。
          </p>
        </div>
        <button
          type="button"
          role="switch"
          aria-checked={thinkingEnabled}
          aria-labelledby="agent-thinking-enabled-label"
          aria-describedby="agent-thinking-enabled-description"
          disabled={saving}
          onClick={() => onThinkingEnabledChange(!thinkingEnabled)}
          className={`relative mt-0.5 h-5 w-9 shrink-0 rounded-full transition-colors disabled:cursor-not-allowed disabled:opacity-50 ${
            thinkingEnabled ? 'bg-brand' : 'bg-borderStrong'
          }`}
        >
          <span
            aria-hidden="true"
            className={`absolute top-0.5 h-4 w-4 rounded-full bg-white transition-all ${
              thinkingEnabled ? 'left-[18px]' : 'left-0.5'
            }`}
          />
        </button>
      </div>

      <label className="mt-3 block">
        <span className="text-[12px] font-medium text-ink2">思考深度</span>
        <select
          className="input mt-1.5 py-1.5"
          value={effort}
          disabled={saving || !thinkingEnabled}
          onChange={(event) => {
            if (isThinkingEffort(event.target.value)) onEffortChange(event.target.value)
          }}
        >
          <option value="low">低</option>
          <option value="high">高</option>
          <option value="max">最大</option>
        </select>
      </label>
      {!thinkingEnabled && (
        <p className="mt-1 text-[11px] text-ink3">已保留当前思考深度；重新开启 Think 后继续使用。</p>
      )}

      <label className="block">
        <span className="mt-3 block text-[12px] font-medium text-ink2">最大思考轮次</span>
        <input
          type="number"
          min={MIN_MAX_ITERATIONS}
          max={MAX_CONFIGURABLE_ITERATIONS}
          step={1}
          className="input mt-1.5 py-1.5"
          value={maxIterationsDraft}
          disabled={saving}
          onChange={(event) => onMaxIterationsDraftChange(event.target.value)}
        />
      </label>
      {validationError && (
        <p role="alert" className="mt-1 text-[11px] text-danger">{validationError}</p>
      )}
      <p className="mt-1 text-[11px] leading-relaxed text-ink3">
        每次 Agent 运行的模型/工具迭代总上限，不是模型的推理深度，默认 {DEFAULT_MAX_ITERATIONS}。
        后续新请求立即生效；已经运行中的请求保持开始时的设置。更高的思考深度或轮次会增加耗时和费用。
      </p>
      <div className="mt-2 flex items-center gap-2">
        <button type="button" className="btn-primary" disabled={!canSave} onClick={onSave}>
          {saving ? '保存中…' : '保存'}
        </button>
        {saveSucceeded && (
          <span role="status" className="text-[11px] text-success">
            已保存，后续新请求立即生效
          </span>
        )}
        {saveError && <span role="alert" className="text-[11px] text-danger">{saveError}</span>}
      </div>
    </div>
  )
}

const DEFAULT_AGENT_THINKING_VALUES: AgentThinkingValues = {
  thinking: {
    enabled: DEFAULT_THINKING_ENABLED,
    effort: DEFAULT_THINKING_EFFORT,
  },
  maxIterations: DEFAULT_MAX_ITERATIONS,
}

function agentThinkingValues(settings: AppSettings): AgentThinkingValues {
  return {
    thinking: normalizeThinkingSettings(settings.thinking),
    maxIterations: normalizeMaxIterations(settings.max_iterations),
  }
}

function AgentThinkingCard() {
  const [settings, setSettings] = useState<AppSettings | null>(null)
  const [thinkingEnabledDraft, setThinkingEnabledDraft] = useState(DEFAULT_THINKING_ENABLED)
  const [effortDraft, setEffortDraft] = useState<ThinkingEffort>(DEFAULT_THINKING_EFFORT)
  const [maxIterationsDraft, setMaxIterationsDraft] = useState(String(DEFAULT_MAX_ITERATIONS))
  const [confirmedValues, setConfirmedValues] = useState<AgentThinkingValues>(
    DEFAULT_AGENT_THINKING_VALUES,
  )
  const [loadError, setLoadError] = useState<string | null>(null)
  const [saving, setSaving] = useState(false)
  const [saveError, setSaveError] = useState<string | null>(null)
  const [saveSucceeded, setSaveSucceeded] = useState(false)

  const load = async () => {
    setLoadError(null)
    setSaveError(null)
    setSaveSucceeded(false)
    try {
      const loaded = sanitizeSettings(await getSettings())
      setSettings(loaded)
      const loadedValues = agentThinkingValues(loaded)
      setConfirmedValues(loadedValues)
      setThinkingEnabledDraft(loadedValues.thinking.enabled)
      setEffortDraft(loadedValues.thinking.effort)
      setMaxIterationsDraft(String(loadedValues.maxIterations))
    } catch (error) {
      setLoadError(errorMessage(error))
    }
  }

  useEffect(() => {
    void load()
  }, [])

  useEffect(() => {
    if (!saveSucceeded) return
    const timer = window.setTimeout(() => setSaveSucceeded(false), 4000)
    return () => window.clearTimeout(timer)
  }, [saveSucceeded])

  const parsedMaxIterations = parseMaxIterationsDraft(maxIterationsDraft)
  const validationError = parsedMaxIterations === null
    ? `请输入 ${MIN_MAX_ITERATIONS}–${MAX_CONFIGURABLE_ITERATIONS} 之间的十进制整数（不支持小数或科学计数法）。`
    : null
  const dirty = settings !== null
    && parsedMaxIterations !== null
    && (
      thinkingEnabledDraft !== confirmedValues.thinking.enabled
      || effortDraft !== confirmedValues.thinking.effort
      || parsedMaxIterations !== confirmedValues.maxIterations
    )
  const canSave = settings !== null && !saving && validationError === null && dirty

  const save = async () => {
    if (!settings || parsedMaxIterations === null || !canSave) return
    const requested: AgentThinkingValues = {
      thinking: { enabled: thinkingEnabledDraft, effort: effortDraft },
      maxIterations: parsedMaxIterations,
    }
    setSaving(true)
    setSaveError(null)
    setSaveSucceeded(false)
    try {
      const payload = buildSettingsPayload(settings, {}, {}, new Set(), {})
      payload.thinking = { ...requested.thinking }
      payload.max_iterations = requested.maxIterations
      const response = await saveSettings(payload)
      const reconciled = reconcileAgentThinkingSaveResponse(
        response,
        requested,
        confirmedValues,
      )
      // Keep the returned revision even when an older/incompatible backend fails to echo the field.
      // The separate coherent baseline remains unchanged, so every user's draft stays retryable.
      setSettings(reconciled.settings)
      setConfirmedValues(reconciled.confirmedValues)
      if (!reconciled.confirmed) {
        setSaveError('设置已返回，但后端未精确确认 Think、思考深度和最大思考轮次，请重试或升级后端。')
        return
      }
      setSaveSucceeded(true)
    } catch (error) {
      setSaveError(`保存失败：${errorMessage(error)}`)
      // Another independently loaded General card may have committed a newer full-form revision.
      // Refresh that revision without replacing this card's draft, so the next click can retry it.
      try {
        const latest = sanitizeSettings(await getSettings())
        setSettings(latest)
        setConfirmedValues(agentThinkingValues(latest))
      } catch {
        // Preserve the original save error and loaded snapshot when refresh is also unavailable.
      }
    } finally {
      setSaving(false)
    }
  }

  if (!settings) {
    return (
      <div className="card p-3" data-agent-thinking-settings="true">
        <div className="text-[13px] font-medium text-ink">Agent 思考</div>
        {loadError ? (
          <div className="mt-2 space-y-2 text-[11px]">
            <p role="alert" className="text-danger">设置加载失败：{loadError}</p>
            <button type="button" className="btn-outline" onClick={() => void load()}>重试</button>
          </div>
        ) : (
          <p role="status" className="mt-2 text-[11px] text-ink3">正在加载…</p>
        )}
      </div>
    )
  }

  return (
    <AgentThinkingEditor
      thinkingEnabled={thinkingEnabledDraft}
      effort={effortDraft}
      maxIterationsDraft={maxIterationsDraft}
      saving={saving}
      canSave={canSave}
      validationError={validationError}
      saveError={saveError}
      saveSucceeded={saveSucceeded}
      onThinkingEnabledChange={(value) => {
        setThinkingEnabledDraft(value)
        setSaveError(null)
        setSaveSucceeded(false)
      }}
      onEffortChange={(value) => {
        setEffortDraft(value)
        setSaveError(null)
        setSaveSucceeded(false)
      }}
      onMaxIterationsDraftChange={(value) => {
        setMaxIterationsDraft(value)
        setSaveError(null)
        setSaveSucceeded(false)
      }}
      onSave={() => void save()}
    />
  )
}

// ---------- 学术数据源 API keys ----------
function AcademicKeysCard() {
  const [settings, setSettings] = useState<AppSettings | null>(null)
  const [openalexDraft, setOpenalexDraft] = useState('')
  const [loadError, setLoadError] = useState<string | null>(null)
  const [saving, setSaving] = useState(false)
  const [saveError, setSaveError] = useState<string | null>(null)
  const [saveSucceeded, setSaveSucceeded] = useState(false)

  const load = async () => {
    setLoadError(null)
    setSaveError(null)
    setSaveSucceeded(false)
    try {
      const s = sanitizeSettings(await getSettings())
      setSettings(s)
      // 草稿初始化为空（后端返回的是 mask 占位，不回填到输入框）。
      setOpenalexDraft('')
    } catch (error) {
      setLoadError(errorMessage(error))
    }
  }

  useEffect(() => {
    void load()
  }, [])

  useEffect(() => {
    if (!saveSucceeded) return
    const timer = window.setTimeout(() => setSaveSucceeded(false), 4000)
    return () => window.clearTimeout(timer)
  }, [saveSucceeded])

  const configured = isDataSourceKeyConfigured(settings, 'OPENALEX_API_KEY')

  const save = async () => {
    if (!canSubmitDataSourceKey(settings, openalexDraft, saving)) return

    setSaving(true)
    setSaveError(null)
    setSaveSucceeded(false)
    try {
      const payload = buildSettingsPayload(
        settings,
        {},
        {},
        new Set(),
        { OPENALEX_API_KEY: openalexDraft },
      )
      const saved = sanitizeSettings(await saveSettings(payload))
      setSettings(saved)
      if (isDataSourceKeyConfigured(saved, 'OPENALEX_API_KEY')) {
        setOpenalexDraft('')
        setSaveSucceeded(true)
      } else {
        setSaveError('设置已返回，但未确认 OpenAlex API Key 已配置，请重试。')
      }
    } catch (error) {
      setSaveError(errorMessage(error))
    } finally {
      setSaving(false)
    }
  }

  if (!settings) {
    return (
      <div className="card p-3">
        <div className="text-[13px] font-medium text-ink">学术数据源</div>
        {loadError ? (
          <div className="mt-2 space-y-2 text-[11px]">
            <p role="alert" className="text-danger">设置加载失败：{loadError}</p>
            <button className="btn-outline" onClick={() => void load()}>重试</button>
          </div>
        ) : (
          <p role="status" className="mt-2 text-[11px] text-ink3">正在加载 OpenAlex 配置…</p>
        )}
      </div>
    )
  }

  return (
    <div className="card p-3">
      <div className="text-[13px] font-medium text-ink">学术数据源</div>
      <label className="mt-2 block">
        <span className="text-[12px] text-ink2">OpenAlex API Key</span>
        <input
          type="password"
          className="input mt-1 py-1.5 font-mono"
          value={openalexDraft}
          autoComplete="off"
          disabled={saving}
          onChange={(e) => {
            setOpenalexDraft(e.target.value)
            setSaveError(null)
            setSaveSucceeded(false)
          }}
          placeholder={configured ? '已配置，输入新值覆盖' : '输入 OpenAlex API key'}
        />
      </label>
      <p className="mt-1 text-[11px] text-ink3">
        OpenAlex API key 可免费获取；未配置时只有少量匿名试用额度，不适合稳定使用。前往{' '}
        <a
          href="https://openalex.org/settings/api"
          target="_blank"
          rel="noreferrer"
          className="text-brand underline"
        >
          OpenAlex API settings
        </a>{' '}
        获取。用于 search_papers / fetch_paper 工具。
      </p>
      <div className="mt-2 flex items-center gap-2">
        <button
          className="btn-primary"
          disabled={!canSubmitDataSourceKey(settings, openalexDraft, saving)}
          onClick={() => void save()}
        >
          {saving ? '保存中…' : '保存'}
        </button>
        {!openalexDraft.trim() && (
          <span className={`text-[11px] ${configured ? 'text-success' : 'text-ink3'}`}>
            {configured ? '已配置' : '未配置'}
          </span>
        )}
        {saveSucceeded && (
          <span className="text-[11px] text-success">已保存</span>
        )}
        {saveError && <span role="alert" className="text-[11px] text-danger">{saveError}</span>}
      </div>
    </div>
  )
}

// ---------- 日志保留策略（D-T07） ----------
function LogRetentionCard() {
  const [settings, setSettings] = useState<AppSettings | null>(null)
  const [days, setDays] = useState('14')
  const [maxRows, setMaxRows] = useState('100000')
  const [loadError, setLoadError] = useState<string | null>(null)
  const [saving, setSaving] = useState(false)
  const [saveError, setSaveError] = useState<string | null>(null)
  const [saveSucceeded, setSaveSucceeded] = useState(false)

  const load = async () => {
    setLoadError(null)
    setSaveError(null)
    setSaveSucceeded(false)
    try {
      const s = sanitizeSettings(await getSettings())
      setSettings(s)
      setDays(String(s.log_retention_days ?? 14))
      setMaxRows(String(s.log_max_rows ?? 100_000))
    } catch (error) {
      setLoadError(errorMessage(error))
    }
  }

  useEffect(() => {
    void load()
  }, [])

  useEffect(() => {
    if (!saveSucceeded) return
    const timer = window.setTimeout(() => setSaveSucceeded(false), 4000)
    return () => window.clearTimeout(timer)
  }, [saveSucceeded])

  const daysNum = Number.parseInt(days, 10)
  const maxRowsNum = Number.parseInt(maxRows, 10)
  const daysValid = Number.isFinite(daysNum) && daysNum >= 1
  const maxRowsValid = Number.isFinite(maxRowsNum) && maxRowsNum >= 1000
  const dirty =
    !!settings &&
    (daysNum !== (settings.log_retention_days ?? 14) ||
      maxRowsNum !== (settings.log_max_rows ?? 100_000))
  const canSave = !!settings && !saving && daysValid && maxRowsValid && dirty

  const save = async () => {
    if (!settings || !canSave) return
    setSaving(true)
    setSaveError(null)
    setSaveSucceeded(false)
    try {
      const payload = buildSettingsPayload(settings, {}, {}, new Set(), {})
      payload.log_retention_days = daysNum
      payload.log_max_rows = maxRowsNum
      const saved = sanitizeSettings(await saveSettings(payload))
      setSettings(saved)
      setDays(String(saved.log_retention_days ?? daysNum))
      setMaxRows(String(saved.log_max_rows ?? maxRowsNum))
      setSaveSucceeded(true)
    } catch (error) {
      setSaveError(errorMessage(error))
    } finally {
      setSaving(false)
    }
  }

  if (!settings) {
    return (
      <div className="card p-3">
        <div className="text-[13px] font-medium text-ink">日志保留</div>
        {loadError ? (
          <div className="mt-2 space-y-2 text-[11px]">
            <p role="alert" className="text-danger">设置加载失败：{loadError}</p>
            <button className="btn-outline" onClick={() => void load()}>重试</button>
          </div>
        ) : (
          <p role="status" className="mt-2 text-[11px] text-ink3">正在加载…</p>
        )}
      </div>
    )
  }

  return (
    <div className="card p-3">
      <div className="text-[13px] font-medium text-ink">日志保留</div>
      <div className="mt-2 grid grid-cols-2 gap-3">
        <label className="block">
          <span className="text-[12px] text-ink2">保留天数</span>
          <input
            type="number"
            min={1}
            className="input mt-1 py-1.5"
            value={days}
            disabled={saving}
            onChange={(e) => {
              setDays(e.target.value)
              setSaveError(null)
              setSaveSucceeded(false)
            }}
          />
          {!daysValid && <span className="mt-1 block text-[11px] text-danger">至少 1 天</span>}
        </label>
        <label className="block">
          <span className="text-[12px] text-ink2">最大条数</span>
          <input
            type="number"
            min={1000}
            className="input mt-1 py-1.5"
            value={maxRows}
            disabled={saving}
            onChange={(e) => {
              setMaxRows(e.target.value)
              setSaveError(null)
              setSaveSucceeded(false)
            }}
          />
          {!maxRowsValid && (
            <span className="mt-1 block text-[11px] text-danger">至少 1000 条</span>
          )}
        </label>
      </div>
      <p className="mt-1 text-[11px] text-ink3">
        超过天数或条数上限的日志会被自动清理（先到先清）。后台启动时跑一次，之后每 6
        小时循环。修改后重启后端生效。
      </p>
      <div className="mt-2 flex items-center gap-2">
        <button className="btn-primary" disabled={!canSave} onClick={() => void save()}>
          {saving ? '保存中…' : '保存'}
        </button>
        {saveSucceeded && <span className="text-[11px] text-success">已保存</span>}
        {saveError && <span role="alert" className="text-[11px] text-danger">{saveError}</span>}
      </div>
    </div>
  )
}
