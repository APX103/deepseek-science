import { describe, expect, test } from 'bun:test'
import { readFileSync } from 'node:fs'
import { renderToStaticMarkup } from 'react-dom/server'
import { AgentThinkingEditor } from '../src/components/SettingsModal'

const settingsModalSource = readFileSync(
  new URL('../src/components/SettingsModal.tsx', import.meta.url),
  'utf8',
)

function renderEditor(overrides: Partial<Parameters<typeof AgentThinkingEditor>[0]> = {}): string {
  return renderToStaticMarkup(
    <AgentThinkingEditor
      thinkingEnabled={true}
      effort="high"
      maxIterationsDraft="160"
      saving={false}
      canSave={true}
      validationError={null}
      saveError={null}
      saveSucceeded={false}
      onThinkingEnabledChange={() => {}}
      onEffortChange={() => {}}
      onMaxIterationsDraftChange={() => {}}
      onSave={() => {}}
      {...overrides}
    />,
  )
}

describe('combined Agent thinking settings UI', () => {
  test('renders one accessible card with the switch, strict effort enum, and iteration contract', () => {
    const html = renderEditor()

    expect(html).toContain('data-agent-thinking-settings="true"')
    expect(html).toMatch(/<button[^>]*role="switch"[^>]*aria-checked="true"/)
    expect(html).toContain('aria-labelledby="agent-thinking-enabled-label"')
    expect(html).toContain('思考深度')
    expect(html).toContain('<option value="low">低</option>')
    expect(html).toContain('<option value="high" selected="">高</option>')
    expect(html).toContain('<option value="max">最大</option>')
    expect(html).toContain('最大思考轮次')
    expect(html).toMatch(/<input[^>]*type="number"/)
    expect(html).toMatch(/<input[^>]*min="1"/)
    expect(html).toMatch(/<input[^>]*max="1000"/)
    expect(html).toMatch(/<input[^>]*step="1"/)
    expect(html).toMatch(/<input[^>]*value="160"/)
    expect(html).toContain('每次 Agent 运行的模型/工具迭代总上限')
    expect(html).toContain('不是模型的推理深度')
    expect(html).toContain('默认 100')
    expect(html).toContain('后续新请求立即生效')
    expect(html).toContain('已经运行中的请求保持开始时的设置')
    expect(html).toContain('增加耗时和费用')
  })

  test('turning Think off disables but retains the selected effort', () => {
    const html = renderEditor({ thinkingEnabled: false, effort: 'max' })

    expect(html).toMatch(/<button[^>]*role="switch"[^>]*aria-checked="false"/)
    expect(html).toMatch(/<select[^>]*disabled=""/)
    expect(html).toContain('<option value="max" selected="">最大</option>')
    expect(html).toContain('已保留当前思考深度')
  })

  test('disables the shared save and exposes an invalid iteration draft as an alert', () => {
    const html = renderEditor({
      maxIterationsDraft: '1e2',
      canSave: false,
      validationError: '请输入 1–1000 之间的十进制整数（不支持小数或科学计数法）。',
    })

    expect(html).toContain('role="alert"')
    expect(html).toContain('不支持小数或科学计数法')
    expect(html).toMatch(/<button[^>]*disabled=""[^>]*>保存<\/button>/)
  })

  test('disables all controls while saving and reports only confirmed success', () => {
    const saving = renderEditor({ saving: true, canSave: false })
    expect(saving).toMatch(/<button[^>]*role="switch"[^>]*disabled=""/)
    expect(saving).toMatch(/<select[^>]*disabled=""/)
    expect(saving).toMatch(/<input[^>]*disabled=""/)
    expect(saving).toContain('保存中…')

    const confirmed = renderEditor({ canSave: false, saveSucceeded: true })
    expect(confirmed).toContain('role="status"')
    expect(confirmed).toContain('已保存，后续新请求立即生效')

    const rejected = renderEditor({ canSave: false, saveError: '后端未精确确认完整设置' })
    expect(rejected).toContain('role="alert"')
    expect(rejected).toContain('后端未精确确认完整设置')
  })

  test('uses one coherent dirty/save path and exact three-field reconciliation', () => {
    const cardStart = settingsModalSource.indexOf('function AgentThinkingCard()')
    const cardEnd = settingsModalSource.indexOf('// ---------- 学术数据源 API keys ----------', cardStart)
    const cardSource = settingsModalSource.slice(cardStart, cardEnd)

    expect(cardStart).toBeGreaterThanOrEqual(0)
    expect(cardEnd).toBeGreaterThan(cardStart)
    expect(settingsModalSource).not.toContain('function MaxIterationsCard()')
    expect(cardSource).toContain('thinkingEnabledDraft !== confirmedValues.thinking.enabled')
    expect(cardSource).toContain('effortDraft !== confirmedValues.thinking.effort')
    expect(cardSource).toContain('parsedMaxIterations !== confirmedValues.maxIterations')
    expect(cardSource).toContain('payload.thinking = { ...requested.thinking }')
    expect(cardSource).toContain('payload.max_iterations = requested.maxIterations')
    expect(cardSource).toContain('reconcileAgentThinkingSaveResponse(')
  })

  test('refreshes a stale revision without replacing any user draft', () => {
    const cardStart = settingsModalSource.indexOf('function AgentThinkingCard()')
    const cardEnd = settingsModalSource.indexOf('// ---------- 学术数据源 API keys ----------', cardStart)
    const cardSource = settingsModalSource.slice(cardStart, cardEnd)
    const saveStart = cardSource.indexOf('const save = async () =>')
    const catchStart = cardSource.indexOf('} catch (error) {', saveStart)
    const finallyStart = cardSource.indexOf('} finally {', catchStart)
    const failurePath = cardSource.slice(catchStart, finallyStart)

    expect(failurePath).toContain('const latest = sanitizeSettings(await getSettings())')
    expect(failurePath).toContain('setSettings(latest)')
    expect(failurePath).toContain('setConfirmedValues(agentThinkingValues(latest))')
    expect(failurePath).not.toContain('setThinkingEnabledDraft(')
    expect(failurePath).not.toContain('setEffortDraft(')
    expect(failurePath).not.toContain('setMaxIterationsDraft(')
  })
})
