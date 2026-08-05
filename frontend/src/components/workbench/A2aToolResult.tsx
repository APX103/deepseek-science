import { useState } from 'react'
import { openUrl } from '@tauri-apps/plugin-opener'
import {
  a2aTaskInterruption,
  arrayField,
  isJsonRecord,
  prettyJson,
  recordField,
  semanticNodesForPayload,
  textField,
  type A2aResponseFrame,
  type A2aSemanticNode,
  type A2aToolResultEnvelope,
  type JsonRecord,
} from '../../api/a2aToolResult'
import AgentMarkdown, { safeMarkdownUrl } from './AgentMarkdown'

interface Props {
  result: A2aToolResultEnvelope
}

function scalarLabel(value: unknown): string {
  if (typeof value === 'string') return value
  if (typeof value === 'number' || typeof value === 'boolean') return String(value)
  return ''
}

function readableDate(value: string): string {
  if (!value) return ''
  const parsed = new Date(value)
  return Number.isNaN(parsed.valueOf()) ? value : parsed.toLocaleString()
}

async function openExternalUrl(href: string) {
  try {
    await openUrl(href)
  } catch {
    if (typeof window !== 'undefined' && !('__TAURI_INTERNALS__' in window)) {
      window.open(href, '_blank', 'noopener,noreferrer')
    }
  }
}

function SafeRemoteLink({ value }: { value: string }) {
  const href = safeMarkdownUrl(value)
  if (!href || href.startsWith('#')) {
    return <code className="break-all text-[11px] text-ink2">{value}</code>
  }
  return (
    <button
      type="button"
      role="link"
      data-external-url={href}
      title="在系统浏览器中打开；不会由 Agent 结果卡自动抓取"
      className="cursor-pointer break-all border-0 bg-transparent p-0 text-left text-[11px] text-brand underline decoration-brand/30 underline-offset-2"
      onClick={() => void openExternalUrl(href)}
    >
      {value}
    </button>
  )
}

function RawJson({ label, value, open = false }: { label: string; value: unknown; open?: boolean }) {
  const [expanded, setExpanded] = useState(open)
  return (
    <details
      className="rounded-md border border-border bg-bg"
      open={expanded}
      onToggle={(event) => setExpanded(event.currentTarget.open)}
    >
      <summary className="cursor-pointer select-none px-2.5 py-2 text-[10px] font-medium text-ink3 hover:text-ink2">
        {label}
      </summary>
      {expanded && (
        <pre className="max-h-96 overflow-auto whitespace-pre-wrap break-words border-t border-border p-2.5 font-mono text-[10px] leading-[1.5] text-ink2">
          {prettyJson(value)}
        </pre>
      )}
    </details>
  )
}

function MetaLine({ children }: { children: React.ReactNode }) {
  return <span className="rounded bg-surface px-1.5 py-0.5 text-[10px] text-ink3">{children}</span>
}

function PartView({ part, index }: { part: unknown; index: number }) {
  if (!isJsonRecord(part)) {
    const displayValue = part === null
      ? '空 Part（原始值可在完整 JSON 中查看）'
      : prettyJson(part)
    return (
      <div className="rounded-md border border-border bg-bg p-2" data-a2a-part-kind="scalar">
        <div className="mb-1 text-[10px] font-medium text-ink3">Part {index + 1}</div>
        <pre className="whitespace-pre-wrap break-words text-[11px] text-ink2">{displayValue}</pre>
      </div>
    )
  }

  const kind = textField(part, 'kind', 'type').toLowerCase()
  const filename = textField(part, 'filename', 'name')
  const mediaType = textField(part, 'mediaType', 'media_type', 'mimeType', 'mime_type')
  const hasTextField = 'text' in part
  const text = typeof part.text === 'string' ? part.text : null
  const url = textField(part, 'url', 'uri')
  const file = recordField(part, 'file')
  const fileUrl = textField(file, 'uri', 'url', 'fileWithUri', 'file_with_uri')
  const fileRaw = file && (
    'bytes' in file
      ? file.bytes
      : 'raw' in file
        ? file.raw
        : 'fileWithBytes' in file
          ? file.fileWithBytes
          : file.file_with_bytes
  )
  const hasFileRawField = Boolean(
    file && ('bytes' in file || 'raw' in file || 'fileWithBytes' in file || 'file_with_bytes' in file),
  )
  const hasDataField = 'data' in part
  const hasRawField = 'raw' in part
  const hasData = hasDataField && part.data !== null && part.data !== undefined
  const hasRaw = hasRawField && part.raw !== null && part.raw !== undefined
  const hasFileRaw = hasFileRawField && fileRaw !== null && fileRaw !== undefined
  const hasKnownContentField = hasTextField || hasDataField || hasRawField || Boolean(url) || Boolean(file)
  const hasRenderableContent = text !== null || hasData || hasRaw || Boolean(url) || Boolean(fileUrl) || hasFileRaw

  return (
    <div
      className="space-y-2 rounded-md border border-border bg-bg p-2.5"
      data-a2a-part-kind={kind || (text !== null ? 'text' : hasDataField ? 'data' : hasRawField ? 'raw' : 'unknown')}
    >
      <div className="flex flex-wrap items-center gap-1.5">
        <span className="text-[10px] font-semibold uppercase tracking-[0.08em] text-ink3">Part {index + 1}</span>
        {kind && <MetaLine>{kind}</MetaLine>}
        {filename && <MetaLine>{filename}</MetaLine>}
        {mediaType && <MetaLine>{mediaType}</MetaLine>}
        {file && textField(file, 'name') && <MetaLine>{textField(file, 'name')}</MetaLine>}
        {file && textField(file, 'mimeType', 'mime_type', 'mediaType', 'media_type') && (
          <MetaLine>{textField(file, 'mimeType', 'mime_type', 'mediaType', 'media_type')}</MetaLine>
        )}
      </div>

      {text !== null && <AgentMarkdown content={text} />}
      {hasData && (
        <pre className="max-h-72 overflow-auto whitespace-pre-wrap break-words rounded border border-border bg-surface/50 p-2 font-mono text-[10px] leading-[1.5] text-ink2">
          {prettyJson(part.data)}
        </pre>
      )}
      {hasRaw && (
        <pre className="max-h-72 overflow-auto whitespace-pre-wrap break-all rounded border border-border bg-surface/50 p-2 font-mono text-[10px] leading-[1.5] text-ink2">
          {scalarLabel(part.raw) || prettyJson(part.raw)}
        </pre>
      )}
      {url && (
        <div className="space-y-1">
          <SafeRemoteLink value={url} />
          <div className="text-[10px] text-ink3">远程 URL 仅展示；客户端不会自动获取内容。</div>
        </div>
      )}
      {fileUrl && (
        <div className="space-y-1">
          <SafeRemoteLink value={fileUrl} />
          <div className="text-[10px] text-ink3">远程文件仅展示；客户端不会自动下载。</div>
        </div>
      )}
      {hasFileRaw && (
        <pre className="max-h-72 overflow-auto whitespace-pre-wrap break-all rounded border border-border bg-surface/50 p-2 font-mono text-[10px] leading-[1.5] text-ink2">
          {scalarLabel(fileRaw) || prettyJson(fileRaw)}
        </pre>
      )}

      {!hasRenderableContent && hasKnownContentField && (
        <div className="rounded border border-border bg-surface/40 px-2 py-1.5 text-[10px] text-ink3" data-a2a-empty-part>
          此 Part 的语义值为空；原始字段仍保留在完整 JSON 中。
        </div>
      )}
      {!hasRenderableContent && !hasKnownContentField && (
        <pre className="whitespace-pre-wrap break-words font-mono text-[10px] text-ink2">{prettyJson(part)}</pre>
      )}
      <RawJson label="查看完整 Part JSON" value={part} />
    </div>
  )
}

function MessageView({ message, label = 'Message' }: { message: JsonRecord; label?: string }) {
  // v1 and v0.3 JSON-RPC use `parts`; the normative v0.3 HTTP+JSON ProtoJSON
  // representation uses `content`. Both remain visible with the same Part renderer.
  const parts = arrayField(message, 'parts', 'content')
  const role = textField(message, 'role')
  const messageId = textField(message, 'messageId', 'message_id', 'id')
  const taskId = textField(message, 'taskId', 'task_id')
  const contextId = textField(message, 'contextId', 'context_id')
  return (
    <div className="space-y-2 rounded-md border border-border bg-surface/50 p-2.5" data-a2a-message-id={messageId || undefined}>
      <div className="flex flex-wrap items-center gap-1.5">
        <span className="text-[11px] font-medium text-ink">{label}</span>
        {role && <MetaLine>{role}</MetaLine>}
        {messageId && <MetaLine>id {messageId}</MetaLine>}
        {taskId && <MetaLine>task {taskId}</MetaLine>}
        {contextId && <MetaLine>context {contextId}</MetaLine>}
      </div>
      {parts.length > 0 ? (
        <div className="space-y-2">
          {parts.map((part, index) => <PartView key={index} part={part} index={index} />)}
        </div>
      ) : (
        <div className="text-[10px] text-ink3">该 Message 没有 parts/content。</div>
      )}
      <RawJson label="查看完整 Message JSON" value={message} />
    </div>
  )
}

function ArtifactView({ value, label = 'Artifact' }: { value: JsonRecord; label?: string }) {
  const artifact = recordField(value, 'artifact') ?? value
  const parts = arrayField(artifact, 'parts')
  const artifactId = textField(artifact, 'artifactId', 'artifact_id', 'id')
  const name = textField(artifact, 'name')
  const description = textField(artifact, 'description')
  const append = typeof value.append === 'boolean' ? value.append : null
  const lastChunk = typeof value.lastChunk === 'boolean'
    ? value.lastChunk
    : typeof value.last_chunk === 'boolean' ? value.last_chunk : null
  return (
    <div className="space-y-2 rounded-md border border-brand/20 bg-brandSoft/30 p-2.5" data-a2a-artifact-id={artifactId || undefined}>
      <div className="flex flex-wrap items-center gap-1.5">
        <span className="text-[11px] font-medium text-ink">{name || label}</span>
        {artifactId && <MetaLine>id {artifactId}</MetaLine>}
        {append !== null && <MetaLine>append {String(append)}</MetaLine>}
        {lastChunk !== null && <MetaLine>lastChunk {String(lastChunk)}</MetaLine>}
      </div>
      {description && <p className="whitespace-pre-wrap text-[11px] text-ink2">{description}</p>}
      {parts.length > 0 ? (
        <div className="space-y-2">
          {parts.map((part, index) => <PartView key={index} part={part} index={index} />)}
        </div>
      ) : (
        <div className="text-[10px] text-ink3">该 Artifact 没有 parts。</div>
      )}
      <RawJson label="查看完整 Artifact JSON" value={value} />
    </div>
  )
}

function StatusView({ value }: { value: JsonRecord }) {
  const status = recordField(value, 'status') ?? value
  const state = textField(status, 'state')
  const message = recordField(status, 'message')
  return (
    <div className="space-y-2 rounded-md border border-border bg-surface/50 p-2.5" data-a2a-task-state={state || undefined}>
      <div className="flex flex-wrap items-center gap-1.5 text-[11px] font-medium text-ink">
        状态更新 {state && <MetaLine>{state}</MetaLine>}
      </div>
      {message && <MessageView message={message} label="状态消息" />}
      <RawJson label="查看完整状态 JSON" value={value} />
    </div>
  )
}

function TaskView({ task }: { task: JsonRecord }) {
  const taskId = textField(task, 'id', 'taskId', 'task_id')
  const contextId = textField(task, 'contextId', 'context_id')
  const status = recordField(task, 'status')
  const state = textField(status, 'state')
  const statusMessage = recordField(status, 'message')
  const history = arrayField(task, 'history', 'messages')
  const artifacts = arrayField(task, 'artifacts')
  return (
    <div className="space-y-2 rounded-md border border-borderStrong bg-bg p-2.5" data-a2a-task-id={taskId || undefined}>
      <div className="flex flex-wrap items-center gap-1.5">
        <span className="text-[11px] font-medium text-ink">Task</span>
        {taskId && <MetaLine>id {taskId}</MetaLine>}
        {contextId && <MetaLine>context {contextId}</MetaLine>}
        {state && <MetaLine>{state}</MetaLine>}
      </div>
      {statusMessage && <MessageView message={statusMessage} label="状态消息" />}
      {history.map((item, index) => isJsonRecord(item)
        ? <MessageView key={index} message={item} label={`History ${index + 1}`} />
        : <RawJson key={index} label={`History ${index + 1}`} value={item} />)}
      {artifacts.map((item, index) => isJsonRecord(item)
        ? <ArtifactView key={index} value={item} label={`Artifact ${index + 1}`} />
        : <RawJson key={index} label={`Artifact ${index + 1}`} value={item} />)}
      <RawJson label="查看完整 Task JSON" value={task} />
    </div>
  )
}

function SemanticNodeView({ node }: { node: A2aSemanticNode }) {
  switch (node.kind) {
    case 'message':
      return <MessageView message={node.value} />
    case 'task':
      return <TaskView task={node.value} />
    case 'status':
      return <StatusView value={node.value} />
    case 'artifact':
      return <ArtifactView value={node.value} label="Artifact update" />
  }
}

function semanticNodeSummary(node: A2aSemanticNode): string {
  switch (node.kind) {
    case 'message': {
      const role = textField(node.value, 'role')
      const id = textField(node.value, 'messageId', 'message_id', 'id')
      return ['Message', role, id].filter(Boolean).join(' · ')
    }
    case 'task': {
      const state = textField(recordField(node.value, 'status'), 'state')
      const id = textField(node.value, 'id', 'taskId', 'task_id')
      return ['Task', state, id].filter(Boolean).join(' · ')
    }
    case 'status': {
      const status = recordField(node.value, 'status') ?? node.value
      const state = textField(status, 'state')
      return ['状态更新', state].filter(Boolean).join(' · ')
    }
    case 'artifact': {
      const artifact = recordField(node.value, 'artifact') ?? node.value
      const name = textField(artifact, 'name')
      const id = textField(artifact, 'artifactId', 'artifact_id', 'id')
      return ['Artifact', name || id].filter(Boolean).join(' · ')
    }
  }
}

function FrameAccordion({
  frame,
  index,
  frameCount,
}: {
  frame: A2aResponseFrame
  index: number
  frameCount: number
}) {
  const defaultExpanded = index === frameCount - 1
  const [expanded, setExpanded] = useState(defaultExpanded)
  const nodes = semanticNodesForPayload(frame.payload)
  const semanticSummary = nodes.length > 0
    ? nodes.map(semanticNodeSummary).join(' / ')
    : '未识别包装 · 原始响应已保留'

  return (
    <details
      className="rounded-md border border-border bg-bg"
      data-a2a-response-sequence={frame.sequence}
      data-a2a-frame-expanded={expanded ? 'true' : 'false'}
      open={expanded}
      onToggle={(event) => setExpanded(event.currentTarget.open)}
    >
      <summary className="cursor-pointer list-none select-none px-2.5 py-2.5 hover:bg-surface/40 [&::-webkit-details-marker]:hidden">
        <span className="flex flex-wrap items-center justify-between gap-2" data-a2a-frame-summary>
          <span className="flex min-w-0 flex-wrap items-center gap-1.5">
            <span className="flex h-5 min-w-5 items-center justify-center rounded-full bg-brandSoft px-1.5 text-[10px] font-semibold text-brand">
              {frame.sequence}
            </span>
            <span className="font-mono text-[10px] font-medium text-ink2">{frame.operation}</span>
            {typeof frame.http_status === 'number' && <MetaLine>HTTP {frame.http_status}</MetaLine>}
            {typeof frame.binding === 'string' && <MetaLine>{frame.binding}</MetaLine>}
            {typeof frame.protocol_version === 'string' && <MetaLine>{frame.protocol_version}</MetaLine>}
            {typeof frame.wire_bytes === 'number' && <MetaLine>{frame.wire_bytes} bytes</MetaLine>}
            <span className="max-w-full truncate text-[10px] text-ink3" title={semanticSummary}>
              {semanticSummary}
            </span>
          </span>
          <span className="flex shrink-0 items-center gap-1.5 text-[10px] text-ink3">
            {defaultExpanded && <span className="rounded bg-brandSoft px-1.5 py-0.5 text-brand">末帧</span>}
            <span>{readableDate(frame.received_at)}</span>
            <span aria-hidden="true">{expanded ? '收起' : '展开'}</span>
          </span>
        </span>
      </summary>
      {expanded && (
        <div className="space-y-2 border-t border-border p-2.5" data-a2a-frame-content>
          {nodes.length > 0 ? (
            <div className="space-y-2">
              {nodes.map((node, nodeIndex) => <SemanticNodeView key={nodeIndex} node={node} />)}
            </div>
          ) : (
            <div className="rounded border border-border bg-surface/40 px-2 py-1.5 text-[10px] text-ink3">
              此帧没有可识别的 Message/Task/Artifact 包装；完整响应仍保留在下方。
            </div>
          )}
          <RawJson label={`查看第 ${frame.sequence} 帧完整响应 JSON`} value={frame.payload} />
        </div>
      )}
    </details>
  )
}

export default function A2aToolResult({ result }: Props) {
  const agentName = textField(result.agent, 'display_name', 'name', 'id', 'config_id') || '远程 Agent'
  const endpoint = textField(result.agent, 'configured_endpoint', 'endpoint')
  const cardSummary = recordField(result.card, 'summary') ?? result.card ?? {}
  const cardName = textField(cardSummary, 'name')
  const cardDescription = textField(cardSummary, 'description')
  const cardVersion = textField(cardSummary, 'agent_version', 'version')
  const protocolVersion = textField(cardSummary, 'protocol_version', 'protocolVersion')
  const cardSkills = arrayField(cardSummary, 'skills')
  const fetchedAt = textField(result.card, 'fetched_at', 'fetchedAt')
  const cardHash = textField(result.card, 'sha256', 'hash')
  const cardRaw = result.card && 'raw' in result.card ? result.card.raw : result.card
  const selectedInterface = recordField(result, 'interface', 'selected_interface')
    ?? recordField(result.card, 'interface', 'selected_interface')
  const terminalKind = textField(result.terminal, 'kind', 'outcome')
  const terminalState = textField(result.terminal, 'state')
  const taskId = textField(result.terminal, 'task_id', 'taskId')
  const contextId = textField(result.terminal, 'context_id', 'contextId')
  const terminalError = textField(result.terminal, 'error')
  const terminalSuccess = typeof result.terminal.success === 'boolean' ? result.terminal.success : null
  const requestAction = textField(result.request, 'action') || 'send'
  const taskPending = terminalKind === 'task_pending'
  const taskInterruption = a2aTaskInterruption(result)
  const taskInterrupted = taskInterruption !== null
  const interruptionLabel = taskInterruption === 'input_required'
    ? '等待输入'
    : taskInterruption === 'auth_required' ? '等待认证' : '等待继续'

  return (
    <div className="space-y-3" data-a2a-tool-result={result.schema} data-a2a-response-count={result.responses.length}>
      <div className="rounded-md border border-brand/20 bg-brandSoft/40 p-3">
        <div className="flex flex-wrap items-start justify-between gap-2">
          <div className="min-w-0">
            <div className="text-[12px] font-semibold text-ink">{agentName}</div>
            {endpoint && <div className="mt-0.5 break-all font-mono text-[10px] text-ink3">{endpoint}</div>}
          </div>
          <div className="flex flex-wrap justify-end gap-1.5">
            {terminalKind && (
              <MetaLine>{taskPending ? '任务已提交' : taskInterrupted ? '任务已暂停' : terminalKind}</MetaLine>
            )}
            {terminalState && <MetaLine>{terminalState}</MetaLine>}
            {terminalSuccess !== null && (
              <MetaLine>
                {taskPending ? '等待远程完成' : taskInterrupted ? interruptionLabel : terminalSuccess ? '成功' : '未成功'}
              </MetaLine>
            )}
          </div>
        </div>
        <div className="mt-2 flex flex-wrap gap-1.5">
          {cardName && <MetaLine>Card: {cardName}</MetaLine>}
          {cardVersion && <MetaLine>Agent {cardVersion}</MetaLine>}
          {protocolVersion && <MetaLine>A2A {protocolVersion}</MetaLine>}
          {fetchedAt && <MetaLine>刷新 {readableDate(fetchedAt)}</MetaLine>}
          {cardHash && <MetaLine>sha256 {cardHash.slice(0, 12)}</MetaLine>}
          <MetaLine>action {requestAction}</MetaLine>
          {taskId && <MetaLine>task {taskId}</MetaLine>}
          {contextId && <MetaLine>context {contextId}</MetaLine>}
        </div>
        {selectedInterface && (
          <div className="mt-2 rounded border border-border bg-bg/70 px-2 py-1.5 font-mono text-[10px] text-ink2" data-a2a-interface>
            {textField(selectedInterface, 'protocol_binding', 'protocolBinding', 'binding') || 'A2A'}
            {textField(selectedInterface, 'protocol_version', 'protocolVersion')
              ? ` / ${textField(selectedInterface, 'protocol_version', 'protocolVersion')}`
              : ''}
            {textField(selectedInterface, 'url') ? ` · ${textField(selectedInterface, 'url')}` : ''}
          </div>
        )}
        {cardDescription && <p className="mt-2 whitespace-pre-wrap text-[11px] text-ink2">{cardDescription}</p>}
        {taskPending && taskId && (
          <div className="mt-2 rounded border border-amber-500/30 bg-amber-500/5 px-2.5 py-2 text-[11px] text-amber-700" data-a2a-task-pending>
            远程 Task 仍在运行；本次 handle 已进入会话记录。后续使用 <code>get_task</code> 和上方 task id 查询，不会重复发送原任务。
          </div>
        )}
        {taskInterrupted && (
          <div className="mt-2 rounded border border-amber-500/30 bg-amber-500/5 px-2.5 py-2 text-[11px] text-amber-700" data-a2a-task-interrupted={taskInterruption}>
            远程 Task 已暂停，尚未完成也未失败。
            {taskInterruption === 'auth_required'
              ? ' 完成 Agent 要求的认证后，'
              : taskInterruption === 'input_required' ? ' 提供 Agent 要求的信息后，' : ' 准备好继续后，'}
            使用同一 A2A 工具发送后续消息（<code>action=send</code>），并携带上方 task id
            {contextId ? ' 与 context id' : ''} 续接原 Task；只查看状态时使用 <code>get_task</code>。
          </div>
        )}
        {cardSkills.length > 0 && (
          <div className="mt-2 flex flex-wrap gap-1" data-a2a-card-skills>
            {cardSkills.map((skill, index) => (
              <MetaLine key={index}>
                {isJsonRecord(skill) ? textField(skill, 'name', 'id') || `skill ${index + 1}` : scalarLabel(skill)}
              </MetaLine>
            ))}
          </div>
        )}
      </div>

      {terminalError && !taskInterrupted && (
        <div className="whitespace-pre-wrap rounded-md border border-danger/30 bg-dangerSoft px-3 py-2 text-[11px] text-danger" data-a2a-terminal-error>
          {terminalError}
        </div>
      )}

      {result.warnings.length > 0 && (
        <div className="rounded-md border border-amber-500/30 bg-amber-500/5 px-3 py-2 text-[11px] text-amber-700" data-a2a-warnings>
          <div className="mb-1 font-medium">警告</div>
          <ul className="list-disc space-y-1 pl-4">
            {result.warnings.map((warning, index) => (
              <li key={index} className="whitespace-pre-wrap break-words">
                {warning === null || warning === undefined
                  ? '空警告项（原始值保留在 envelope 元数据中）'
                  : typeof warning === 'string' ? warning : prettyJson(warning)}
              </li>
            ))}
          </ul>
        </div>
      )}

      <div className="space-y-2" aria-label="A2A 返回时间线">
        <div className="text-[10px] font-medium uppercase tracking-[0.08em] text-ink3">
          Agent 返回 · {result.responses.length} 帧
        </div>
        {result.responses.map((frame, index) => (
          <FrameAccordion
            key={`${frame.sequence}-${index}`}
            frame={frame}
            index={index}
            frameCount={result.responses.length}
          />
        ))}
        {result.responses.length === 0 && (
          <div className="rounded-md border border-danger/30 bg-dangerSoft px-3 py-2 text-[11px] text-danger">
            本次调用没有接收到可保存的远程响应帧。
          </div>
        )}
      </div>

      <div className="grid gap-2 sm:grid-cols-2">
        <RawJson
          label={result.card ? '查看调用时刷新后的完整 Agent Card' : 'Agent Card 未能刷新'}
          value={cardRaw}
        />
        <RawJson label="查看终态与错误详情" value={result.terminal} />
      </div>
      <RawJson
        label="查看 A2A envelope 元数据"
        value={{
          schema: result.schema,
          agent: result.agent,
          request: result.request,
          responses: result.responses.map(({ payload: _payload, ...metadata }) => metadata),
          warnings: result.warnings,
        }}
      />
    </div>
  )
}
