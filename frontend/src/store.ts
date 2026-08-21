// 轻量前端 store：useSyncExternalStore 驱动，不引外部状态库。
// projects/sessions 从真实后端 API 加载；messages 按需从后端恢复。
// 流式 buffer（stream）仍是纯内存。
import { useSyncExternalStore } from 'react'
import type {
  Artifact,
  AwaitingKind,
  Bot,
  ContentBlock,
  Message,
  PendingAsk,
  Plan,
  Project,
  RunKind,
  SessionRun,
  SessionState,
  SessionStatus,
  SessionSummary,
  Usage,
} from './types'
import * as api from './api/client'
import { AsyncVersionGuard } from './api/asyncVersionGuard'
import { sanitizeAssistantDisplayText } from './api/assistantProtocol'
import { createRunId } from './api/sessionRun'
import {
  carryThinkingDisclosureMessageMetadata,
  discardThinkingDisclosureBlock,
  reconcileThinkingDisclosureHistory,
  registerThinkingDisclosureBlock,
  registerThinkingDisclosureMessage,
  thinkingDisclosureId,
} from './api/thinkingDisclosure'

interface State {
  projects: Project[]
  bots: Bot[]
  sessions: SessionSummary[]
  messages: Record<string, Message[]>
  /** GET /sessions/{sid} 恢复的 frame / plan / artifacts 等完整状态。 */
  sessionStates: Record<string, SessionState>
  /** 后端是否在线（影响 Home/Workbench 空态提示）。 */
  backendOnline: boolean
}

const EMPTY_MESSAGES: Message[] = []
const sessionLoadGuard = new AsyncVersionGuard()

function emptyState(): State {
  return { projects: [], bots: [], sessions: [], messages: {}, sessionStates: {}, backendOnline: false }
}

let state: State = emptyState()

const listeners = new Set<() => void>()

function setState(next: State) {
  state = next
  listeners.forEach((l) => l())
}

/** 从后端拉取 projects + sessions，回填 store。后端离线则置空 + backendOnline=false。 */
export async function loadFromBackend(): Promise<void> {
  try {
    const [projects, bots, sessions] = await Promise.all([
      api.listProjects(),
      api.listBots(),
      api.listSessions(),
    ])
    const sessionCounts = new Map<string, number>()
    for (const session of sessions) {
      sessionCounts.set(session.project_id, (sessionCounts.get(session.project_id) ?? 0) + 1)
    }
    const projectsWithCounts = projects.map((project) => ({
      ...project,
      session_count: sessionCounts.get(project.id) ?? 0,
    }))
    setState({ ...state, projects: projectsWithCounts, bots, sessions, backendOnline: true })
  } catch {
    setState({ ...state, projects: [], bots: [], sessions: [], backendOnline: false })
  }
}

/** 拉取某会话的消息历史（从 GET /sessions/{sid} 恢复），回填 messages[sid]。 */
export async function loadMessages(sid: string): Promise<boolean> {
  const version = sessionLoadGuard.begin(sid)
  try {
    const s = await api.getSession(sid)
    if (!sessionLoadGuard.isCurrent(sid, version)) return false
    reconcileThinkingDisclosureHistory(s.messages)
    setState({
      ...state,
      messages: { ...state.messages, [sid]: s.messages },
      sessionStates: { ...state.sessionStates, [sid]: s },
      sessions: state.sessions.map((summary) =>
        summary.id === sid ? { ...summary, status: s.status } : summary,
      ),
    })
    return true
  } catch {
    // 后端无此会话或离线：保留空。
    return false
  }
}

/** 标记后端在线态（probeBackend 后由 App 设置）。 */
export function setBackendOnline(online: boolean) {
  if (state.backendOnline !== online) setState({ ...state, backendOnline: online })
}

function subscribe(l: () => void) {
  listeners.add(l)
  return () => {
    listeners.delete(l)
  }
}

// ---------- hooks ----------
export function useProjects(): Project[] {
  return useSyncExternalStore(subscribe, () => state.projects)
}

export function useBots(): Bot[] {
  return useSyncExternalStore(subscribe, () => state.bots)
}

export function useSessions(): SessionSummary[] {
  return useSyncExternalStore(subscribe, () => state.sessions)
}

export function useBackendOnline(): boolean {
  return useSyncExternalStore(subscribe, () => state.backendOnline)
}

export function useSession(sid: string): SessionSummary | undefined {
  return useSyncExternalStore(subscribe, () => state.sessions.find((s) => s.id === sid))
}

export function useMessages(sid: string): Message[] {
  return useSyncExternalStore(subscribe, () => state.messages[sid] ?? EMPTY_MESSAGES)
}

export function useSessionState(sid: string): SessionState | undefined {
  return useSyncExternalStore(subscribe, () => state.sessionStates[sid])
}

/** Imperative session snapshot for run coordinators that outlive a page render. */
export function getSessionStateSnapshot(sid: string): SessionState | undefined {
  return state.sessionStates[sid]
}

// ---------- projects ----------
/** 新项目入列表（NewProjectModal 经 api/client 建好后回传）。 */
export function addProject(p: Project) {
  setState({ ...state, projects: [p, ...state.projects] })
}

export function updateProject(pid: string, patch: Partial<Project>) {
  setState({
    ...state,
    projects: state.projects.map((p) =>
      p.id === pid ? { ...p, ...patch, updated_at: new Date().toISOString() } : p,
    ),
  })
}

/** 后端操作成功后，同步移除本地项目状态。 */
export function removeProject(pid: string) {
  setState({ ...state, projects: state.projects.filter((p) => p.id !== pid) })
}

// ---------- bots ----------
export function addBot(bot: Bot) {
  setState({ ...state, bots: [bot, ...state.bots] })
}

export function replaceBot(bot: Bot) {
  setState({ ...state, bots: state.bots.map((candidate) => candidate.id === bot.id ? bot : candidate) })
}

export function removeBot(botId: string) {
  setState({ ...state, bots: state.bots.filter((bot) => bot.id !== botId) })
}

// ---------- sessions ----------
/** 用后端已创建的真实 sid 加入本地列表；绝不生成前端伪 sid。 */
export function createSession(projectId: string, opts: { id: string; botId?: string | null }): SessionSummary {
  const now = new Date().toISOString()
  const s: SessionSummary = {
    id: opts.id,
    project_id: projectId,
    title: 'New session',
    status: 'awaiting',
    live: true,
    bot_id: opts.botId ?? null,
    created_at: now,
    updated_at: now,
  }
  setState({
    ...state,
    projects: state.projects.map((project) =>
      project.id === projectId
        ? {
            ...project,
            last_session_id: s.id,
            session_count: project.session_count + 1,
            updated_at: now,
          }
        : project,
    ),
    sessions: [s, ...state.sessions],
    messages: { ...state.messages, [s.id]: [] },
  })
  return s
}

/** 追加一条用户消息；首条消息同时作为会话标题。 */
export function sendUserMessage(sid: string, text: string) {
  const content = text.trim()
  if (!content) return
  // A GET started before this live run must never overwrite its optimistic or
  // streamed transcript when the older response arrives.
  sessionLoadGuard.invalidate(sid)
  setState({
    ...state,
    sessions: state.sessions.map((s) =>
      s.id === sid && s.title === 'New session'
        ? { ...s, title: content.slice(0, 60), updated_at: new Date().toISOString() }
        : s,
    ),
    messages: {
      ...state.messages,
      [sid]: [...(state.messages[sid] ?? []), { role: 'user', content }],
    },
  })
}

// ---------- 流式 buffer（仅内存，刷新丢失；P3 才做恢复）----------
/** 一轮工具调用的实时态（id 配对 use/result）。 */
export interface StreamToolCall {
  id: string
  name: string
  input: Record<string, unknown>
  content?: string
  is_error?: boolean
  /** 是否已收到对应 tool_results。 */
  resolved: boolean
}

export interface StreamBuffer {
  /** Client-generated id that scopes Stop to this exact accepted request. */
  runId: string
  running: boolean
  /** Stop has been requested; input remains locked until backend acknowledgement. */
  stopping: boolean
  stopped: boolean
  thinking: string
  text: string
  usage: Usage | null
  iterations: number
  /** 当前正在呈现的 LLM iteration；进入下一轮时会提交上一段。 */
  currentIteration: number
  /** `draft_reset` 后递增，避免折叠选择泄漏到新的候选草稿。 */
  draftRevision: number
  error: string | null
  /** 本轮 run 累积的工具调用（含结果，按到达顺序）。 */
  toolCalls: StreamToolCall[]
  /** ask_user 挂起的提问（complete.kind=awaiting 时才有）。 */
  pendingAsk: PendingAsk | null
  /** complete.kind（complete 后填充）。 */
  kind: RunKind | null
  /** awaiting 的具体原因：普通用户回复或计划审批。 */
  awaiting: AwaitingKind
  /** plan_update / complete.plan 的最新快照。 */
  plan: Plan | null
  planMode: boolean
  frameId: string
  taskSummary: string
  startedAt: string
  /** 当前 user turn 在 messages 数组中的起点，用于挂接持久 run 终态。 */
  messageStartIndex: number
  /** SSE failure observed while cancellation acknowledgement is in flight. */
  deferredStopError: string | null
}

let streams: Record<string, StreamBuffer> = {}
const aborters = new Map<string, { runId: string; abort: () => void }>()

function activeStream(sid: string, expectedRunId?: string): StreamBuffer | undefined {
  const stream = streams[sid]
  if (!stream?.running) return undefined
  if (expectedRunId !== undefined && stream.runId !== expectedRunId) return undefined
  return stream
}

function deleteStreamAborter(sid: string, runId: string) {
  if (aborters.get(sid)?.runId === runId) aborters.delete(sid)
}

export function useStream(sid: string): StreamBuffer | undefined {
  return useSyncExternalStore(subscribe, () => streams[sid])
}

export function startStream(
  sid: string,
  preserveApprovedPlan = false,
  planMode = false,
): string {
  const runId = createRunId()
  const messageStartIndex = Math.max(0, (state.messages[sid]?.length ?? 1) - 1)
  streams = {
    ...streams,
    [sid]: {
      runId,
      running: true,
      stopping: false,
      stopped: false,
      thinking: '',
      text: '',
      usage: null,
      iterations: 0,
      currentIteration: 0,
      draftRevision: 0,
      error: null,
      toolCalls: [],
      pendingAsk: null,
      kind: null,
      awaiting: null,
      // Only the explicit execute-plan request may carry an approved plan into
      // a new run. Ordinary/fresh-plan prompts supersede it on the backend.
      plan: preserveApprovedPlan ? state.sessionStates[sid]?.plan ?? null : null,
      planMode,
      frameId: state.sessionStates[sid]?.frame_id ?? '',
      taskSummary: '',
      startedAt: new Date().toISOString(),
      messageStartIndex,
      deferredStopError: null,
    },
  }
  setSessionStatus(sid, 'processing')
  listeners.forEach((l) => l())
  return runId
}

export function appendStreamThinking(sid: string, t: string, expectedRunId?: string): boolean {
  const s = activeStream(sid, expectedRunId)
  if (!s) return false
  const thinking = s.thinking + t
  registerThinkingDisclosureBlock(
    s.runId,
    thinkingDisclosureId(s.runId, s.currentIteration, s.draftRevision),
    sanitizeAssistantDisplayText(thinking),
  )
  streams = { ...streams, [sid]: { ...s, thinking } }
  listeners.forEach((l) => l())
  return true
}

export function appendStreamText(sid: string, t: string, expectedRunId?: string): boolean {
  const s = activeStream(sid, expectedRunId)
  if (!s) return false
  streams = { ...streams, [sid]: { ...s, text: s.text + t } }
  listeners.forEach((l) => l())
  return true
}

/** Discard only the current textual draft after an internal retry/reviewer veto.
 * Tool calls/results are committed research actions and remain in the stream. */
export function resetStreamDraft(sid: string, expectedRunId?: string): boolean {
  const s = activeStream(sid, expectedRunId)
  if (!s) return false
  discardThinkingDisclosureBlock(
    s.runId,
    thinkingDisclosureId(s.runId, s.currentIteration, s.draftRevision),
  )
  streams = {
    ...streams,
    [sid]: {
      ...s,
      thinking: '',
      text: '',
      draftRevision: s.draftRevision + 1,
    },
  }
  listeners.forEach((l) => l())
  return true
}

export function setStreamStart(
  sid: string,
  frameId: string,
  taskSummary: string,
  expectedRunId?: string,
): boolean {
  const stream = activeStream(sid, expectedRunId)
  if (!stream) return false
  streams = { ...streams, [sid]: { ...stream, frameId, taskSummary } }
  const detail = state.sessionStates[sid]
  if (detail) {
    setState({
      ...state,
      sessionStates: {
        ...state.sessionStates,
        [sid]: { ...detail, frame_id: frameId, task_summary: taskSummary },
      },
    })
  } else {
    listeners.forEach((listener) => listener())
  }
  return true
}

/** Start a new ordered UI segment and commit the completed previous iteration. */
export function advanceStreamIteration(
  sid: string,
  iteration: number,
  expectedRunId?: string,
): boolean {
  const stream = activeStream(sid, expectedRunId)
  if (!stream || iteration <= stream.currentIteration) return false
  if (stream.currentIteration > 0) commitStreamMessage(sid, stream)
  const current = streams[sid] ?? stream
  streams = {
    ...streams,
    [sid]: {
      ...current,
      currentIteration: iteration,
      draftRevision: 0,
      iterations: Math.max(current.iterations, iteration),
      thinking: '',
      text: '',
      usage: null,
      toolCalls: [],
    },
  }
  listeners.forEach((listener) => listener())
  return true
}

export function setStreamPlan(sid: string, plan: Plan, expectedRunId?: string): boolean {
  const s = streams[sid]
  if (expectedRunId !== undefined && (!s?.running || s.runId !== expectedRunId)) return false
  if (s) streams = { ...streams, [sid]: { ...s, plan } }
  const detail = state.sessionStates[sid]
  if (detail) {
    setState({
      ...state,
      sessionStates: {
        ...state.sessionStates,
        [sid]: { ...detail, plan },
      },
    })
  } else {
    listeners.forEach((l) => l())
  }
  return true
}

/** 追加一批 tool_calls（按 id 去重，前端契约要求）。 */
export function appendStreamToolCall(
  sid: string,
  calls: { id: string; name: string; input: Record<string, unknown> }[],
  expectedRunId?: string,
): boolean {
  const s = activeStream(sid, expectedRunId)
  if (!s) return false
  const existing = new Map(s.toolCalls.map((c) => [c.id, c]))
  for (const c of calls) {
    const prior = existing.get(c.id)
    if (!prior) {
      existing.set(c.id, { id: c.id, name: c.name, input: c.input, resolved: false })
    } else if (prior.name === '(unknown)') {
      existing.set(c.id, { ...prior, name: c.name, input: c.input })
    }
  }
  streams = { ...streams, [sid]: { ...s, toolCalls: [...existing.values()] } }
  listeners.forEach((l) => l())
  return true
}

/** 把 tool_results 回挂到对应 tool_call（按 tool_use_id 配对）。 */
export function appendStreamToolResult(
  sid: string,
  results: { tool_use_id: string; content: string; is_error: boolean }[],
  expectedRunId?: string,
): boolean {
  const s = activeStream(sid, expectedRunId)
  if (!s) return false
  const byId = new Map(s.toolCalls.map((c) => [c.id, { ...c }]))
  for (const r of results) {
    const call = byId.get(r.tool_use_id)
    if (call) {
      call.content = r.content
      call.is_error = r.is_error
      call.resolved = true
    } else {
      // 没匹配上对应 tool_call（理论上不该发生）：造一个占位，避免结果丢失。
      byId.set(r.tool_use_id, {
        id: r.tool_use_id,
        name: '(unknown)',
        input: {},
        content: r.content,
        is_error: r.is_error,
        resolved: true,
      })
    }
  }
  streams = { ...streams, [sid]: { ...s, toolCalls: [...byId.values()] } }
  listeners.forEach((l) => l())
  return true
}

/** 把当前 iteration 的 thinking/text/tool 按真实顺序提交为一条 assistant 消息。 */
function commitStreamMessage(sid: string, s: StreamBuffer): boolean {
  if (!s.thinking && !s.text && s.toolCalls.length === 0) return false
  const blocks: ContentBlock[] = []
  const thinkingDisclosureIds: string[] = []
  if (s.thinking) {
    blocks.push({ type: 'thinking', thinking: sanitizeAssistantDisplayText(s.thinking) })
    thinkingDisclosureIds.push(
      thinkingDisclosureId(s.runId, s.currentIteration, s.draftRevision),
    )
  }
  if (s.text) blocks.push({ type: 'text', text: sanitizeAssistantDisplayText(s.text) })
  // 工具调用按到达顺序输出 use + result 块（与历史消息渲染一致）。
  for (const c of s.toolCalls) {
    blocks.push({ type: 'tool_use', id: c.id, name: c.name, input: c.input })
    if (c.resolved) {
      blocks.push({
        type: 'tool_result',
        tool_use_id: c.id,
        content: c.content ?? '',
        is_error: !!c.is_error,
      })
    }
  }
  const message: Message = {
    role: 'assistant',
    content: blocks,
    usage: s.usage,
  }
  if (thinkingDisclosureIds.length > 0) {
    registerThinkingDisclosureMessage(message, s.runId, thinkingDisclosureIds)
  }
  setState({
    ...state,
    messages: {
      ...state.messages,
      [sid]: [
        ...(state.messages[sid] ?? []),
        message,
      ],
    },
  })
  return true
}

function runStatus(kind: RunKind, error: string | null): SessionStatus {
  if (kind === 'reconciliation') return 'needs_reconciliation'
  if (kind === 'cancelled') return 'interrupted'
  if (kind === 'error' || kind === 'max_iters' || error) return 'failed'
  if (kind === 'awaiting') return 'awaiting'
  return 'completed'
}

function streamRun(
  stream: StreamBuffer,
  kind: RunKind,
  usage: Usage | null,
  iterations: number,
  pendingAsk: PendingAsk | null,
  awaiting: AwaitingKind,
  plan: Plan | null,
  error: string | null,
): SessionRun {
  return {
    run_id: stream.runId,
    ordinal: 0,
    frame_id: stream.frameId,
    task_summary: stream.taskSummary,
    plan_mode: stream.planMode,
    status: runStatus(kind, error),
    kind,
    awaiting,
    pending_ask: pendingAsk,
    error,
    usage: usage ?? { input_tokens: 0, output_tokens: 0 },
    iterations,
    plan,
    start_seq: null,
    end_seq: null,
    started_at: stream.startedAt,
    completed_at: new Date().toISOString(),
  }
}

function attachRunToCurrentTurn(sid: string, startIndex: number, run: SessionRun) {
  const messages = [...(state.messages[sid] ?? [])]
  if (messages.length === 0) return
  const target = Math.max(0, Math.min(messages.length - 1, Math.max(startIndex, messages.length - 1)))
  const source = messages[target]!
  const next = { ...source, run }
  carryThinkingDisclosureMessageMetadata(source, next)
  messages[target] = next
  setState({ ...state, messages: { ...state.messages, [sid]: messages } })
}

function setSessionStatus(
  sid: string,
  status: SessionStatus,
  plan?: Plan | null,
  artifacts?: Record<string, Artifact>,
) {
  const detail = state.sessionStates[sid]
  const projectId = state.sessions.find((session) => session.id === sid)?.project_id
  setState({
    ...state,
    projects: state.projects.map((project) =>
      project.id === projectId
        ? { ...project, last_session_id: sid, updated_at: new Date().toISOString() }
        : project,
    ),
    sessions: state.sessions.map((sess) =>
      sess.id === sid ? { ...sess, status, updated_at: new Date().toISOString() } : sess,
    ),
    sessionStates: detail
      ? {
          ...state.sessionStates,
          [sid]: {
            ...detail,
            status,
            plan: plan === undefined ? detail.plan : plan,
            artifacts: artifacts ?? detail.artifacts,
          },
        }
      : state.sessionStates,
  })
}

export function completeStream(
  sid: string,
  usage: Usage | null,
  iterations: number,
  kind: RunKind = 'natural',
  pendingAsk: PendingAsk | null = null,
  awaiting: AwaitingKind = null,
  plan: Plan | null = null,
  error: string | null = null,
  artifacts?: Record<string, Artifact>,
  expectedRunId?: string,
): boolean {
  const s = activeStream(sid, expectedRunId)
  if (!s) return false
  const terminalError =
    error ??
    (kind === 'error'
      ? 'Agent 执行失败，后端未返回详细原因。'
      : kind === 'max_iters'
        ? 'Agent 达到迭代上限，后端未返回详细原因。'
        : null)
  const terminalPlan = plan ?? s.plan
  const terminalStream = { ...s, usage, iterations }
  commitStreamMessage(sid, terminalStream)
  attachRunToCurrentTurn(
    sid,
    s.messageStartIndex,
    streamRun(
      terminalStream,
      kind,
      usage,
      iterations,
      pendingAsk,
      awaiting,
      terminalPlan,
      terminalError,
    ),
  )
  streams = {
    ...streams,
    [sid]: {
      ...s,
      running: false,
      stopping: false,
      stopped: false,
      thinking: '',
      text: '',
      toolCalls: [],
      usage,
      iterations,
      kind,
      pendingAsk,
      awaiting,
      plan: terminalPlan,
      deferredStopError: null,
      error: terminalError,
    },
  }
  setSessionStatus(sid, runStatus(kind, terminalError), terminalPlan, artifacts)
  deleteStreamAborter(sid, s.runId)
  listeners.forEach((l) => l())
  return true
}

export function failStream(sid: string, error: string, expectedRunId?: string): boolean {
  const s = activeStream(sid, expectedRunId)
  if (!s) return false
  // Explicit cancellation closes the SSE transport without a terminal frame.
  // The cancel endpoint, not this transport symptom, owns the final UI state.
  if (s.stopping) {
    streams = { ...streams, [sid]: { ...s, deferredStopError: error } }
    listeners.forEach((l) => l())
    return true
  }
  return failStreamNow(sid, s, error)
}

function failStreamNow(sid: string, s: StreamBuffer, error: string): boolean {
  commitStreamMessage(sid, s) // 已流出的部分不丢
  attachRunToCurrentTurn(
    sid,
    s.messageStartIndex,
    streamRun(
      s,
      'error',
      s.usage,
      s.iterations || s.currentIteration,
      null,
      null,
      s.plan,
      error,
    ),
  )
  streams = {
    ...streams,
    [sid]: {
      ...s,
      running: false,
      stopping: false,
      thinking: '',
      text: '',
      toolCalls: [],
      kind: 'error',
      pendingAsk: null,
      awaiting: null,
      stopped: false,
      error,
      deferredStopError: null,
    },
  }
  setSessionStatus(sid, 'failed')
  deleteStreamAborter(sid, s.runId)
  listeners.forEach((l) => l())
  return true
}

export function setStreamAborter(
  sid: string,
  fn: () => void,
  expectedRunId?: string,
): boolean {
  const s = activeStream(sid, expectedRunId)
  if (!s) {
    fn()
    return false
  }
  aborters.set(sid, { runId: s.runId, abort: fn })
  return true
}

/** Enter stopping state without unlocking the composer or aborting SSE yet. */
export function beginStreamStop(sid: string, expectedRunId?: string): string | null {
  const s = streams[sid]
  if (
    !s?.running ||
    s.stopping ||
    (expectedRunId !== undefined && s.runId !== expectedRunId)
  ) return null
  streams = {
    ...streams,
    [sid]: { ...s, stopping: true, error: null },
  }
  listeners.forEach((l) => l())
  return s.runId
}

/** Backend acknowledged cancellation after releasing the session mutex. */
export function finishStreamStop(sid: string, expectedRunId?: string): boolean {
  const s = activeStream(sid, expectedRunId)
  // Normal completion or a newer run may have won while cancellation was in flight.
  if (!s) return false
  const aborter = aborters.get(sid)
  if (aborter?.runId === s.runId) aborter.abort()
  commitStreamMessage(sid, s)
  attachRunToCurrentTurn(
    sid,
    s.messageStartIndex,
    streamRun(
      s,
      'cancelled',
      s.usage,
      s.iterations || s.currentIteration,
      null,
      null,
      s.plan,
      null,
    ),
  )
  streams = {
    ...streams,
    [sid]: {
      ...s,
      running: false,
      stopping: false,
      stopped: true,
      thinking: '',
      text: '',
      toolCalls: [],
      kind: 'cancelled',
      pendingAsk: null,
      awaiting: null,
      deferredStopError: null,
    },
  }
  setSessionStatus(sid, 'interrupted')
  deleteStreamAborter(sid, s.runId)
  listeners.forEach((l) => l())
  return true
}

/** Cancellation could not be acknowledged. Keep the composer locked and allow retry. */
export function failStreamStop(sid: string, error: string, expectedRunId?: string): boolean {
  const s = activeStream(sid, expectedRunId)
  if (!s) return false
  streams = {
    ...streams,
    [sid]: { ...s, stopping: false, error: `停止失败：${error}` },
  }
  listeners.forEach((l) => l())
  return true
}

/** A terminal event had already committed, so resume waiting for normal SSE completion. */
export function resumeStreamAfterLateStop(sid: string, expectedRunId?: string): boolean {
  const s = activeStream(sid, expectedRunId)
  if (!s) return false
  if (s.deferredStopError) {
    return failStreamNow(sid, { ...s, stopping: false }, s.deferredStopError)
  }
  streams = { ...streams, [sid]: { ...s, stopping: false } }
  listeners.forEach((l) => l())
  return true
}

/**
 * The cancel endpoint reported that a normal terminal beat Stop and the caller
 * has already restored the authoritative transcript after persistence. Drop
 * the exact local shell without committing its partial draft: appending here
 * would duplicate content that is already present in the restored history.
 */
export function retireStreamAfterBackendFinish(
  sid: string,
  expectedRunId: string,
): boolean {
  const s = streams[sid]
  if (!s?.running || s.runId !== expectedRunId) return false

  const aborter = aborters.get(sid)
  if (aborter?.runId === s.runId) aborter.abort()
  const nextStreams = { ...streams }
  delete nextStreams[sid]
  streams = nextStreams
  deleteStreamAborter(sid, s.runId)
  listeners.forEach((listener) => listener())
  return true
}

/** Imperative snapshot for protocol coordinators and deterministic tests. */
export function getStreamSnapshot(sid: string): StreamBuffer | undefined {
  return streams[sid]
}

/** Imperative transcript snapshot for protocol/restore regression tests. */
export function getMessagesSnapshot(sid: string): Message[] {
  return state.messages[sid] ?? EMPTY_MESSAGES
}
