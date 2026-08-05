// 工具调用卡片：名称、动作摘要和终态始终可见；展开查看完整输入/输出。
import { useState } from 'react'
import type { ContentBlock } from '../../types'
import { a2aTaskInterruption, parseA2aToolResult } from '../../api/a2aToolResult'
import CodeBlock from './CodeBlock'
import A2aToolResult from './A2aToolResult'
import { IconChevronDown, IconChevronRight, IconTerminal } from '../icons'

type ToolUse = Extract<ContentBlock, { type: 'tool_use' }>
type ToolResult = Extract<ContentBlock, { type: 'tool_result' }>

interface Props {
  call: ToolUse
  result?: ToolResult
}

function textValue(value: unknown): string {
  return typeof value === 'string' ? value : ''
}

export function summarizeToolCall(call: ToolUse): string {
  const input = call.input as Record<string, unknown>
  if (call.name.toLowerCase().startsWith('a2a_agent_')) {
    return `委派给远程 Agent：${textValue(input.task) || '等待任务内容'}`
  }
  switch (call.name.toLowerCase()) {
    case 'read_file':
    case 'read':
      return `读取文件 ${textValue(input.path ?? input.file_path)}`.trim()
    case 'write_file':
      return `写入文件 ${textValue(input.path)}`.trim()
    case 'edit_file':
      return `编辑文件 ${textValue(input.path)}`.trim()
    case 'list_files':
      return `列出目录 ${textValue(input.path) || '.'}`
    case 'bash':
    case 'shell':
      return textValue(input.command) || '运行 Shell 命令'
    case 'python':
      return '运行 Python 代码'
    case 'compile_pdf':
      return `编译 PDF ${textValue(input.path ?? input.tex_path)}`.trim()
    case 'web_search':
      return `搜索网络 ${textValue(input.query)}`.trim()
    case 'fetch_url':
      return `读取网页 ${textValue(input.url)}`.trim()
    case 'search_skills':
      return `搜索技能 ${textValue(input.query)}`.trim()
    case 'list_skills':
      return '列出可用技能'
    case 'skill':
      return `加载技能 ${textValue(input.name ?? input.skill)}`.trim()
    case 'generate_plan':
      return '生成研究计划'
    case 'update_step_status':
      return `更新计划步骤 ${textValue(input.status)}`.trim()
    case 'delegate':
      return `委派子任务 ${textValue(input.task ?? input.prompt)}`.trim()
    case 'submit_output':
      return '提交研究产物'
    case 'ask_user':
      return `向用户提问 ${textValue(input.question)}`.trim()
    default:
      return `调用 ${call.name}`
  }
}

export function toolResultLanguage(toolName: string): string {
  const normalized = toolName.toLowerCase()
  if (normalized === 'bash' || normalized === 'shell') return 'shell'
  if (normalized === 'python') return 'python'
  return 'text'
}

interface CodeInput {
  code: string
  language: 'python' | 'shell'
}

function splitToolInput(call: ToolUse): {
  codeInput: CodeInput | null
  remainingInput: Record<string, unknown>
} {
  const remainingInput = { ...call.input }
  const normalized = call.name.toLowerCase()
  if (normalized === 'python' && typeof remainingInput.code === 'string') {
    const code = remainingInput.code
    delete remainingInput.code
    return { codeInput: { code, language: 'python' }, remainingInput }
  }
  if (
    (normalized === 'bash' || normalized === 'shell') &&
    typeof remainingInput.command === 'string'
  ) {
    const code = remainingInput.command
    delete remainingInput.command
    return { codeInput: { code, language: 'shell' }, remainingInput }
  }
  return { codeInput: null, remainingInput }
}

export function toolStatus(
  result?: ToolResult,
): 'running' | 'pending' | 'input_required' | 'auth_required' | 'interrupted' | 'success' | 'error' {
  if (!result) return 'running'
  const a2aResult = parseA2aToolResult(result.content)
  if (a2aResult?.terminal.kind === 'task_pending') return 'pending'
  if (a2aResult) {
    const interruption = a2aTaskInterruption(a2aResult)
    if (interruption) return interruption
  }
  return result.is_error ? 'error' : 'success'
}

const STATUS = {
  running: { label: '运行中', dot: 'bg-brand animate-pulse', text: 'text-brand', bg: 'bg-brandSoft' },
  pending: { label: '远程运行中', dot: 'bg-amber-500', text: 'text-amber-700', bg: 'bg-amber-500/10' },
  input_required: { label: '等待输入', dot: 'bg-amber-500', text: 'text-amber-700', bg: 'bg-amber-500/10' },
  auth_required: { label: '等待认证', dot: 'bg-amber-500', text: 'text-amber-700', bg: 'bg-amber-500/10' },
  interrupted: { label: '等待继续', dot: 'bg-amber-500', text: 'text-amber-700', bg: 'bg-amber-500/10' },
  success: { label: '已完成', dot: 'bg-success', text: 'text-success', bg: 'bg-success/10' },
  error: { label: '失败', dot: 'bg-danger', text: 'text-danger', bg: 'bg-dangerSoft' },
} as const

export default function ToolCallCard({ call, result }: Props) {
  const status = toolStatus(result)
  const appearance = STATUS[status]
  const isA2a = call.name.toLowerCase().startsWith('a2a_agent_')
  const a2aResult = result && isA2a ? parseA2aToolResult(result.content) : null
  // 正在运行、远程 pending/中断和失败的调用默认展开，成功历史默认收起。
  const [open, setOpen] = useState(() => status !== 'success' || isA2a)
  const { codeInput, remainingInput } = splitToolInput(call)
  const inputJson =
    Object.keys(remainingInput).length > 0 ? JSON.stringify(remainingInput, null, 2) : null

  return (
    <section
      className="overflow-hidden rounded-lg border border-border bg-bg shadow-[0_1px_0_rgba(0,0,0,0.02)]"
      aria-label={`工具调用 ${call.name}，${appearance.label}`}
      data-tool-call-id={call.id}
      data-tool-status={status}
    >
      <button
        type="button"
        className="flex w-full items-center gap-2.5 px-3 py-2.5 text-left hover:bg-surface"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
      >
        {open ? (
          <IconChevronDown width={13} height={13} className="shrink-0 text-ink3" />
        ) : (
          <IconChevronRight width={13} height={13} className="shrink-0 text-ink3" />
        )}
        <span className="flex h-6 w-6 shrink-0 items-center justify-center rounded-md bg-surface">
          <IconTerminal width={13} height={13} className="text-ink2" />
        </span>
        <span className="min-w-0 flex-1">
          <span className="flex min-w-0 items-center gap-2">
            <span className="shrink-0 text-[10px] font-medium uppercase tracking-[0.08em] text-ink3">
              {isA2a ? 'A2A Agent 调用' : '工具调用'}
            </span>
            <code className="truncate rounded bg-surface px-1.5 py-0.5 text-[11px] font-medium text-ink">
              {call.name}
            </code>
          </span>
          <span className="mt-0.5 block truncate text-[12px] text-ink2">
            {summarizeToolCall(call)}
          </span>
        </span>
        <span
          className={`flex shrink-0 items-center gap-1.5 rounded-full px-2 py-1 text-[10px] font-medium ${appearance.bg} ${appearance.text}`}
        >
          <span className={`h-1.5 w-1.5 rounded-full ${appearance.dot}`} />
          {appearance.label}
        </span>
      </button>

      {open && (
        <div className="space-y-3 border-t border-border bg-surface/40 px-3 py-3">
          <div>
            <div className="mb-1.5 text-[10px] font-medium uppercase tracking-[0.08em] text-ink3">
              输入参数
            </div>
            <div className="space-y-2">
              {codeInput && (
                <div data-tool-input-code-language={codeInput.language}>
                  <CodeBlock code={codeInput.code} lang={codeInput.language} />
                </div>
              )}
              {inputJson && (
                <div>
                  {codeInput && (
                    <div className="mb-1 text-[10px] font-medium text-ink3">其他参数</div>
                  )}
                  <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-words rounded-md border border-border bg-bg p-2.5 font-mono text-[11px] leading-[1.55] text-ink2">
                    {inputJson}
                  </pre>
                </div>
              )}
              {!codeInput && !inputJson && (
                <pre className="rounded-md border border-border bg-bg p-2.5 font-mono text-[11px] leading-[1.55] text-ink2">
                  无参数
                </pre>
              )}
            </div>
          </div>

          <div>
            <div className="mb-1.5 text-[10px] font-medium uppercase tracking-[0.08em] text-ink3">
              {status === 'running' ? '执行状态' : '执行结果'}
            </div>
            {!result ? (
              <div className="rounded-md border border-brand/20 bg-brandSoft px-2.5 py-2 text-[11px] text-brand">
                工具正在执行，结果会自动更新并持久化到当前会话。
              </div>
            ) : a2aResult ? (
              <A2aToolResult result={a2aResult} />
            ) : result.is_error ? (
              <pre className="max-h-80 overflow-auto whitespace-pre-wrap break-words rounded-md border border-danger/30 bg-dangerSoft p-2.5 font-mono text-[11px] leading-[1.55] text-danger">
                {result.content || '工具返回错误，但没有提供详情。'}
              </pre>
            ) : result.content.includes('\n') || result.content.startsWith('vid') ? (
              <div className="max-h-80 overflow-auto rounded-md">
                <CodeBlock code={result.content} lang={toolResultLanguage(call.name)} />
              </div>
            ) : (
              <pre className="max-h-80 overflow-auto whitespace-pre-wrap break-words rounded-md border border-border bg-bg p-2.5 font-mono text-[11px] leading-[1.55] text-ink2">
                {result.content || '工具已成功完成，没有文本输出。'}
              </pre>
            )}
          </div>
        </div>
      )}
    </section>
  )
}
