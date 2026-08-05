import { describe, expect, test } from 'bun:test'
import {
  backendReflectsSettings,
  buildSettingsPayload,
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
