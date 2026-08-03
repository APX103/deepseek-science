// 轻量前端 store：useSyncExternalStore 驱动，不引外部状态库。
// projects/sessions 从真实后端 API 加载；messages 按需从后端恢复。
// 流式 buffer（stream）仍是纯内存。
import { useSyncExternalStore } from 'react'
import type { ContentBlock, Message, Project, SessionSummary, Usage } from './types'
import * as api from './api/client'

interface State {
  projects: Project[]
  sessions: SessionSummary[]
  messages: Record<string, Message[]>
  /** 后端是否在线（影响 Home/Workbench 空态提示）。 */
  backendOnline: boolean
}

const EMPTY_MESSAGES: Message[] = []

function emptyState(): State {
  return { projects: [], sessions: [], messages: {}, backendOnline: false }
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
    const [projects, sessions] = await Promise.all([api.listProjects(), api.listSessions()])
    setState({ ...state, projects, sessions, backendOnline: true })
  } catch {
    setState({ ...state, projects: [], sessions: [], backendOnline: false })
  }
}

/** 拉取某会话的消息历史（从 GET /sessions/{sid} 恢复），回填 messages[sid]。 */
export async function loadMessages(sid: string): Promise<void> {
  try {
    const s = await api.getSession(sid)
    setState({ ...state, messages: { ...state.messages, [sid]: s.messages } })
  } catch {
    // 后端无此会话或离线：保留空。
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

/** 归档/删除在第一版均为前端态移除（TODO: 接后端后区分软删/硬删）。 */
export function removeProject(pid: string) {
  setState({ ...state, projects: state.projects.filter((p) => p.id !== pid) })
}

// ---------- sessions ----------
/** 创建新会话（可传入后端真实 sid；标题待首条消息生成）。 */
export function createSession(projectId: string, opts?: { id?: string }): SessionSummary {
  const now = new Date().toISOString()
  const s: SessionSummary = {
    id: opts?.id ?? crypto.randomUUID().replace(/-/g, '').slice(0, 12),
    project_id: projectId,
    title: 'New session',
    status: 'awaiting',
    live: true,
    created_at: now,
    updated_at: now,
  }
  setState({
    ...state,
    sessions: [s, ...state.sessions],
    messages: { ...state.messages, [s.id]: [] },
  })
  return s
}

/** 追加一条用户消息；首条消息同时作为会话标题。 */
export function sendUserMessage(sid: string, text: string) {
  const content = text.trim()
  if (!content) return
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
  running: boolean
  stopped: boolean
  thinking: string
  text: string
  usage: Usage | null
  iterations: number
  error: string | null
  /** 本轮 run 累积的工具调用（含结果，按到达顺序）。 */
  toolCalls: StreamToolCall[]
  /** ask_user 挂起的提问（complete.kind=awaiting 时才有）。 */
  pendingAsk: import('./types').PendingAsk | null
  /** complete.kind（complete 后填充）。 */
  kind: import('./types').RunKind | null
}

let streams: Record<string, StreamBuffer> = {}
const aborters = new Map<string, () => void>()

export function useStream(sid: string): StreamBuffer | undefined {
  return useSyncExternalStore(subscribe, () => streams[sid])
}

export function startStream(sid: string) {
  streams = {
    ...streams,
    [sid]: {
      running: true,
      stopped: false,
      thinking: '',
      text: '',
      usage: null,
      iterations: 0,
      error: null,
      toolCalls: [],
      pendingAsk: null,
      kind: null,
    },
  }
  listeners.forEach((l) => l())
}

export function appendStreamThinking(sid: string, t: string) {
  const s = streams[sid]
  if (!s?.running) return
  streams = { ...streams, [sid]: { ...s, thinking: s.thinking + t } }
  listeners.forEach((l) => l())
}

export function appendStreamText(sid: string, t: string) {
  const s = streams[sid]
  if (!s?.running) return
  streams = { ...streams, [sid]: { ...s, text: s.text + t } }
  listeners.forEach((l) => l())
}

/** 追加一批 tool_calls（按 id 去重，前端契约要求）。 */
export function appendStreamToolCall(
  sid: string,
  calls: { id: string; name: string; input: Record<string, unknown> }[],
) {
  const s = streams[sid]
  if (!s?.running) return
  const existing = new Map(s.toolCalls.map((c) => [c.id, c]))
  for (const c of calls) {
    if (!existing.has(c.id)) {
      existing.set(c.id, { id: c.id, name: c.name, input: c.input, resolved: false })
    }
  }
  streams = { ...streams, [sid]: { ...s, toolCalls: [...existing.values()] } }
  listeners.forEach((l) => l())
}

/** 把 tool_results 回挂到对应 tool_call（按 tool_use_id 配对）。 */
export function appendStreamToolResult(
  sid: string,
  results: { tool_use_id: string; content: string; is_error: boolean }[],
) {
  const s = streams[sid]
  if (!s?.running) return
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
}

/** 把 buffer 里已流出的 thinking/text/tool 提交为一条 assistant 消息（进 dss_state 持久化）。 */
function commitStreamMessage(sid: string, s: StreamBuffer) {
  if (!s.thinking && !s.text && s.toolCalls.length === 0) return
  const blocks: ContentBlock[] = []
  if (s.thinking) blocks.push({ type: 'thinking', thinking: s.thinking })
  if (s.text) blocks.push({ type: 'text', text: s.text })
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
  setState({
    ...state,
    messages: { ...state.messages, [sid]: [...(state.messages[sid] ?? []), { role: 'assistant', content: blocks }] },
  })
}

export function completeStream(
  sid: string,
  usage: Usage | null,
  iterations: number,
  kind: import('./types').RunKind = 'natural',
  pendingAsk: import('./types').PendingAsk | null = null,
) {
  const s = streams[sid]
  if (!s) return
  commitStreamMessage(sid, s)
  streams = {
    ...streams,
    [sid]: { ...s, running: false, thinking: '', text: '', usage, iterations, kind, pendingAsk },
  }
  // awaiting 时把会话状态置 awaiting（侧栏/输入框据此切换为"回复"态）。
  if (kind === 'awaiting') {
    setState({
      ...state,
      sessions: state.sessions.map((sess) =>
        sess.id === sid ? { ...sess, status: 'awaiting', updated_at: new Date().toISOString() } : sess,
      ),
    })
  }
  aborters.delete(sid)
  listeners.forEach((l) => l())
}

export function failStream(sid: string, error: string) {
  const s = streams[sid]
  if (!s) return
  commitStreamMessage(sid, s) // 已流出的部分不丢
  streams = { ...streams, [sid]: { ...s, running: false, thinking: '', text: '', error } }
  aborters.delete(sid)
  listeners.forEach((l) => l())
}

export function setStreamAborter(sid: string, fn: () => void) {
  aborters.set(sid, fn)
}

/** 停止：abort 底层 fetch，已流出的部分照样提交。 */
export function stopStream(sid: string) {
  aborters.get(sid)?.()
  const s = streams[sid]
  if (!s?.running) return
  commitStreamMessage(sid, s)
  streams = { ...streams, [sid]: { ...s, running: false, stopped: true, thinking: '', text: '' } }
  aborters.delete(sid)
  listeners.forEach((l) => l())
}
