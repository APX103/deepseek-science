import { describe, expect, test } from 'bun:test'
import {
  backendReflectsSettings,
  buildSettingsPayload,
  normalizeMcpServers,
  parseMcpJsonConfig,
  sanitizeSettings,
  settingsSaveNotice,
} from '../src/api/settingsState'
import type { AppSettings, BackendStatus } from '../src/types'

function settings(overrides: Partial<AppSettings> = {}): AppSettings {
  return {
    providers: [
      {
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
