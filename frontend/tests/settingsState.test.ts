import { describe, expect, test } from 'bun:test'
import {
  backendReflectsSettings,
  buildSettingsPayload,
  canSubmitDataSourceKey,
  DATA_SOURCE_KEY_MASK,
  DEFAULT_MAX_ITERATIONS,
  DEFAULT_THINKING_EFFORT,
  DEFAULT_THINKING_ENABLED,
  isDataSourceKeyConfigured,
  MAX_CONFIGURABLE_ITERATIONS,
  MIN_MAX_ITERATIONS,
  normalizeMcpServers,
  normalizeMaxIterations,
  normalizeThinkingSettings,
  parseMcpJsonConfig,
  parseMaxIterationsDraft,
  reconcileAgentThinkingSaveResponse,
  sanitizeSettings,
  settingsResponseConfirmsAgentThinking,
  settingsSaveNotice,
  type AgentThinkingValues,
} from '../src/api/settingsState'
import type { AppSettings, BackendStatus } from '../src/types'

function settings(overrides: Partial<AppSettings> = {}): AppSettings {
  return {
    providers: [
      {
        id: 'DeepSeek',
        name: 'DeepSeek',
        base_url: 'https://api.example.invalid',
        api_key_masked: '••••••••',
        api_key: 'server-response-must-be-discarded',
        enabled: true,
        model: 'deepseek-research',
      },
    ],
    model: 'deepseek-research',
    default_workspace: '/tmp/workspace',
    restart_required: false,
    revision: 7,
    overridden_fields: [],
    ...overrides,
  }
}

function matchingBackend(overrides: Partial<BackendStatus> = {}): BackendStatus {
  return {
    online: true,
    llmConfigured: true,
    model: 'deepseek-research',
    baseUrl: 'https://api.example.invalid',
    revision: 7,
    ...overrides,
  }
}

describe('settings state safety', () => {
  test('removes a server-provided plaintext credential without mutating the input', () => {
    const received = settings()
    const sanitized = sanitizeSettings(received)

    expect('api_key' in sanitized.providers[0]).toBe(false)
    expect(received.providers[0].api_key).toBe('server-response-must-be-discarded')
    expect(JSON.stringify(sanitized)).not.toContain('server-response-must-be-discarded')
  })

  test('includes only a fresh non-empty draft in the outbound payload', () => {
    const draftValue = 'typed-only-for-this-unit-test'
    const payload = buildSettingsPayload(settings(), { DeepSeek: `  ${draftValue}  ` })
    const preserveExisting = buildSettingsPayload(settings(), { DeepSeek: '   ' })

    expect(payload.providers[0].api_key).toBe(draftValue)
    expect('api_key' in preserveExisting.providers[0]).toBe(false)
    expect(payload.revision).toBe(settings().revision)
    expect('overridden_fields' in payload).toBe(false)
    expect('restart_required' in payload).toBe(false)
  })

  test('drops unexpected plaintext data-source keys and accepts only the fixed mask', () => {
    const received = {
      ...settings(),
      api_keys: { OPENALEX_API_KEY: 'server-response-must-be-discarded' },
      api_keys_masked: {
        OPENALEX_API_KEY: DATA_SOURCE_KEY_MASK,
        UNSAFE_SOURCE: 'server-response-must-be-discarded',
      },
    } as AppSettings
    const sanitized = sanitizeSettings(received)

    expect('api_keys' in sanitized).toBe(false)
    expect(sanitized.api_keys_masked).toEqual({
      OPENALEX_API_KEY: DATA_SOURCE_KEY_MASK,
      UNSAFE_SOURCE: '',
    })
    expect(isDataSourceKeyConfigured(sanitized, 'OPENALEX_API_KEY')).toBe(true)
    expect(isDataSourceKeyConfigured(sanitized, 'UNSAFE_SOURCE')).toBe(false)
    expect(JSON.stringify(sanitized)).not.toContain('server-response-must-be-discarded')
  })

  test('data-source payload omits blank drafts and submits only trimmed new values', () => {
    const current = settings({ api_keys_masked: { OPENALEX_API_KEY: DATA_SOURCE_KEY_MASK } })
    const preserve = buildSettingsPayload(current, {}, {}, new Set(), {
      OPENALEX_API_KEY: '   ',
    })
    const replace = buildSettingsPayload(current, {}, {}, new Set(), {
      OPENALEX_API_KEY: '  typed-openalex-key  ',
      EMPTY_SOURCE: '\n',
    })

    expect('api_keys' in preserve).toBe(false)
    expect(replace.api_keys).toEqual({ OPENALEX_API_KEY: 'typed-openalex-key' })
    expect(replace.providers).toEqual(buildSettingsPayload(current, {}).providers)
    expect(replace.revision).toBe(current.revision)
  })

  test('data-source save stays disabled until settings load and the draft is non-empty', () => {
    expect(canSubmitDataSourceKey(null, 'typed-openalex-key', false)).toBe(false)
    expect(canSubmitDataSourceKey(settings(), '   ', false)).toBe(false)
    expect(canSubmitDataSourceKey(settings(), 'typed-openalex-key', true)).toBe(false)
    expect(canSubmitDataSourceKey(settings(), ' typed-openalex-key ', false)).toBe(true)
  })

  test('requires an exact runtime revision, model, base URL, and configured state', () => {
    const saved = settings()
    expect(backendReflectsSettings(saved, matchingBackend())).toBe(true)
    expect(backendReflectsSettings(saved, matchingBackend({ revision: 6 }))).toBe(false)
    expect(backendReflectsSettings(saved, matchingBackend({ model: 'stale-model' }))).toBe(false)
    expect(
      backendReflectsSettings(saved, matchingBackend({ baseUrl: 'https://stale.example.invalid' })),
    ).toBe(false)
    expect(backendReflectsSettings(saved, matchingBackend({ llmConfigured: false }))).toBe(false)
    expect(backendReflectsSettings(settings({ restart_required: true }), matchingBackend())).toBe(false)
    expect(backendReflectsSettings(settings({ revision: Number.NaN }), matchingBackend())).toBe(false)
  })

  test('accepts an exact unconfigured snapshot but warns that the LLM is unavailable', () => {
    const unconfigured = settings({
      providers: settings().providers.map((provider) => ({ ...provider, api_key_masked: '' })),
    })
    const backend = matchingBackend({ llmConfigured: false })

    expect(backendReflectsSettings(unconfigured, backend)).toBe(true)
    const notice = settingsSaveNotice(unconfigured, backend)
    expect(notice.kind).toBe('warning')
    expect(notice.message).toContain('LLM 当前不可用')
    expect(notice.message).not.toContain('立即生效')
    expect(notice.message).not.toContain('将使用模型')
  })

  test('warns about environment-owned fields without exposing their values', () => {
    const saved = settings({ overridden_fields: ['api_key', 'base_url', 'model'] })
    const notice = settingsSaveNotice(saved, matchingBackend())

    expect(notice.kind).toBe('warning')
    expect(notice.message).toContain('API Key、Base URL、模型当前由环境变量接管')
    expect(notice.message).not.toContain('https://api.example.invalid')
    expect(notice.message).not.toContain('deepseek-research')
    expect(notice.message).not.toContain('立即生效')
  })

  test('verifies env-owned runtime fields without copying their values into editable settings', () => {
    const persistedFallback = settings({
      providers: settings().providers.map((provider) => ({
        ...provider,
        base_url: 'https://persisted.example.invalid',
        model: 'persisted-model',
        api_key_masked: '',
      })),
      model: 'persisted-model',
      overridden_fields: ['api_key', 'base_url', 'model'],
    })
    const effectiveRuntime = matchingBackend({
      baseUrl: 'https://environment.example.invalid',
      model: 'environment-model',
      llmConfigured: true,
    })

    expect(backendReflectsSettings(persistedFallback, effectiveRuntime)).toBe(true)
    expect(buildSettingsPayload(persistedFallback, {}).providers[0].base_url).toBe(
      'https://persisted.example.invalid',
    )
  })

  test('claims immediate activation only for a configured snapshot without overrides', () => {
    expect(settingsSaveNotice(settings(), matchingBackend())).toEqual({
      kind: 'success',
      message: '已保存并立即生效。后续新请求将使用模型 deepseek-research。',
    })
  })
})

describe('agent thinking settings', () => {
  test('normalizes a legacy response and carries complete defaults through every full-form save', () => {
    const legacy = settings()
    const sanitized = sanitizeSettings(legacy)

    expect(legacy.max_iterations).toBeUndefined()
    expect(legacy.thinking).toBeUndefined()
    expect(sanitized.max_iterations).toBe(DEFAULT_MAX_ITERATIONS)
    expect(sanitized.thinking).toEqual({
      enabled: DEFAULT_THINKING_ENABLED,
      effort: DEFAULT_THINKING_EFFORT,
    })
    expect(buildSettingsPayload(legacy, {}).max_iterations).toBe(DEFAULT_MAX_ITERATIONS)
    expect(buildSettingsPayload(sanitized, {}).max_iterations).toBe(DEFAULT_MAX_ITERATIONS)
    expect(buildSettingsPayload(legacy, {}).thinking).toEqual({ enabled: true, effort: 'high' })
    expect(buildSettingsPayload(sanitized, {}).thinking).toEqual({ enabled: true, effort: 'high' })
  })

  test('strictly normalizes partial or malformed thinking members', () => {
    expect(normalizeThinkingSettings(undefined)).toEqual({ enabled: true, effort: 'high' })
    expect(normalizeThinkingSettings({ enabled: false, effort: 'max' })).toEqual({
      enabled: false,
      effort: 'max',
    })
    expect(normalizeThinkingSettings({ enabled: false, effort: 'medium' })).toEqual({
      enabled: false,
      effort: 'high',
    })
    expect(normalizeThinkingSettings({ enabled: 'false', effort: 'low' })).toEqual({
      enabled: true,
      effort: 'low',
    })
    for (const malformed of [null, [], 'high', 1, { enabled: true, effort: 'xhigh' }]) {
      expect(normalizeThinkingSettings(malformed)).toEqual({ enabled: true, effort: 'high' })
    }
  })

  test('preserves explicit values, including a disabled switch with its retained effort', () => {
    const configured = settings({
      max_iterations: 640,
      thinking: { enabled: false, effort: 'max' },
    })
    const payload = buildSettingsPayload(configured, {})

    expect(normalizeMaxIterations(configured.max_iterations)).toBe(640)
    expect(sanitizeSettings(configured).max_iterations).toBe(640)
    expect(payload.max_iterations).toBe(640)
    expect(payload.thinking).toEqual({ enabled: false, effort: 'max' })
    expect(JSON.stringify(payload)).not.toContain('server-response-must-be-discarded')
  })

  test('accepts only plain decimal safe integers in the shared inclusive range', () => {
    expect(parseMaxIterationsDraft(String(MIN_MAX_ITERATIONS))).toBe(MIN_MAX_ITERATIONS)
    expect(parseMaxIterationsDraft(String(DEFAULT_MAX_ITERATIONS))).toBe(DEFAULT_MAX_ITERATIONS)
    expect(parseMaxIterationsDraft(String(MAX_CONFIGURABLE_ITERATIONS))).toBe(
      MAX_CONFIGURABLE_ITERATIONS,
    )

    for (const invalid of ['', ' ', '0', '-1', '1.5', '1e2', '1001', 'NaN', '+1']) {
      expect(parseMaxIterationsDraft(invalid)).toBeNull()
    }
  })

  test('fails safe to the default for malformed values received from a backend', () => {
    for (const invalid of [0, -1, 1.5, 1001, Number.NaN, Number.POSITIVE_INFINITY]) {
      expect(normalizeMaxIterations(invalid)).toBe(DEFAULT_MAX_ITERATIONS)
    }
  })

  test('requires the raw POST response to explicitly echo all three requested values', () => {
    const requested: AgentThinkingValues = {
      thinking: { enabled: false, effort: 'max' },
      maxIterations: 240,
    }

    expect(settingsResponseConfirmsAgentThinking(settings({ max_iterations: 240 }), requested)).toBe(false)
    expect(settingsResponseConfirmsAgentThinking(settings({
      thinking: { enabled: true, effort: 'max' },
      max_iterations: 240,
    }), requested)).toBe(false)
    expect(settingsResponseConfirmsAgentThinking(settings({
      thinking: { enabled: false, effort: 'high' },
      max_iterations: 240,
    }), requested)).toBe(false)
    expect(settingsResponseConfirmsAgentThinking(settings({
      thinking: { enabled: false, effort: 'max' },
      max_iterations: 241,
    }), requested)).toBe(false)
    expect(settingsResponseConfirmsAgentThinking(settings({
      thinking: { enabled: false, effort: 'max' },
      max_iterations: 240,
    }), requested)).toBe(true)
  })

  test('keeps a default-colliding mismatched save retryable while adopting its newer revision', () => {
    const requested: AgentThinkingValues = {
      thinking: { enabled: true, effort: 'high' },
      maxIterations: DEFAULT_MAX_ITERATIONS,
    }
    const previousConfirmed: AgentThinkingValues = {
      thinking: { enabled: true, effort: 'high' },
      maxIterations: 160,
    }
    const reconciled = reconcileAgentThinkingSaveResponse(
      settings({ revision: 8 }),
      requested,
      previousConfirmed,
    )

    expect(reconciled.confirmed).toBe(false)
    expect(reconciled.settings.revision).toBe(8)
    // Sanitization fills legacy defaults, but the independent confirmed baseline must not move.
    expect(reconciled.settings.thinking).toEqual(requested.thinking)
    expect(reconciled.settings.max_iterations).toBe(requested.maxIterations)
    expect(reconciled.confirmedValues).toEqual(previousConfirmed)
    expect(requested.maxIterations).not.toBe(reconciled.confirmedValues.maxIterations)
  })

  test('one mismatched thinking member preserves the whole coherent confirmed baseline', () => {
    const requested: AgentThinkingValues = {
      thinking: { enabled: false, effort: 'max' },
      maxIterations: 240,
    }
    const previousConfirmed: AgentThinkingValues = {
      thinking: { enabled: true, effort: 'low' },
      maxIterations: 160,
    }
    const reconciled = reconcileAgentThinkingSaveResponse(
      settings({ revision: 8, thinking: { enabled: false, effort: 'high' }, max_iterations: 240 }),
      requested,
      previousConfirmed,
    )

    expect(reconciled.confirmed).toBe(false)
    expect(reconciled.confirmedValues).toEqual(previousConfirmed)
  })

  test('advances the whole confirmed baseline only after an exact response echo', () => {
    const requested: AgentThinkingValues = {
      thinking: { enabled: false, effort: 'max' },
      maxIterations: 240,
    }
    const reconciled = reconcileAgentThinkingSaveResponse(
      settings({ revision: 8, thinking: requested.thinking, max_iterations: requested.maxIterations }),
      requested,
      {
        thinking: { enabled: true, effort: 'high' },
        maxIterations: DEFAULT_MAX_ITERATIONS,
      },
    )

    expect(reconciled.confirmed).toBe(true)
    expect(reconciled.settings.revision).toBe(8)
    expect(reconciled.confirmedValues).toEqual(requested)
  })
})

describe('mcp server settings', () => {
  test('normalize fills defaults for a missing or partial list', () => {
    expect(normalizeMcpServers(undefined)).toEqual([])
    expect(normalizeMcpServers(null)).toEqual([])
    expect(
      normalizeMcpServers([{ name: 'a', url: 'http://x', enabled: true } as never]),
    ).toEqual([{ name: 'a', url: 'http://x', enabled: true, connected: false, tool_count: null }])
  })

  test('payload carries mcp servers without diagnostic fields', () => {
    const withMcp = settings({
      mcp_servers: [
        { name: 'lit', url: 'http://127.0.0.1:8901/sse', enabled: true, connected: true, tool_count: 4 },
      ],
    })
    const payload = buildSettingsPayload(withMcp, {})
    expect(payload.mcp_servers).toEqual([{ name: 'lit', url: 'http://127.0.0.1:8901/sse', enabled: true }])
    expect(JSON.stringify(payload)).not.toContain('tool_count')
  })

  test('sanitize normalizes a missing mcp list so saves never drop it', () => {
    const sanitized = sanitizeSettings(settings())
    expect(sanitized.mcp_servers).toEqual([])
    expect(buildSettingsPayload(sanitized, {}).mcp_servers).toEqual([])
  })

  test('parses a single server object', () => {
    expect(parseMcpJsonConfig('{"name":"lit","url":"http://127.0.0.1:8901/sse"}')).toEqual([
      { name: 'lit', url: 'http://127.0.0.1:8901/sse', enabled: true },
    ])
  })

  test('parses the Claude-style mcpServers map', () => {
    const parsed = parseMcpJsonConfig(
      '{"mcpServers":{"lit":{"url":"http://a/sse"},"off":{"url":"https://b/mcp","enabled":false}}}',
    )
    expect(parsed).toEqual([
      { name: 'lit', url: 'http://a/sse', enabled: true },
      { name: 'off', url: 'https://b/mcp', enabled: false },
    ])
  })

  test('parses an array and a bare name-keyed map', () => {
    expect(parseMcpJsonConfig('[{"name":"a","url":"http://a"}]')).toEqual([
      { name: 'a', url: 'http://a', enabled: true },
    ])
    expect(parseMcpJsonConfig('{"b":{"url":"http://b"}}')).toEqual([
      { name: 'b', url: 'http://b', enabled: true },
    ])
  })

  test('rejects stdio servers, bad urls, duplicates, and broken json', () => {
    expect(() => parseMcpJsonConfig('{"mcpServers":{"x":{"command":"npx","args":[]}}}')).toThrow(
      /command\/stdio/,
    )
    expect(() => parseMcpJsonConfig('{"name":"x","url":"stdio://nope"}')).toThrow(/http/)
    expect(() =>
      parseMcpJsonConfig('[{"name":"x","url":"http://a"},{"name":"X","url":"http://b"}]'),
    ).toThrow(/重复/)
    expect(() => parseMcpJsonConfig('{not json')).toThrow(/JSON 解析失败/)
  })
})
