import { describe, expect, test } from 'bun:test'
import {
  buildSettingsPayload,
  createA2aAgentDraft,
  sanitizeSettings,
} from '../src/api/settingsState'
import type { AppSettings } from '../src/types'

function settings(): AppSettings {
  return {
    providers: [
      {
        name: 'DeepSeek',
        base_url: 'https://api.example.invalid',
        api_key_masked: '••••••••',
        api_key: 'server-must-not-return-this-key',
        enabled: true,
        model: 'deepseek-research',
      },
    ],
    a2a_agents: [
      {
        id: 'fast_reactor',
        name: '快堆专家',
        endpoint: 'http://127.0.0.1:9901',
        enabled: true,
        timeout_seconds: 120,
        bearer_token_masked: '••••••••',
        bearer_token: 'server-must-not-return-this-token',
        status: 'ready',
        last_error: null,
        last_refreshed_at: '2026-08-05T10:00:00Z',
        tool_name: 'a2a_agent_fast_reactor',
        card_summary: {
          name: 'Fast Reactor Lab',
          description: '核数据与四代快堆研究',
          version: '2.1.0',
          protocol_version: '1.0',
          skills: ['nuclear-data', 'sodium-fast-reactor'],
          supported_interfaces: [
            { url: 'http://127.0.0.1:9901/a2a', protocol_binding: 'JSONRPC', protocol_version: '1.0' },
          ],
        },
      },
      {
        id: 'materials',
        name: '材料专家',
        endpoint: 'https://materials.example.invalid',
        enabled: false,
        timeout_seconds: 60,
        bearer_token_masked: '',
        status: 'disabled',
        tool_name: 'a2a_agent_materials',
      },
    ],
    model: 'deepseek-research',
    default_workspace: '/tmp/workspace',
    revision: 9,
    overridden_fields: [],
  }
}

describe('A2A settings safety', () => {
  test('drops all server-returned plaintext credentials without mutating the response', () => {
    const received = settings()
    const sanitized = sanitizeSettings(received)

    expect(received.providers[0].api_key).toBe('server-must-not-return-this-key')
    expect(received.a2a_agents?.[0].bearer_token).toBe('server-must-not-return-this-token')
    expect('api_key' in sanitized.providers[0]).toBe(false)
    expect('bearer_token' in (sanitized.a2a_agents?.[0] ?? {})).toBe(false)
    expect(JSON.stringify(sanitized)).not.toContain('server-must-not-return')
  })

  test('preserves both agent configs and includes plaintext only for a fresh token draft', () => {
    const token = 'one-outbound-token'
    const payload = buildSettingsPayload(settings(), {}, { fast_reactor: `  ${token}  ` })

    expect(payload.providers).toHaveLength(1)
    expect(payload.a2a_agents).toHaveLength(2)
    expect(payload.a2a_agents[0]).toEqual({
      id: 'fast_reactor',
      name: '快堆专家',
      endpoint: 'http://127.0.0.1:9901',
      enabled: true,
      timeout_seconds: 120,
      bearer_token: token,
    })
    expect('bearer_token' in payload.a2a_agents[1]).toBe(false)
    expect(JSON.stringify(payload)).not.toContain('last_refreshed_at')
    expect(JSON.stringify(payload)).not.toContain('card_summary')
    expect(JSON.stringify(payload)).not.toContain('server-must-not-return')
  })

  test('an LLM-only save still carries A2A configs without re-sending a token', () => {
    const payload = buildSettingsPayload(settings(), { DeepSeek: 'new-llm-key' })

    expect(payload.providers[0].api_key).toBe('new-llm-key')
    expect(payload.a2a_agents.map((agent) => agent.id)).toEqual(['fast_reactor', 'materials'])
    expect(payload.a2a_agents.every((agent) => !('bearer_token' in agent))).toBe(true)
  })

  test('clears an existing Bearer only through an explicit outbound flag', () => {
    const payload = buildSettingsPayload(settings(), {}, { fast_reactor: 'ignored-draft' }, new Set(['fast_reactor']))

    expect(payload.a2a_agents[0]).toEqual({
      id: 'fast_reactor',
      name: '快堆专家',
      endpoint: 'http://127.0.0.1:9901',
      enabled: true,
      timeout_seconds: 120,
      clear_bearer_token: true,
    })
    expect('bearer_token' in payload.a2a_agents[0]).toBe(false)
    expect(JSON.stringify(payload)).not.toContain('ignored-draft')
  })

  test('creates an unchecked stable local draft for the harness tool', () => {
    expect(createA2aAgentDraft('nuclear_lab')).toEqual({
      id: 'nuclear_lab',
      name: '',
      endpoint: '',
      enabled: true,
      timeout_seconds: 120,
      bearer_token_masked: '',
      status: 'unchecked',
      last_error: null,
      last_refreshed_at: null,
      tool_name: 'a2a_agent_nuclear_lab',
      card_summary: null,
    })
  })
})
