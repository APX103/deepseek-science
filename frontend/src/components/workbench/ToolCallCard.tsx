// 工具调用卡片：可折叠。标题行 = 工具名 + 一句摘要；展开看 input / 结果。
import { useState } from 'react'
import type { ContentBlock } from '../../types'
import CodeBlock from './CodeBlock'
import { IconChevronDown, IconChevronRight, IconTerminal } from '../icons'

type ToolUse = Extract<ContentBlock, { type: 'tool_use' }>
type ToolResult = Extract<ContentBlock, { type: 'tool_result' }>

interface Props {
  call: ToolUse
  result?: ToolResult
}

function summarize(call: ToolUse): string {
  const input = call.input as Record<string, unknown>
  if (call.name === 'read_file' || call.name === 'Read') return `读取 ${String(input.path ?? input.file_path ?? '')}`
  if (call.name === 'write_file') return `写入 ${String(input.path ?? '')}`
  if (call.name === 'edit_file') return `编辑 ${String(input.path ?? '')}`
  if (call.name === 'list_files') return `列目录 ${String(input.path ?? '.')}`
  if (call.name === 'bash' || call.name === 'Bash') return String(input.command ?? '')
  if (call.name === 'ask_user') return '向用户提问'
  return call.name
}

export default function ToolCallCard({ call, result }: Props) {
  const [open, setOpen] = useState(false)
  const failed = result?.is_error

  return (
    <div className="overflow-hidden rounded-md border border-border bg-bg">
      <button
        className="flex w-full items-center gap-2 px-3 py-2 text-left hover:bg-surface"
        onClick={() => setOpen((v) => !v)}
      >
        {open ? (
          <IconChevronDown width={12} height={12} className="shrink-0 text-ink3" />
        ) : (
          <IconChevronRight width={12} height={12} className="shrink-0 text-ink3" />
        )}
        <IconTerminal width={12} height={12} className="shrink-0 text-ink3" />
        <span className="min-w-0 flex-1 truncate text-[12px] text-ink2">{summarize(call)}</span>
        {failed ? (
          <span className="shrink-0 rounded bg-dangerSoft px-1.5 py-0.5 text-[10px] font-medium text-danger">
            error
          </span>
        ) : (
          <span className="shrink-0 text-[10px] text-ink3">4 steps · 2 failed</span>
        )}
      </button>

      {open && (
        <div className="space-y-2 border-t border-border px-3 py-2">
          {/* input 参数 */}
          <div className="font-mono text-[11px] leading-[1.6] text-ink2">
            {Object.entries(call.input).map(([k, v]) => (
              <div key={k}>
                <span className="text-ink3">{k}</span>{' '}
                <span className="text-brand">{JSON.stringify(v)}</span>
              </div>
            ))}
          </div>
          {/* 结果：错误 JSON 用红底块；python 输出用代码块 */}
          {result &&
            (result.is_error ? (
              <pre className="overflow-x-auto rounded-md border border-danger/30 bg-dangerSoft p-2 font-mono text-[11px] leading-[1.6] text-danger">
                {result.content}
              </pre>
            ) : result.content.includes('\n') || result.content.startsWith('vid') ? (
              <CodeBlock code={result.content} lang="python" />
            ) : (
              <pre className="overflow-x-auto rounded-md bg-surface p-2 font-mono text-[11px] text-ink2">
                {result.content}
              </pre>
            ))}
        </div>
      )}
    </div>
  )
}
