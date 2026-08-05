import type {
  A2aAgentSettings,
  AppSettings,
  AppSettingsUpdate,
  BackendStatus,
  LlmOverriddenField,
} from '../types'

export type SettingsKeyDrafts = Readonly<Record<string, string>>
export type A2aTokenDrafts = Readonly<Record<string, string>>
export type A2aTokenClears = ReadonlySet<string>

/** Never retain a plaintext key received from the server in frontend state. */
export function sanitizeSettings(settings: AppSettings): AppSettings {
  return {
    ...settings,
    providers: settings.providers.map((provider) => {
      const { api_key: _sensitiveKey, ...publicProvider } = provider
      return publicProvider
    }),
    a2a_agents: (settings.a2a_agents ?? []).map((agent) => {
      const {
        bearer_token: _sensitiveToken,
        clear_bearer_token: _serverControlledClear,
        ...publicAgent
      } = agent
      return publicAgent
    }),
  }
}

/** Build the one outbound payload where a freshly typed key is allowed to exist. */
export function buildSettingsPayload(
  settings: AppSettings,
  keyDrafts: SettingsKeyDrafts,
  a2aTokenDrafts: A2aTokenDrafts = {},
  a2aTokenClears: A2aTokenClears = new Set(),
): AppSettingsUpdate {
  const sanitized = sanitizeSettings(settings)
  return {
    model: sanitized.model,
    default_workspace: sanitized.default_workspace,
    revision: sanitized.revision,
    providers: sanitized.providers.map((provider) => {
      const draft = keyDrafts[provider.name]?.trim()
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
