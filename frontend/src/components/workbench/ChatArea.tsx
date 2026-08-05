// 中间对话流：空态欢迎区 / 消息流 + 失败横幅 + 流式渲染区 + 底部输入框。
// 发送走 store 流式 buffer（connectSSE 由 WorkbenchPage 接线）；离线时输入框禁用。
import { useEffect, useRef, useState } from 'react'
import type { ContentBlock, Message, Plan, SessionRun } from '../../types'
import type { StreamBuffer } from '../../store'
import { useApp } from '../../App'
import { planStatusLabel } from '../../api/planExecution'
import { sanitizeAssistantDisplayText } from '../../api/assistantProtocol'
import AgentMarkdown from './AgentMarkdown'
import ToolCallCard from './ToolCallCard'
import { IconChevronRight, IconSend, IconStop } from '../icons'

interface Props {
  messages: Message[]
  /** 失败会话才显示 Agent Failed / interrupted 横幅 */
  failed?: boolean
  /** 当前会话的流式 buffer（无则为 undefined） */
  stream?: StreamBuffer
  plan: Plan | null
  awaitingPlan: boolean
  canExecutePlan: boolean
  approvingPlan: boolean
  planError?: string | null
  planMode: boolean
  onPlanModeChange: (enabled: boolean) => void
  onApprovePlan: () => void
  onExecutePlan: () => void
  onSend: (text: string) => void
  onStop: () => void
}

type ToolUse = Extract<ContentBlock, { type: 'tool_use' }>
type ToolResult = Extract<ContentBlock, { type: 'tool_result' }>

export default function ChatArea({
  messages,
  failed,
  stream,
  plan,
  awaitingPlan,
  canExecutePlan,
  approvingPlan,
  planError,
  planMode,
  onPlanModeChange,
  onApprovePlan,
  onExecutePlan,
  onSend,
  onStop,
}: Props) {
  const running = stream?.running ?? false
  const stopping = stream?.stopping ?? false
  const hasPersistedFailure = hasPersistedRunFailure(messages)
  const scrollRef = useRef<HTMLDivElement>(null)
  const bottomRef = useRef<HTMLDivElement>(null)
  const followTailRef = useRef(true)

  useEffect(() => {
    if (!followTailRef.current) return
    const frame = window.requestAnimationFrame(() => {
      bottomRef.current?.scrollIntoView({ block: 'end' })
    })
    return () => window.cancelAnimationFrame(frame)
  }, [messages.length, stream?.thinking.length, stream?.text.length, stream?.toolCalls.length])

  if (messages.length === 0 && !stream && !awaitingPlan) {
    // 新会话空态：居中欢迎区 + 大输入框
    return (
      <div className="flex min-w-0 flex-1 flex-col items-center justify-center px-6">
        <h1 className="text-[20px] font-semibold text-ink">有什么可以帮你？</h1>
        <div className="mt-6 w-full max-w-xl">
          <Composer
            large
            running={running}
            stopping={stopping}
            planMode={planMode}
            onPlanModeChange={onPlanModeChange}
            onSend={onSend}
            onStop={onStop}
          />
          <BackendHint />
        </div>
        <p className="mt-3 text-[12px] text-ink3">可开启 Plan，让 Agent 先提交研究计划供你批准。</p>
      </div>
    )
  }

  return (
    <div className="flex min-w-0 flex-1 flex-col">
      {/* 消息流 */}
      <div
        ref={scrollRef}
        className="min-h-0 flex-1 overflow-y-auto"
        onScroll={() => {
          const element = scrollRef.current
          if (!element) return
          followTailRef.current = element.scrollHeight - element.scrollTop - element.clientHeight < 96
        }}
      >
        <div className="mx-auto max-w-2xl space-y-4 px-6 py-6">
          {messages.map((m, i) => (
            <MessageView
              key={`${m.source_run_id ?? 'live'}:${m.source_seq ?? i}`}
              message={m}
            />
          ))}

          {/* 当前 iteration；已完成的迭代已按真实顺序提交到 messages。 */}
          {stream?.running && (
            <div className="space-y-2">
              {stream.thinking && <ThinkingBlock text={stream.thinking} running={stream.running} />}
              {stream.text && <AgentMarkdown content={stream.text} />}
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
              {stream.running && !stream.thinking && !stream.text && stream.toolCalls.length === 0 && (
                <p className="text-[12px] text-ink3">{stream.stopping ? '停止中…' : '思考中…'}</p>
              )}
            </div>
          )}

          {plan && (
            <PlanPanel
              plan={plan}
              awaitingApproval={awaitingPlan}
              executionReady={canExecutePlan}
              running={running}
              approving={approvingPlan}
              error={planError}
              onApprove={onApprovePlan}
              onExecute={onExecutePlan}
            />
          )}

          {failed && !hasPersistedFailure && !stream?.running && (
            <ErrorBanner message="该会话执行失败。请检查后端日志或修改输入后重新发送。" />
          )}
          <div ref={bottomRef} aria-hidden="true" />
        </div>
      </div>

      {/* 底部输入框 */}
      <div className="shrink-0 border-t border-border px-6 py-3">
        <div className="mx-auto max-w-2xl">
          <Composer
            running={running}
            stopping={stopping}
            planMode={planMode}
            onPlanModeChange={onPlanModeChange}
            onSend={onSend}
            onStop={onStop}
          />
          <BackendHint />
        </div>
      </div>
    </div>
  )
}

/** Agent Failed 红条（流式或持久化失败态共用）。 */
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

function PlanPanel({
  plan,
  awaitingApproval,
  executionReady,
  running,
  approving,
  error,
  onApprove,
  onExecute,
}: {
  plan: Plan
  awaitingApproval: boolean
  executionReady: boolean
  running: boolean
  approving: boolean
  error?: string | null
  onApprove: () => void
  onExecute: () => void
}) {
  return (
    <section className="rounded-md border border-brand/40 bg-brandSoft p-3" aria-label="Research plan">
      <div className="flex items-center justify-between gap-3">
        <h2 className="text-[13px] font-medium text-ink">研究计划</h2>
        <span className="text-[11px] text-brand">
          {planStatusLabel(plan, awaitingApproval, running)}
        </span>
      </div>
      <ol className="mt-2 space-y-1.5">
        {plan.steps.map((step, index) => (
          <li key={`${index}:${step.title}`} className="flex items-start gap-2 text-[12px] text-ink2">
            <span
              className={`mt-1 h-1.5 w-1.5 shrink-0 rounded-full ${
                step.status === 'failed'
                  ? 'bg-danger'
                  : step.status === 'done'
                    ? 'bg-success'
                    : step.status === 'running'
                      ? 'bg-brand'
                      : 'bg-ink3'
              }`}
            />
            <span className="min-w-0 flex-1 whitespace-pre-wrap">{step.title}</span>
            <span className="shrink-0 text-[10px] text-ink3">{step.status}</span>
          </li>
        ))}
      </ol>
      {awaitingApproval && !plan.approved && (
        <div className="mt-3 border-t border-brand/20 pt-3">
          <p className="text-[11px] text-ink2">批准后，Agent 会按此计划立即开始执行。</p>
          <button className="btn-primary mt-2" disabled={approving} onClick={onApprove}>
            {approving ? '批准中…' : '批准并执行'}
          </button>
        </div>
      )}
      {executionReady && (
        <div className="mt-3 border-t border-brand/20 pt-3">
          <p className="text-[11px] text-ink2">计划已安全保存。若上次启动失败，可从这里继续。</p>
          <button className="btn-primary mt-2" onClick={onExecute}>
            执行计划/重试
          </button>
        </div>
      )}
      {error && <p className="mt-2 text-[11px] text-danger">{error}</p>}
    </section>
  )
}

/** thinking 增量：可折叠块，默认收起。 */
export function ThinkingBlock({ text, running }: { text: string; running: boolean }) {
  const [open, setOpen] = useState(false)
  const displayText = sanitizeAssistantDisplayText(text)
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
          {displayText}
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
  stopping: boolean
  planMode: boolean
  onPlanModeChange: (enabled: boolean) => void
  onSend: (text: string) => void
  onStop: () => void
}

function Composer({ large, running, stopping, planMode, onPlanModeChange, onSend, onStop }: ComposerProps) {
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
          ready ? '描述你的科研问题、数据和期望产物…' : '后端未连接…'
        }
        className="w-full resize-none bg-transparent px-3 pt-2.5 text-[13px] outline-none placeholder:text-ink3 disabled:opacity-50"
      />
      <div className="flex items-center gap-1 px-2 pb-2">
        <div className="ml-auto flex items-center gap-2">
          <button
            type="button"
            className={`rounded px-2 py-1 text-[12px] ${
              planMode ? 'bg-brandSoft font-medium text-brand' : 'btn-ghost'
            }`}
            aria-pressed={planMode}
            disabled={running}
            title="先生成研究计划，批准后再执行"
            onClick={() => onPlanModeChange(!planMode)}
          >
            {planMode ? 'Plan on' : 'Plan off'}
          </button>
          {running ? (
            <button
              className="btn-outline rounded p-1.5 disabled:opacity-40"
              title={stopping ? '正在停止' : '停止'}
              disabled={stopping}
              onClick={onStop}
            >
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
      <div className="space-y-2">
        <div className="flex justify-end">
          <div className="max-w-[85%] whitespace-pre-wrap break-words rounded-lg bg-brandSoft px-3.5 py-2 text-[13px] text-ink">
            {typeof message.content === 'string' ? message.content : null}
          </div>
        </div>
        {message.run && <RunFooter run={message.run} />}
      </div>
    )
  }

  const blocks = typeof message.content === 'string' ? null : pairTools(message.content)

  if (!blocks) {
    return (
      <div>
        <AgentMarkdown content={message.content as string} />
        {message.run ? <RunFooter run={message.run} /> : message.usage && <MessageUsage usage={message.usage} />}
      </div>
    )
  }

  return (
    <div className="space-y-2">
      {blocks.map((b, i) => {
        if (b.kind === 'text') {
          return <AgentMarkdown key={i} content={b.text} />
        }
        if (b.kind === 'thinking') {
          return <ThinkingBlock key={i} text={b.text} running={false} />
        }
        return <ToolCallCard key={b.call.id} call={b.call} result={b.result} />
      })}
      {message.run ? <RunFooter run={message.run} /> : message.usage && <MessageUsage usage={message.usage} />}
    </div>
  )
}

function MessageUsage({ usage }: { usage: NonNullable<Message['usage']> }) {
  return (
    <p className="mt-1 text-[10px] text-ink3">
      tokens: {usage.input_tokens} in / {usage.output_tokens} out
    </p>
  )
}

export function RunFooter({ run }: { run: SessionRun }) {
  if (run.kind === 'max_iters') {
    const detail = run.error ? `\n详细信息：${run.error}` : ''
    return (
      <div className="space-y-1.5">
        <ErrorBanner message={`执行预算已耗尽，以上输出可能不完整。${detail}`} />
        <RunMetrics run={run} />
      </div>
    )
  }
  if (run.status === 'failed' || run.kind === 'error' || run.error) {
    return (
      <div className="space-y-1.5">
        <ErrorBanner message={run.error ?? 'Agent 执行失败，后端未返回详细原因。'} />
        <RunMetrics run={run} />
      </div>
    )
  }
  if (run.kind === 'awaiting' && run.pending_ask) {
    return (
      <div className="space-y-1.5">
        <AskUserPanel ask={run.pending_ask} />
        <RunMetrics run={run} />
      </div>
    )
  }
  return <RunMetrics run={run} />
}

function RunMetrics({ run }: { run: SessionRun }) {
  const status =
    run.kind === 'max_iters'
      ? '达到迭代上限'
      : run.status === 'failed' || run.kind === 'error' || run.error
        ? '执行失败'
        : run.kind === 'cancelled'
          ? '已停止'
          : run.kind === 'awaiting'
            ? run.awaiting === 'plan_approval'
              ? '等待计划批准'
              : run.awaiting === 'plan_execution'
                ? '等待执行计划'
                : '等待你的回复'
            : '已完成'
  return (
    <p className="mt-1 text-[10px] text-ink3" data-run-id={run.run_id} data-run-kind={run.kind}>
      {status} · tokens: {run.usage.input_tokens} in / {run.usage.output_tokens} out ·{' '}
      {run.iterations} iteration{run.iterations === 1 ? '' : 's'}
    </p>
  )
}

export function hasPersistedRunFailure(messages: Message[]): boolean {
  return messages.some(
    (message) =>
      message.run?.status === 'failed' ||
      message.run?.kind === 'error' ||
      message.run?.kind === 'max_iters' ||
      !!message.run?.error,
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
