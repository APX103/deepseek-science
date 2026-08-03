// 中间对话流：空态欢迎区 / 消息流 + 失败横幅 + 流式渲染区 + 底部输入框。
// 发送走 store 流式 buffer（connectSSE 由 WorkbenchPage 接线）；离线时输入框禁用。
import { useMemo, useState } from 'react'
import type { ContentBlock, Message } from '../../types'
import type { StreamBuffer } from '../../store'
import { useApp } from '../../App'
import ToolCallCard from './ToolCallCard'
import { IconBook, IconChevronRight, IconPlus, IconRefresh, IconSend, IconStop } from '../icons'

interface Props {
  messages: Message[]
  /** 失败会话才显示 Agent Failed / interrupted 横幅 */
  failed?: boolean
  /** 当前会话的流式 buffer（无则为 undefined） */
  stream?: StreamBuffer
  onSend: (text: string) => void
  onStop: () => void
}

type ToolUse = Extract<ContentBlock, { type: 'tool_use' }>
type ToolResult = Extract<ContentBlock, { type: 'tool_result' }>

export default function ChatArea({ messages, failed, stream, onSend, onStop }: Props) {
  const running = stream?.running ?? false

  if (messages.length === 0 && !stream) {
    // 新会话空态：居中欢迎区 + 大输入框
    return (
      <div className="flex min-w-0 flex-1 flex-col items-center justify-center px-6">
        <h1 className="text-[20px] font-semibold text-ink">有什么可以帮你？</h1>
        <div className="mt-6 w-full max-w-xl">
          <Composer large running={running} onSend={onSend} onStop={onStop} />
          <BackendHint />
        </div>
        <p className="mt-3 text-[12px] text-ink3">@ for artifacts · # for sessions · / for skills · ⌘K to search</p>
      </div>
    )
  }

  return (
    <div className="flex min-w-0 flex-1 flex-col">
      {/* 消息流 */}
      <div className="min-h-0 flex-1 overflow-y-auto">
        <div className="mx-auto max-w-2xl space-y-4 px-6 py-6">
          {messages.map((m, i) => (
            <MessageView key={i} message={m} />
          ))}

          {/* 流式渲染区：thinking 折叠块 + 工具卡片 + text 打字机 */}
          {stream && (stream.running || stream.stopped) && (
            <div className="space-y-2">
              {stream.thinking && <ThinkingBlock text={stream.thinking} running={stream.running} />}
              {stream.toolCalls.map((c) => (
                <ToolCallCard
                  key={c.id}
                  call={{ type: 'tool_use', id: c.id, name: c.name, input: c.input }}
                  result={
                    c.resolved
                      ? { type: 'tool_result', tool_use_id: c.id, content: c.content ?? '', is_error: !!c.is_error }
                      : undefined
                  }
                />
              ))}
              {stream.text && <p className="whitespace-pre-wrap text-[13px] leading-[1.6] text-ink">{stream.text}</p>}
              {stream.running && !stream.thinking && !stream.text && stream.toolCalls.length === 0 && (
                <p className="text-[12px] text-ink3">思考中…</p>
              )}
              {stream.stopped && <p className="text-[11px] text-ink3">已停止</p>}
            </div>
          )}

          {/* ask_user 阻塞面板：run 挂起等用户回复 */}
          {stream && !stream.running && stream.kind === 'awaiting' && stream.pendingAsk && (
            <AskUserPanel ask={stream.pendingAsk} />
          )}

          {/* 流式错误横幅（沿用 Agent Failed 样式） */}
          {stream?.error && <ErrorBanner message={stream.error} />}

          {/* complete 后的 usage 行 */}
          {stream && !stream.running && !stream.error && stream.usage && (
            <p className="text-[11px] text-ink3">
              tokens: {stream.usage.input_tokens} in / {stream.usage.output_tokens} out · {stream.iterations}{' '}
              iteration{stream.iterations > 1 ? 's' : ''}
            </p>
          )}

          {failed && (
            <>
              <ErrorBanner message={`400 {"error":{"message":"The input you provided is invalid","type":"input_invalid"}}`} />

              {/* This session was interrupted */}
              <div className="flex items-center justify-between rounded-md border border-border bg-surface px-3 py-2">
                <span className="text-[12px] text-ink2">This session was interrupted.</span>
                <button className="btn-outline py-1 text-[12px]">
                  <IconRefresh width={12} height={12} /> Resume
                </button>
              </div>

              {/* Notebook 条 */}
              <button className="flex w-full items-center gap-2 rounded-md border border-border bg-surface px-3 py-2 text-[12px] text-ink2 hover:bg-surface2">
                <IconBook width={13} height={13} /> Notebook
              </button>
            </>
          )}
        </div>
      </div>

      {/* 底部输入框 */}
      <div className="shrink-0 border-t border-border px-6 py-3">
        <div className="mx-auto max-w-2xl">
          <Composer running={running} onSend={onSend} onStop={onStop} />
          <BackendHint />
        </div>
      </div>
    </div>
  )
}

/** Agent Failed 红条（mock 失败横幅与流式 error 事件共用）。 */
function ErrorBanner({ message }: { message: string }) {
  return (
    <div className="rounded-md border border-danger/30 bg-dangerSoft p-3">
      <div className="flex items-center gap-1.5 text-[13px] font-medium text-danger">
        <span className="inline-block h-1.5 w-1.5 rounded-full bg-danger" /> Agent Failed
      </div>
      <pre className="mt-1.5 overflow-x-auto font-mono text-[11px] text-danger">{message}</pre>
    </div>
  )
}

/** ask_user 阻塞面板：展示挂起的问题 + 候选项。 */
function AskUserPanel({ ask }: { ask: import('../../types').PendingAsk }) {
  return (
    <div className="rounded-md border border-brand/40 bg-brandSoft p-3">
      {ask.header && <div className="text-[11px] font-medium text-brand">{ask.header}</div>}
      <p className="mt-1 text-[13px] text-ink">{ask.question}</p>
      {ask.options && ask.options.length > 0 && (
        <div className="mt-2 flex flex-wrap gap-2">
          {ask.options.map((o, i) => (
            <span
              key={i}
              className="rounded-md border border-border bg-bg px-2.5 py-1 text-[12px] text-ink2"
            >
              {o.label}
            </span>
          ))}
        </div>
      )}
      <p className="mt-2 text-[11px] text-ink3">Agent 正在等待你的回复 — 在下方输入框继续。</p>
    </div>
  )
}

/** thinking 增量：可折叠块，默认收起。 */
function ThinkingBlock({ text, running }: { text: string; running: boolean }) {
  const [open, setOpen] = useState(false)
  return (
    <div className="rounded-md border border-border bg-surface">
      <button
        className="flex w-full items-center gap-1.5 px-3 py-2 text-left text-[12px] font-medium text-ink2"
        onClick={() => setOpen((v) => !v)}
      >
        <IconChevronRight width={12} height={12} className={`text-ink3 transition-transform ${open ? 'rotate-90' : ''}`} />
        Thinking{running ? '…' : ''}
      </button>
      {open && (
        <div className="max-h-64 overflow-y-auto whitespace-pre-wrap border-t border-border px-3 py-2 text-[12px] leading-relaxed text-ink2">
          {text}
        </div>
      )}
    </div>
  )
}

/** 后端状态提示：离线/未配置禁用输入并提示；在线显示模型。 */
function BackendHint() {
  const { backend } = useApp()
  if (!backend.online) {
    return <p className="mt-2 text-[12px] text-danger">后端未连接 — 输入已禁用（已有内容仍可浏览）</p>
  }
  if (!backend.llmConfigured) {
    return <p className="mt-2 text-[12px] text-danger">后端已连接，但 LLM 未配置 — 请先在 Settings 中配置 provider</p>
  }
  return (
    <p className="mt-2 flex items-center gap-1.5 text-[11px] text-ink3">
      <span className="inline-block h-1.5 w-1.5 rounded-full bg-success" />
      已连接后端{backend.model ? ` · ${backend.model}` : ''}
    </p>
  )
}

interface ComposerProps {
  large?: boolean
  running: boolean
  onSend: (text: string) => void
  onStop: () => void
}

function Composer({ large, running, onSend, onStop }: ComposerProps) {
  const { backend } = useApp()
  const [value, setValue] = useState('')
  const ready = backend.online && backend.llmConfigured

  const submit = () => {
    if (!value.trim() || !ready || running) return
    onSend(value)
    setValue('')
  }

  return (
    <div className="rounded-lg border border-border bg-bg focus-within:border-brand">
      <textarea
        rows={large ? 3 : 2}
        value={value}
        disabled={!ready || running}
        onChange={(e) => setValue(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
            e.preventDefault()
            submit()
          }
        }}
        placeholder={
          ready ? 'Ask anything — @ for artifacts, # for sessions, / for skills, ⌘K to search…' : '后端未连接…'
        }
        className="w-full resize-none bg-transparent px-3 pt-2.5 text-[13px] outline-none placeholder:text-ink3 disabled:opacity-50"
      />
      <div className="flex items-center gap-1 px-2 pb-2">
        <button className="btn-ghost rounded p-1.5" title="添加（TODO）">
          <IconPlus width={14} height={14} />
        </button>
        <div className="ml-auto flex items-center gap-2">
          <button className="btn-ghost py-1 text-[12px]">Default</button>
          {running ? (
            <button className="btn-outline rounded p-1.5" title="停止" onClick={onStop}>
              <IconStop width={13} height={13} />
            </button>
          ) : (
            <button
              className="btn-primary rounded p-1.5 disabled:opacity-40"
              title="发送"
              disabled={!value.trim() || !ready}
              onClick={submit}
            >
              <IconSend width={13} height={13} />
            </button>
          )}
        </div>
      </div>
    </div>
  )
}

function MessageView({ message }: { message: Message }) {
  if (message.role === 'user') {
    return (
      <div className="flex justify-end">
        <div className="max-w-[85%] rounded-lg bg-brandSoft px-3.5 py-2 text-[13px] text-ink">
          {typeof message.content === 'string' ? message.content : null}
        </div>
      </div>
    )
  }

  const blocks = useMemo(() => (typeof message.content === 'string' ? null : pairTools(message.content)), [message])

  if (!blocks) {
    return <div className="text-[13px] text-ink">{message.content as string}</div>
  }

  return (
    <div className="space-y-2">
      {blocks.map((b, i) => {
        if (b.kind === 'text') {
          return (
            <p key={i} className="text-[13px] leading-[1.6] text-ink">
              {b.text}
            </p>
          )
        }
        if (b.kind === 'thinking') {
          return <ThinkingBlock key={i} text={b.text} running={false} />
        }
        return <ToolCallCard key={b.call.id} call={b.call} result={b.result} />
      })}
    </div>
  )
}

type RenderBlock =
  | { kind: 'text'; text: string }
  | { kind: 'thinking'; text: string }
  | { kind: 'tool'; call: ToolUse; result?: ToolResult }

/** tool_use 与后续 tool_result 按 id 配对（契约要求两遍重建；此处单消息内配对即可）。 */
function pairTools(blocks: ContentBlock[]): RenderBlock[] {
  const results = new Map<string, ToolResult>()
  for (const b of blocks) if (b.type === 'tool_result') results.set(b.tool_use_id, b)
  const out: RenderBlock[] = []
  for (const b of blocks) {
    if (b.type === 'text') out.push({ kind: 'text', text: b.text })
    else if (b.type === 'tool_use') out.push({ kind: 'tool', call: b, result: results.get(b.id) })
    else if (b.type === 'thinking') out.push({ kind: 'thinking', text: b.thinking })
  }
  return out
}
