import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import {
  A2A_TOOL_RESULT_SCHEMA,
  a2aTaskInterruption,
  parseA2aToolResult,
  semanticNodesForPayload,
} from '../src/api/a2aToolResult'
import A2aToolResult from '../src/components/workbench/A2aToolResult'
import ToolCallCard from '../src/components/workbench/ToolCallCard'

function envelopeContent(): string {
  return JSON.stringify({
    schema: A2A_TOOL_RESULT_SCHEMA,
    agent: {
      config_id: 'fast_reactor',
      display_name: '快堆研究 Agent',
      configured_endpoint: 'http://127.0.0.1:9901',
    },
    card: {
      fetched_at: '2026-08-05T10:00:00Z',
      sha256: '0123456789abcdef0123456789abcdef',
      summary: {
        name: 'Fast Reactor Lab',
        description: '处理核数据、不确定度量化与钠冷快堆问题。',
        agent_version: '2.1.0',
        protocol_version: 'v1',
        streaming: false,
        skills: [
          {
            id: 'nuclear-data',
            name: '核数据灵敏度分析',
            description: '协方差传播',
            tags: ['ENDF'],
            input_modes: ['text/plain'],
            output_modes: ['text/markdown'],
          },
        ],
      },
      selected_interface: {
        url: 'http://127.0.0.1:9901/a2a',
        binding: 'json_rpc',
        protocol_version: '1.0',
      },
      raw: { name: 'Fast Reactor Lab', version: '2.1.0', capabilities: { streaming: false } },
    },
    request: {
      message_id: 'msg-local-1',
      task_id: null,
      context_id: null,
      task: '分析 SFR 空泡反应性不确定度。',
    },
    responses: [
      {
        sequence: 1,
        operation: 'SendMessage',
        received_at: '2026-08-05T10:00:01Z',
        http_status: 200,
        protocol_version: 'v1',
        binding: 'json_rpc',
        request_id: 'rpc-1',
        wire_bytes: 901,
        payload: {
          jsonrpc: '2.0',
          id: 'rpc-1',
          result: {
            message: {
              messageId: 'remote-message-1',
              role: 'ROLE_AGENT',
              parts: [
                { text: '## 初步结论\n\n**推荐**先做协方差传播。<script>bad()</script> :rocket:' },
                { data: { keff: 1.00321, contributors: ['Na void', 'Pu-239'] } },
                { raw: 'AAEC', filename: 'sensitivity.bin', mediaType: 'application/octet-stream' },
                { url: 'https://example.invalid/report.pdf', filename: 'report.pdf', mediaType: 'application/pdf' },
              ],
            },
          },
        },
      },
      {
        sequence: 2,
        operation: 'GetTask',
        received_at: '2026-08-05T10:00:02Z',
        http_status: 200,
        protocol_version: 'v1',
        binding: 'http_json',
        wire_bytes: 1200,
        payload: {
          task: {
            id: 'task-1',
            contextId: 'ctx-1',
            status: {
              state: 'TASK_STATE_COMPLETED',
              message: {
                messageId: 'status-message',
                role: 'ROLE_AGENT',
                parts: [{ text: '计算完成。' }],
              },
            },
            history: [
              { messageId: 'history-1', role: 'ROLE_USER', parts: [{ text: '用户问题' }] },
              { messageId: 'history-2', role: 'ROLE_AGENT', parts: [{ data: null }] },
            ],
            artifacts: [
              {
                artifactId: 'artifact-1',
                name: '不确定度预算',
                description: '所有主导项',
                parts: [{ text: '| 核素 | 贡献 |\n|---|---:|\n| Pu-239 | 42% |' }],
              },
            ],
          },
        },
      },
      {
        sequence: 3,
        operation: 'message/send',
        received_at: '2026-08-05T10:00:03Z',
        http_status: 200,
        protocol_version: 'v03',
        binding: 'json_rpc',
        wire_bytes: 540,
        payload: {
          jsonrpc: '2.0',
          id: 'rpc-v03',
          result: {
            kind: 'message',
            messageId: 'v03-message',
            role: 'agent',
            parts: [
              {
                kind: 'file',
                file: { name: 'covariance.csv', mimeType: 'text/csv', bytes: 'bnVjbGlkZSx2YWx1ZQ==' },
              },
            ],
          },
        },
      },
      {
        sequence: 4,
        operation: 'tasks/get',
        received_at: '2026-08-05T10:00:04Z',
        http_status: 200,
        protocol_version: 'v03',
        binding: 'http_json',
        wire_bytes: 415,
        payload: {
          body: JSON.stringify({
            artifactUpdate: {
              taskId: 'task-v03',
              append: true,
              lastChunk: true,
              artifact: {
                artifactId: 'artifact-v03',
                name: '模型输入',
                parts: [{ kind: 'data', data: [1, 2, 3] }],
              },
            },
          }),
        },
      },
    ],
    terminal: {
      kind: 'task',
      task_id: 'task-1',
      context_id: 'ctx-1',
      state: 'TASK_STATE_COMPLETED',
      success: true,
      error: null,
    },
    warnings: ['远程 Agent Card 在调用前已更新。'],
  })
}

function renderResponseFrames(...indices: number[]): string {
  const envelope = JSON.parse(envelopeContent())
  envelope.responses = indices.map((index) => envelope.responses[index])
  const parsed = parseA2aToolResult(JSON.stringify(envelope))!
  return renderToStaticMarkup(<A2aToolResult result={parsed} />)
}

describe('A2A result parsing', () => {
  test('recognizes only the exact complete v1 envelope schema', () => {
    const parsed = parseA2aToolResult(envelopeContent())
    expect(parsed?.schema).toBe(A2A_TOOL_RESULT_SCHEMA)
    expect(parsed?.responses).toHaveLength(4)
    expect(parseA2aToolResult('{"schema":"dss.a2a.tool-result.v2"}')).toBeNull()
    expect(parseA2aToolResult(`{"schema":"${A2A_TOOL_RESULT_SCHEMA}","responses":[]}`)).toBeNull()
    expect(parseA2aToolResult('not-json')).toBeNull()
  })

  test('recognizes current and legacy resumable Task interruption envelopes', () => {
    const current = JSON.parse(envelopeContent())
    current.terminal = {
      kind: 'task_interrupted',
      task_id: 'task-input',
      context_id: 'ctx-input',
      state: 'TASK_STATE_INPUT_REQUIRED',
      success: true,
    }
    expect(a2aTaskInterruption(parseA2aToolResult(JSON.stringify(current))!)).toBe('input_required')

    const legacy = JSON.parse(envelopeContent())
    legacy.terminal = {
      kind: 'task',
      task_id: 'task-auth',
      context_id: 'ctx-auth',
      state: 'auth-required',
      success: false,
      error: 'remote task requires input or authentication',
    }
    expect(a2aTaskInterruption(parseA2aToolResult(JSON.stringify(legacy))!)).toBe('auth_required')
  })

  test('extracts v1/v0.3 JSON-RPC and HTTP+JSON semantic wrappers in wire order', () => {
    const parsed = parseA2aToolResult(envelopeContent())!
    expect(parsed.responses.map((frame) => semanticNodesForPayload(frame.payload).map((node) => node.kind))).toEqual([
      ['message'],
      ['task'],
      ['message'],
      ['artifact'],
    ])
  })
})

describe('A2A result presentation', () => {
  test('renders v0.3 HTTP+JSON ProtoJSON content and file fields', () => {
    const envelope = JSON.parse(envelopeContent())
    envelope.responses = [{
      sequence: 1,
      operation: 'message:send',
      received_at: '2026-08-05T10:00:05Z',
      http_status: 200,
      protocol_version: 'v03',
      binding: 'http_json',
      wire_bytes: 512,
      payload: {
        message: {
          messageId: 'v03-rest-message',
          role: 'ROLE_AGENT',
          content: [
            { text: '## v0.3 REST 结果\n\n**ProtoJSON** 已显示。' },
            { file: { name: 'remote.csv', mimeType: 'text/csv', fileWithUri: 'https://example.invalid/remote.csv' } },
            { file: { name: 'inline.bin', mimeType: 'application/octet-stream', fileWithBytes: 'AAEC' } },
          ],
        },
      },
    }]

    const html = renderToStaticMarkup(
      <A2aToolResult result={parseA2aToolResult(JSON.stringify(envelope))!} />,
    )
    expect(semanticNodesForPayload(envelope.responses[0].payload).map((node) => node.kind)).toEqual(['message'])
    expect(html).toContain('v0.3 REST 结果')
    expect(html).toContain('ProtoJSON')
    expect(html).toContain('remote.csv')
    expect(html).toContain('https://example.invalid/remote.csv')
    expect(html).toContain('inline.bin')
    expect(html).toContain('AAEC')
    expect(html).not.toContain('没有 parts/content')
  })

  test('shows every frame summary while mounting only the final frame content by default', () => {
    const parsed = parseA2aToolResult(envelopeContent())!
    const html = renderToStaticMarkup(<A2aToolResult result={parsed} />)

    expect(html).toContain(`data-a2a-tool-result="${A2A_TOOL_RESULT_SCHEMA}"`)
    expect(html).toContain('data-a2a-response-count="4"')
    expect(html.match(/data-a2a-response-sequence=/g)).toHaveLength(4)
    expect(html.match(/data-a2a-frame-summary=/g)).toHaveLength(4)
    expect(html.match(/data-a2a-frame-expanded="false"/g)).toHaveLength(3)
    expect(html.match(/data-a2a-frame-expanded="true"/g)).toHaveLength(1)
    expect(html.match(/data-a2a-frame-content=/g)).toHaveLength(1)
    expect(html).toContain('SendMessage')
    expect(html).toContain('GetTask')
    expect(html).toContain('message/send')
    expect(html).toContain('tasks/get')
    expect(html).toContain('Message · ROLE_AGENT · remote-message-1')
    expect(html).toContain('Task · TASK_STATE_COMPLETED · task-1')
    expect(html).toContain('Message · agent · v03-message')
    expect(html).toContain('Artifact · 模型输入')
    expect(html).toContain('末帧')
    expect(html).toContain('Fast Reactor Lab')
    expect(html).toContain('json_rpc')
    expect(html).toContain('TASK_STATE_COMPLETED')
    expect(html).toContain('核数据灵敏度分析')
    expect(html).toContain('artifact-v03')
    expect(html).toContain('[\n  1,\n  2,\n  3\n]')
    expect(html).toContain('查看第 4 帧完整响应 JSON')
    expect(html).not.toContain('初步结论')
    expect(html).not.toContain('不确定度预算')
    expect(html).not.toContain('covariance.csv')
    expect(html).not.toContain('查看第 1 帧完整响应 JSON')
  })

  test('retains every old frame Message/history/artifact/Part when that frame is expanded', () => {
    const messageFrame = renderResponseFrames(0)
    expect(messageFrame).toContain('初步结论')
    expect(messageFrame).toContain('<strong>推荐</strong>')
    expect(messageFrame).toContain('keff')
    expect(messageFrame).toContain('AAEC')
    expect(messageFrame).toContain('report.pdf')
    expect(messageFrame).toContain('远程 URL 仅展示')
    expect(messageFrame).toContain('data-external-url="https://example.invalid/report.pdf"')
    expect(messageFrame).not.toContain('href="https://example.invalid/report.pdf"')
    expect(messageFrame).not.toContain('<script>')
    expect(messageFrame).not.toContain('javascript:')

    const taskFrame = renderResponseFrames(1)
    expect(taskFrame).toContain('计算完成。')
    expect(taskFrame).toContain('用户问题')
    expect(taskFrame).toContain('不确定度预算')
    expect(taskFrame).toContain('Pu-239')
    expect(taskFrame).toContain('data-a2a-empty-part="true"')
    expect(taskFrame).not.toMatch(/>\s*null\s*</)

    const fileFrame = renderResponseFrames(2)
    expect(fileFrame).toContain('covariance.csv')
    expect(fileFrame).toContain('bnVjbGlkZSx2YWx1ZQ==')

    const artifactFrame = renderResponseFrames(3)
    expect(artifactFrame).toContain('artifact-v03')
    expect(artifactFrame).toContain('[\n  1,\n  2,\n  3\n]')
  })

  test('never leaks a standalone null from semantic fields or warnings', () => {
    const envelope = JSON.parse(envelopeContent())
    envelope.responses = [envelope.responses[1]]
    envelope.warnings.push(null)
    const html = renderToStaticMarkup(
      <A2aToolResult result={parseA2aToolResult(JSON.stringify(envelope))!} />,
    )

    expect(html).toContain('此 Part 的语义值为空')
    expect(html).toContain('空警告项')
    expect(html).not.toMatch(/>\s*null\s*</)
  })

  test('keeps multi-megabyte raw frames out of the DOM until the user expands them', () => {
    const envelope = JSON.parse(envelopeContent())
    const marker = `RAW_ONLY_${'x'.repeat(1_000_000)}`
    envelope.responses = [{
      sequence: 1,
      operation: 'GetTask',
      received_at: '2026-08-05T10:00:00Z',
      http_status: 200,
      protocol_version: 'v1',
      binding: 'json_rpc',
      wire_bytes: marker.length,
      payload: { opaque_unrecognized_payload: marker },
    }]
    const parsed = parseA2aToolResult(JSON.stringify(envelope))!
    const html = renderToStaticMarkup(<A2aToolResult result={parsed} />)

    expect(html).toContain('查看第 1 帧完整响应 JSON')
    expect(html).not.toContain('RAW_ONLY_')
    expect(html.length).toBeLessThan(100_000)
  })

  test('uses the specialized renderer after session restore and falls back for malformed/future envelopes', () => {
    const call = {
      type: 'tool_use' as const,
      id: 'restored-call',
      name: 'a2a_agent_fast_reactor',
      input: { task: '复核钠空泡反应性。' },
    }
    const restored = renderToStaticMarkup(
      <ToolCallCard
        call={call}
        result={{ type: 'tool_result', tool_use_id: call.id, content: envelopeContent(), is_error: false }}
      />,
    )
    expect(restored).toContain('A2A Agent 调用')
    expect(restored).toContain('aria-expanded="true"')
    expect(restored).toContain(`data-a2a-tool-result="${A2A_TOOL_RESULT_SCHEMA}"`)
    expect(restored).toContain('委派给远程 Agent')

    for (const content of [
      `{"schema":"${A2A_TOOL_RESULT_SCHEMA}","responses":[]}`,
      '{"schema":"dss.a2a.tool-result.v2","opaque":true}',
    ]) {
      const fallback = renderToStaticMarkup(
        <ToolCallCard
          call={call}
          result={{ type: 'tool_result', tool_use_id: call.id, content, is_error: true }}
        />,
      )
      expect(fallback).not.toContain('data-a2a-tool-result=')
      expect(fallback).toContain('失败')
      expect(fallback).toContain('schema')
    }
  })

  test('keeps structured remote failures inspectable instead of flattening them to plain text', () => {
    const failed = JSON.parse(envelopeContent())
    failed.responses = []
    delete failed.card
    failed.terminal = {
      kind: 'transport_error',
      success: false,
      error: 'Agent Card refresh failed before SendMessage',
    }
    const call = {
      type: 'tool_use' as const,
      id: 'failed-call',
      name: 'a2a_agent_fast_reactor',
      input: { task: 'run' },
    }
    const html = renderToStaticMarkup(
      <ToolCallCard
        call={call}
        result={{ type: 'tool_result', tool_use_id: call.id, content: JSON.stringify(failed), is_error: true }}
      />,
    )
    expect(html).toContain(`data-a2a-tool-result="${A2A_TOOL_RESULT_SCHEMA}"`)
    expect(html).toContain('Agent Card refresh failed before SendMessage')
    expect(html).toContain('Agent Card 未能刷新')
    expect(html).toContain('没有接收到可保存的远程响应帧')
  })

  test('presents a submitted long task as resumable rather than completed', () => {
    const pending = JSON.parse(envelopeContent())
    pending.request.action = 'submit'
    pending.responses = [pending.responses[0]]
    pending.terminal = {
      kind: 'task_pending',
      task_id: 'task-long-1',
      context_id: 'ctx-long-1',
      state: 'TASK_STATE_SUBMITTED',
      success: true,
    }
    const html = renderToStaticMarkup(
      <A2aToolResult result={parseA2aToolResult(JSON.stringify(pending))!} />,
    )

    expect(html).toContain('data-a2a-task-pending="true"')
    expect(html).toContain('任务已提交')
    expect(html).toContain('等待远程完成')
    expect(html).toContain('action submit')
    expect(html).toContain('get_task')
    expect(html).not.toContain('action send')

    const call = {
      type: 'tool_use' as const,
      id: 'pending-call',
      name: 'a2a_agent_fast_reactor',
      input: { action: 'submit', task: 'run long task' },
    }
    const card = renderToStaticMarkup(
      <ToolCallCard
        call={call}
        result={{ type: 'tool_result', tool_use_id: call.id, content: JSON.stringify(pending), is_error: false }}
      />,
    )
    expect(card).toContain('data-tool-status="pending"')
    expect(card).toContain('远程运行中')
    expect(card).not.toContain('已完成')
  })

  test('presents input/auth requirements as resumable interruptions, including legacy envelopes', () => {
    const cases = [
      {
        state: 'TASK_STATE_INPUT_REQUIRED',
        kind: 'task_interrupted',
        success: true,
        outerError: false,
        status: 'input_required',
        label: '等待输入',
      },
      {
        state: 'auth-required',
        kind: 'task',
        success: false,
        outerError: true,
        status: 'auth_required',
        label: '等待认证',
      },
    ] as const

    for (const item of cases) {
      const interrupted = JSON.parse(envelopeContent())
      interrupted.request.action = 'get_task'
      interrupted.terminal = {
        kind: item.kind,
        task_id: 'task-paused-1',
        context_id: 'ctx-paused-1',
        state: item.state,
        success: item.success,
        error: item.outerError ? 'legacy failure-shaped interruption' : null,
      }
      const call = {
        type: 'tool_use' as const,
        id: `interrupted-${item.status}`,
        name: 'a2a_agent_fast_reactor',
        input: { action: 'get_task', task_id: 'task-paused-1' },
      }
      const html = renderToStaticMarkup(
        <ToolCallCard
          call={call}
          result={{
            type: 'tool_result',
            tool_use_id: call.id,
            content: JSON.stringify(interrupted),
            is_error: item.outerError,
          }}
        />,
      )

      expect(html).toContain(`data-tool-status="${item.status}"`)
      expect(html).toContain(item.label)
      expect(html).toContain(`data-a2a-task-interrupted="${item.status}"`)
      expect(html).toContain('尚未完成也未失败')
      expect(html).toContain('<code>action=send</code>')
      expect(html).toContain('携带上方 task id 与 context id 续接原 Task')
      expect(html).toContain('<code>get_task</code>')
      expect(html).not.toContain('data-tool-status="error"')
      expect(html).not.toContain('data-a2a-terminal-error')
      expect(html).not.toContain('legacy failure-shaped interruption')
    }
  })
})
