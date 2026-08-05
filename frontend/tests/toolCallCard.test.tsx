import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import ToolCallCard, {
  summarizeToolCall,
  toolResultLanguage,
  toolStatus,
} from '../src/components/workbench/ToolCallCard'

const pythonCall = {
  type: 'tool_use' as const,
  id: 'call-python',
  name: 'python',
  input: { code: 'print(42)' },
}

describe('tool result presentation', () => {
  test('labels multiline output with the originating tool language', () => {
    expect(toolResultLanguage('bash')).toBe('shell')
    expect(toolResultLanguage('Bash')).toBe('shell')
    expect(toolResultLanguage('SHELL')).toBe('shell')
    expect(toolResultLanguage('python')).toBe('python')
    expect(toolResultLanguage('compile_pdf')).toBe('text')
  })

  test('covers built-in tool summaries and terminal states', () => {
    expect(summarizeToolCall(pythonCall)).toBe('运行 Python 代码')
    expect(
      summarizeToolCall({ type: 'tool_use', id: 'w', name: 'web_search', input: { query: 'RNA' } }),
    ).toBe('搜索网络 RNA')
    expect(toolStatus()).toBe('running')
    expect(toolStatus({ type: 'tool_result', tool_use_id: 'x', content: '', is_error: false })).toBe(
      'success',
    )
    expect(toolStatus({ type: 'tool_result', tool_use_id: 'x', content: '', is_error: true })).toBe(
      'error',
    )
  })

  test('makes running and failed calls explicit and inspectable', () => {
    const running = renderToStaticMarkup(<ToolCallCard call={pythonCall} />)
    expect(running).toContain('工具调用')
    expect(running).toContain('运行中')
    expect(running).toContain('输入参数')
    expect(running).toContain('data-tool-input-code-language="python"')
    expect(running).toContain('print')
    expect(running).toContain('42')
    expect(running).toContain('结果会自动更新并持久化')

    const failed = renderToStaticMarkup(
      <ToolCallCard
        call={pythonCall}
        result={{ type: 'tool_result', tool_use_id: 'call-python', content: 'boom', is_error: true }}
      />,
    )
    expect(failed).toContain('失败')
    expect(failed).toContain('执行结果')
    expect(failed).toContain('boom')
    expect(failed).toContain('aria-expanded="true"')
  })

  test('renders Python source as code once and keeps only additional JSON parameters', () => {
    const html = renderToStaticMarkup(
      <ToolCallCard
        call={{
          type: 'tool_use',
          id: 'python-with-timeout',
          name: 'PYTHON',
          input: { code: 'print("alpha")\n# agenda check', timeout: 30 },
        }}
      />,
    )

    expect(html).toContain('data-tool-input-code-language="python"')
    expect(html).toContain('>python<')
    expect(html).toContain('其他参数')
    expect(html).toContain('&quot;timeout&quot;')
    expect(html).not.toContain('&quot;code&quot;')
    expect(html).not.toContain('\\n')
    expect(html.match(/agenda check/g)).toHaveLength(1)
  })

  test('renders Bash and Shell commands as shell code without duplicating command', () => {
    for (const name of ['Bash', 'SHELL']) {
      const html = renderToStaticMarkup(
        <ToolCallCard
          call={{
            type: 'tool_use',
            id: `shell-${name}`,
            name,
            input: { command: 'printf shell-once', cwd: '/tmp' },
          }}
        />,
      )
      expect(html).toContain('data-tool-input-code-language="shell"')
      expect(html).toContain('>shell<')
      expect(html).toContain('&quot;cwd&quot;')
      expect(html).not.toContain('&quot;command&quot;')
      expect(html.match(/shell-once/g)).toHaveLength(1)
    }
  })

  test('keeps generic JSON fallback for non-string and unknown code fields', () => {
    const nonString = renderToStaticMarkup(
      <ToolCallCard
        call={{ type: 'tool_use', id: 'bad-python', name: 'python', input: { code: 42 } }}
      />,
    )
    expect(nonString).not.toContain('data-tool-input-code-language')
    expect(nonString).toContain('&quot;code&quot;')

    const unknown = renderToStaticMarkup(
      <ToolCallCard
        call={{ type: 'tool_use', id: 'unknown', name: 'custom', input: { code: 'raw' } }}
      />,
    )
    expect(unknown).not.toContain('data-tool-input-code-language')
    expect(unknown).toContain('&quot;code&quot;')
    expect(unknown).toContain('raw')
  })
})
