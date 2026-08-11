import type {
  A2aAgentSettings,
  AppSettings,
  AppSettingsProvider,
  AppSettingsUpdate,
  BackendStatus,
  LlmOverriddenField,
  McpServer,
  McpServerUpdate,
  MaskedApiKey,
  SkillSettingsValue,
} from '../types'

export type SettingsKeyDrafts = Readonly<Record<string, string>>
export type DataSourceKeyDrafts = Readonly<Record<string, string>>
export const DATA_SOURCE_KEY_MASK: MaskedApiKey = '••••••••'

/** Normalize skill settings so the shape is always complete regardless of backend version. */
export function normalizeSkillSettings(value?: SkillSettingsValue | null): SkillSettingsValue {
  return {
    disabled: value?.disabled ? [...value.disabled] : [],
    include_claude: value?.include_claude ?? false,
    include_codex: value?.include_codex ?? false,
    include_cursor: value?.include_cursor ?? false,
    custom_dirs: value?.custom_dirs ? [...value.custom_dirs] : [],
  }
}
export type A2aTokenDrafts = Readonly<Record<string, string>>
export type A2aTokenClears = ReadonlySet<string>

/**
 * Parse a JSON MCP config into editable server entries. Accepts three shapes:
 * a single server object (`{"name": …, "url": …}`), an array of those, or the Claude-style
 * map (`{"mcpServers": {"<name>": {"url": …}}}`). Only http(s) servers are supported;
 * stdio/command entries are rejected with a readable error.
 */
export function parseMcpJsonConfig(text: string): McpServerUpdate[] {
  let raw: unknown
  try {
    raw = JSON.parse(text)
  } catch (error) {
    throw new Error(`JSON 解析失败：${error instanceof Error ? error.message : String(error)}`)
  }

  const toEntry = (name: unknown, value: unknown): McpServerUpdate => {
    if (typeof name !== 'string' || name.trim() === '') {
      throw new Error('每个 server 都需要非空的 name')
    }
    if (typeof value !== 'object' || value === null || Array.isArray(value)) {
      throw new Error(`server "${name}" 的配置必须是对象`)
    }
    const record = value as Record<string, unknown>
    if (typeof record.command === 'string' && record.command.trim() !== '') {
      throw new Error(`server "${name}" 使用 command/stdio 方式启动，当前仅支持 http(s) URL 接入`)
    }
    const url = typeof record.url === 'string' ? record.url.trim() : ''
    if (!url.startsWith('http://') && !url.startsWith('https://')) {
      throw new Error(`server "${name}" 的 url 必须以 http:// 或 https:// 开头`)
    }
    return {
      name: name.trim(),
      url,
      enabled: typeof record.enabled === 'boolean' ? record.enabled : true,
    }
  }

  const entries: McpServerUpdate[] = []
  if (Array.isArray(raw)) {
    for (const item of raw) {
      if (typeof item !== 'object' || item === null) throw new Error('数组中的每项必须是对象')
      entries.push(toEntry((item as Record<string, unknown>).name, item))
    }
  } else if (typeof raw === 'object' && raw !== null) {
    const record = raw as Record<string, unknown>
    const map = record.mcpServers ?? record.mcp_servers
    if (map !== undefined) {
      if (typeof map !== 'object' || map === null || Array.isArray(map)) {
        throw new Error('mcpServers 必须是 { 名称: 配置 } 形式的对象')
      }
      for (const [name, value] of Object.entries(map)) entries.push(toEntry(name, value))
    } else if ('url' in record || 'name' in record) {
      entries.push(toEntry(record.name, record))
    } else {
      // 裸 map 形式：{"<name>": {"url": …}}
      for (const [name, value] of Object.entries(record)) entries.push(toEntry(name, value))
    }
  } else {
    throw new Error('JSON 配置必须是对象或数组')
  }

  const seen = new Set<string>()
  for (const entry of entries) {
    const key = entry.name.toLowerCase()
    if (seen.has(key)) throw new Error(`JSON 中存在重复的 server 名称：${entry.name}`)
    seen.add(key)
  }
  return entries
}

/** Normalize the MCP server list so the shape is always complete regardless of backend version. */
export function normalizeMcpServers(value?: McpServer[] | null): McpServer[] {
  return (value ?? []).map((server) => ({
    name: server.name ?? '',
    url: server.url ?? '',
    enabled: server.enabled ?? true,
    connected: server.connected ?? false,
    tool_count: server.tool_count ?? null,
  }))
}

/** Never retain a plaintext key received from the server in frontend state. */
export function sanitizeSettings(settings: AppSettings): AppSettings {
  // `api_keys` is outbound-only. Destructure it defensively in case a broken or
  // older backend includes plaintext data-source credentials in a response.
  const {
    api_keys: _sensitiveDataSourceKeys,
    ...publicSettings
  } = settings as AppSettings & { api_keys?: unknown }
  const apiKeysMasked = publicSettings.api_keys_masked
    ? Object.fromEntries(
        Object.entries(publicSettings.api_keys_masked).map(([name, value]) => [
          name,
          value === DATA_SOURCE_KEY_MASK ? DATA_SOURCE_KEY_MASK : '',
        ]),
      ) as Record<string, MaskedApiKey>
    : undefined

  return {
    ...publicSettings,
    providers: publicSettings.providers.map((provider) => {
      const { api_key: _sensitiveKey, ...publicProvider } = provider
      return publicProvider
    }),
    a2a_agents: (publicSettings.a2a_agents ?? []).map((agent) => {
      const {
        bearer_token: _sensitiveToken,
        clear_bearer_token: _serverControlledClear,
        ...publicAgent
      } = agent
      return publicAgent
    }),
    skills: normalizeSkillSettings(publicSettings.skills),
    mcp_servers: normalizeMcpServers(publicSettings.mcp_servers),
    api_keys_masked: apiKeysMasked,
  }
}

/** Only the backend's exact public mask may establish configured state. */
export function isDataSourceKeyConfigured(
  settings: AppSettings | null,
  name: string,
): boolean {
  return settings?.api_keys_masked?.[name] === DATA_SOURCE_KEY_MASK
}

/** Loading, saving, and blank-draft states must never submit a settings form. */
export function canSubmitDataSourceKey(
  settings: AppSettings | null,
  draft: string,
  saving: boolean,
): settings is AppSettings {
  return settings !== null && !saving && draft.trim().length > 0
}

/** Build the one outbound payload where a freshly typed key is allowed to exist. */
export function buildSettingsPayload(
  settings: AppSettings,
  keyDrafts: SettingsKeyDrafts,
  a2aTokenDrafts: A2aTokenDrafts = {},
  a2aTokenClears: A2aTokenClears = new Set(),
  dataSourceKeyDrafts: DataSourceKeyDrafts = {},
): AppSettingsUpdate {
  const sanitized = sanitizeSettings(settings)
  const apiKeys = Object.fromEntries(
    Object.entries(dataSourceKeyDrafts)
      .map(([name, value]) => [name, value.trim()] as const)
      .filter(([, value]) => value.length > 0),
  )
  return {
    model: sanitized.model,
    default_workspace: sanitized.default_workspace,
    revision: sanitized.revision,
    providers: sanitized.providers.map((provider) => {
      const draft = keyDrafts[provider.id]?.trim()
      return draft ? { ...provider, api_key: draft } : provider
    }),
    a2a_agents: (sanitized.a2a_agents ?? []).map((agent) => {
      const draft = a2aTokenDrafts[agent.id]?.trim()
      const editable = {
        id: agent.id,
        name: agent.name,
        endpoint: agent.endpoint,
        enabled: agent.enabled,
        timeout_seconds: agent.timeout_seconds,
      }
      if (a2aTokenClears.has(agent.id)) {
        return { ...editable, clear_bearer_token: true }
      }
      return draft ? { ...editable, bearer_token: draft } : editable
    }),
    skills: normalizeSkillSettings(sanitized.skills),
    // connected/tool_count are server-side diagnostics; only the editable triple goes back out.
    mcp_servers: normalizeMcpServers(sanitized.mcp_servers).map((server) => ({
      name: server.name,
      url: server.url,
      enabled: server.enabled,
    })),
    ...(Object.keys(apiKeys).length > 0 ? { api_keys: apiKeys } : {}),
    log_retention_days: sanitized.log_retention_days ?? 14,
    log_max_rows: sanitized.log_max_rows ?? 100_000,
  }
}

function generatedA2aId(): string {
  const randomId = typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function'
    ? crypto.randomUUID().replace(/-/g, '').slice(0, 16)
    : `${Date.now().toString(36)}${Math.random().toString(36).slice(2, 10)}`
  return `agent_${randomId.toLowerCase()}`
}

/** Create a local draft without pretending that an Agent Card was fetched. */
export function createA2aAgentDraft(id = generatedA2aId()): A2aAgentSettings {
  return {
    id,
    name: '',
    endpoint: '',
    enabled: true,
    timeout_seconds: 120,
    bearer_token_masked: '',
    status: 'unchecked',
    last_error: null,
    last_refreshed_at: null,
    tool_name: `a2a_agent_${id}`,
    card_summary: null,
  }
}

export function effectiveSettingsModel(settings: AppSettings): string | undefined {
  const providerModel = settings.providers.find((provider) => provider.enabled)?.model?.trim()
  return providerModel || settings.model.trim() || undefined
}

export function effectiveSettingsBaseUrl(settings: AppSettings): string | undefined {
  return settings.providers.find((provider) => provider.enabled)?.base_url.trim() || undefined
}

export function enabledProvider(settings: AppSettings): AppSettingsProvider | undefined {
  return settings.providers.find((provider) => provider.enabled)
}

/** The save is considered active only after the runtime config reflects the response. */
export function backendReflectsSettings(
  settings: AppSettings,
  backend: BackendStatus,
): boolean {
  if (settings.restart_required || !backend.online) return false
  if (!Number.isSafeInteger(settings.revision)) return false
  const provider = settings.providers.find((candidate) => candidate.enabled)
  if (!provider) return false
  const expectsConfiguredLlm = provider.api_key_masked.trim().length > 0
  const expectedModel = effectiveSettingsModel(settings)
  const expectedBaseUrl = effectiveSettingsBaseUrl(settings)
  const overridden = new Set(settings.overridden_fields ?? [])
  return Boolean(expectedModel && expectedBaseUrl)
    && backend.revision === settings.revision
    && (overridden.has('model') ? Boolean(backend.model) : backend.model === expectedModel)
    && (overridden.has('base_url') ? Boolean(backend.baseUrl) : backend.baseUrl === expectedBaseUrl)
    && (overridden.has('api_key') ? backend.llmConfigured : backend.llmConfigured === expectsConfiguredLlm)
}

export interface SettingsSaveNotice {
  kind: 'success' | 'warning'
  message: string
}

const OVERRIDE_LABELS: Record<LlmOverriddenField, string> = {
  api_key: 'API Key',
  base_url: 'Base URL',
  model: '模型',
}

/** Describe only the verified runtime state; never include an override value or plaintext key. */
export function settingsSaveNotice(
  saved: AppSettings,
  backend: BackendStatus,
): SettingsSaveNotice {
  const overridden = (saved.overridden_fields ?? [])
    .filter((field): field is LlmOverriddenField => field in OVERRIDE_LABELS)
    .map((field) => OVERRIDE_LABELS[field])
  const overrideMessage = overridden.length > 0
    ? `${overridden.join('、')}当前由环境变量接管，本次输入未切换这些运行中字段。`
    : ''

  if (!backend.llmConfigured) {
    return {
      kind: 'warning',
      message: `设置已保存，但尚未配置可用的 API Key，LLM 当前不可用。${overrideMessage}`,
    }
  }
  if (overrideMessage) {
    return {
      kind: 'warning',
      message: `设置已保存，但${overrideMessage}`,
    }
  }

  const model = effectiveSettingsModel(saved)
  return {
    kind: 'success',
    message: model
      ? `已保存并立即生效。后续新请求将使用模型 ${model}。`
      : '已保存并立即生效。后续新请求将使用新配置。',
  }
}
