// API 客户端：projects/sessions/health/config 走真实后端；其余（MCP/Skills/Templates/Files/Compile/Logs）仍 mock（对应后端阶段未做）。
import type {
  AppConfig,
  AppSettings,
  Artifact,
  BackendStatus,
  CompileReq,
  CompileResult,
  ContentBlock,
  HealthResponse,
  LogEntry,
  McpServer,
  Memory,
  Message,
  Plan,
  Project,
  ProjectDetail,
  RunResult,
  SessionState,
  SessionSummary,
  Skill,
  SSEEvent,
  TemplateInfo,
  WorkspaceFile,
} from '../types'
import {
  mockArtifacts,
  mockFiles,
  mockLogs,
  mockMcpServers,
  mockSettings,
  mockSkills,
  mockTemplates,
} from '../mock/data'

/** API 基址：Tauri 注入端口优先，浏览器开发走 Vite 代理（/api）。 */
export function apiBase(): string {
  const w = window as unknown as { __BACKEND_PORT__?: number }
  const port = w.__BACKEND_PORT__ ?? localStorage.getItem('dss_backend_port')
  return port ? `http://127.0.0.1:${port}/api` : '/api'
}

/** 统一 fetch 封装：非 ok 抛错；ok 解 JSON。 */
async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const r = await fetch(`${apiBase()}${path}`, init)
  if (!r.ok) {
    let msg = `${r.status} ${r.statusText}`
    try {
      const body = await r.json()
      if (body?.error) msg = String(body.error)
    } catch {
      // 非 JSON 错误体
    }
    throw new Error(msg)
  }
  if (r.status === 204) return undefined as unknown as T
  return (await r.json()) as T
}

/** localStorage 写助手（仅 settings/mcp 仍在本地）。 */
function readLS<T>(key: string, seed: T[]): T[] {
  try {
    const raw = localStorage.getItem(key)
    if (raw) return JSON.parse(raw) as T[]
  } catch {
    // 解析失败则回退 seed
  }
  localStorage.setItem(key, JSON.stringify(seed))
  return [...seed]
}

function writeLS<T>(key: string, value: T) {
  localStorage.setItem(key, JSON.stringify(value))
}

// ---------- 系统 / 配置 ----------
export async function getHealth(): Promise<HealthResponse> {
  return request<HealthResponse>('/health')
}

export async function getConfig(): Promise<AppConfig> {
  // 后端 GET /api/config 当前返回子集（llm_configured/model/base_url）；补默认值给前端。
  const c = await request<{ llm_configured: boolean; model: string; base_url: string }>('/config')
  return {
    llm_configured: c.llm_configured,
    model: c.model,
    base_url: c.base_url,
    context_window: 128_000,
    has_mcp: false,
    mcp_count: 0,
    api_keys_configured: c.llm_configured ? ['deepseek'] : [],
    default_workspace: '~/deepseek-science',
    host: '127.0.0.1',
    port: 17896,
  }
}

const SETTINGS_KEY = 'dss_settings'

export async function getSettings(): Promise<AppSettings> {
  // 后端 /api/settings 端点尚未做；暂走 localStorage。
  try {
    const raw = localStorage.getItem(SETTINGS_KEY)
    if (raw) return JSON.parse(raw) as AppSettings
  } catch {
    // 解析失败则回退 mock
  }
  localStorage.setItem(SETTINGS_KEY, JSON.stringify(mockSettings))
  return structuredClone(mockSettings)
}

export async function saveSettings(patch: Partial<AppSettings>): Promise<AppSettings> {
  const next = { ...(await getSettings()), ...patch }
  localStorage.setItem(SETTINGS_KEY, JSON.stringify(next))
  return next
}

// ---------- MCP ----------
const MCP_KEY = 'dss_mcp_servers'

export async function listMcpServers(): Promise<McpServer[]> {
  return readLS(MCP_KEY, mockMcpServers)
}

export async function saveMcpServers(servers: McpServer[]): Promise<void> {
  writeLS(MCP_KEY, servers)
}

// ---------- 记忆 ----------
export async function listMemories(_entity?: string): Promise<Memory[]> {
  return []
}

export async function deleteMemory(_memId: string): Promise<void> {}

// ---------- Skills / 模板 ----------
export async function listSkills(): Promise<Skill[]> {
  try {
    return await request<Skill[]>('/skills')
  } catch {
    return mockSkills
  }
}

export async function listTemplates(): Promise<TemplateInfo[]> {
  try {
    return await request<TemplateInfo[]>('/templates')
  } catch {
    return mockTemplates
  }
}

export async function getTemplate(templateId: string): Promise<string> {
  try {
    const r = await fetch(`${apiBase()}/templates/${encodeURIComponent(templateId)}`)
    if (!r.ok) throw new Error(`${r.status}`)
    return await r.text()
  } catch {
    return '\\documentclass{ctexart}\n\\begin{document}\n\\end{document}\n'
  }
}

// ---------- 后端行 → 前端类型映射 ----------

/** 后端 projects 行（dss-db ProjectRow 序列化）。 */
interface ProjectRowBE {
  id: string
  name: string
  description: string | null
  last_session_id: string | null
  archived: boolean
  created_at: string
  updated_at: string
}

function mapProject(r: ProjectRowBE): Project {
  return {
    id: r.id,
    name: r.name,
    description: r.description ?? '',
    agent_context: '',
    session_count: 0,
    pinned: false,
    archived: r.archived,
    created_at: r.created_at,
    updated_at: r.updated_at,
  }
}

// ---------- Projects ----------
export async function listProjects(archived = false): Promise<Project[]> {
  const rows = await request<ProjectRowBE[]>(`/projects?archived=${archived ? 'true' : 'false'}`)
  return rows.map(mapProject)
}

export async function createProject(input: {
  name: string
  description?: string
  agent_context?: string
}): Promise<Project> {
  const row = await request<ProjectRowBE>('/projects', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ name: input.name, description: input.description ?? null }),
  })
  return mapProject(row)
}

export async function updateProject(pid: string, patch: Partial<Project>): Promise<Project> {
  const row = await request<ProjectRowBE>(`/projects/${encodeURIComponent(pid)}`, {
    method: 'PATCH',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      name: patch.name,
      description: patch.description,
    }),
  })
  return mapProject(row)
}

export async function archiveProject(pid: string): Promise<Project> {
  const row = await request<ProjectRowBE>(`/projects/${encodeURIComponent(pid)}/archive`, {
    method: 'POST',
  })
  return mapProject(row)
}

export async function unarchiveProject(pid: string): Promise<Project> {
  const row = await request<ProjectRowBE>(`/projects/${encodeURIComponent(pid)}/unarchive`, {
    method: 'POST',
  })
  return mapProject(row)
}

export async function deleteProject(pid: string, force = false): Promise<void> {
  await request<void>(`/projects/${encodeURIComponent(pid)}?force=${force ? 'true' : 'false'}`, {
    method: 'DELETE',
  })
}

export async function getProject(pid: string): Promise<ProjectDetail> {
  // 后端返回 { project: ProjectRow, sessions: SessionRow[] }
  const data = await request<{ project: ProjectRowBE; sessions: SessionRowBE[] }>(
    `/projects/${encodeURIComponent(pid)}`,
  )
  const p = mapProject(data.project)
  p.session_count = data.sessions.length
  return { ...p, sessions: data.sessions.map(mapSession) }
}

// ---------- Sessions ----------
/** 后端 sessions 行（dss-db SessionRow 序列化 + GET /sessions 列表项的 live）。 */
interface SessionRowBE {
  id: string
  title: string | null
  workspace: string
  model: string | null
  plan_mode?: boolean
  status: string
  project_id: string | null
  live?: boolean
  created_at: string
  updated_at: string
}

function mapSession(r: SessionRowBE): SessionSummary {
  // 后端 status: active/...；前端 SessionStatus: processing|completed|failed|interrupted|awaiting
  const statusMap: Record<string, SessionSummary['status']> = {
    active: 'completed',
    processing: 'processing',
    completed: 'completed',
    failed: 'failed',
    awaiting: 'awaiting',
  }
  return {
    id: r.id,
    project_id: r.project_id ?? 'proj_default',
    title: r.title ?? 'New session',
    status: statusMap[r.status] ?? 'completed',
    live: r.live ?? false,
    created_at: r.created_at,
    updated_at: r.updated_at,
  }
}

export async function listSessions(): Promise<SessionSummary[]> {
  const rows = await request<SessionRowBE[]>('/sessions')
  return rows.map(mapSession)
}

/** 后端探测：GET /api/health + /api/config，判断在线与 LLM 配置状态。 */
export async function probeBackend(): Promise<BackendStatus> {
  try {
    const h = await fetch(`${apiBase()}/health`)
    if (!h.ok) return { online: false, llmConfigured: false }
    const c = await fetch(`${apiBase()}/config`)
    const cfg = c.ok ? ((await c.json()) as AppConfig) : null
    return { online: true, llmConfigured: cfg?.llm_configured ?? false, model: cfg?.model }
  } catch {
    return { online: false, llmConfigured: false }
  }
}

/** 真实建会话：POST /api/sessions → {id, frame_id, model, workspace}。 */
export async function createSessionApi(
  projectId: string,
): Promise<{ id: string; frame_id: string; mcp_tools: string[]; model: string; workspace: string }> {
  const r = await fetch(`${apiBase()}/sessions`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ project_id: projectId }),
  })
  if (!r.ok) throw new Error(`POST /api/sessions failed: ${r.status}`)
  return (await r.json()) as {
    id: string
    frame_id: string
    mcp_tools: string[]
    model: string
    workspace: string
  }
}

export async function getSession(sid: string): Promise<SessionState> {
  const data = await request<{
    id: string
    title: string | null
    workspace: string
    model: string | null
    status: string
    plan_mode: boolean
    project_id: string | null
    messages: { role: string; content: unknown; harness_notice: boolean }[]
  }>(`/sessions/${encodeURIComponent(sid)}`)
  // 后端把每条消息存为「OpenAI 协议形态 ChatMessage」对象（{role,content,tool_calls,tool_call_id,name}）。
  // 转成前端 Message：content 拼 ContentBlock[]（text / tool_use / tool_result），ChatArea pairTools 据此渲染。
  const messages: Message[] = data.messages.map((m) => {
    const obj = (m.content ?? {}) as {
      content?: string | null
      tool_calls?: { id: string; function: { name: string; arguments: string } }[]
      tool_call_id?: string
    }
    const role: 'user' | 'assistant' = m.role === 'user' ? 'user' : 'assistant'
    const blocks: ContentBlock[] = []
    if (obj.content) blocks.push({ type: 'text', text: obj.content })
    if (obj.tool_calls) {
      for (const tc of obj.tool_calls) {
        let input: Record<string, unknown> = {}
        try {
          input = JSON.parse(tc.function.arguments || '{}') as Record<string, unknown>
        } catch {
          input = { _raw: tc.function.arguments }
        }
        blocks.push({ type: 'tool_use', id: tc.id, name: tc.function.name, input })
      }
    }
    if (m.role === 'tool' && obj.tool_call_id) {
      // tool 结果：单独一条 user-role tool_result 块（前端按 tool_use_id 配对）。
      blocks.length = 0
      blocks.push({
        type: 'tool_result',
        tool_use_id: obj.tool_call_id,
        content: obj.content ?? '',
        is_error: false,
      })
      return {
        role: 'user',
        content: blocks,
        harness_notice: m.harness_notice ? true : null,
      }
    }
    // 无文本无工具调用时，content 用空字符串兜底（避免 ContentBlock[] 为空被当 string）。
    const content: ContentBlock[] | string =
      blocks.length > 0 ? blocks : obj.content ?? ''
    return { role, content, harness_notice: m.harness_notice ? true : null }
  })
  return {
    id: data.id,
    frame_id: '',
    status: 'completed',
    task_summary: data.title ?? '',
    plan_mode: data.plan_mode,
    plan: null,
    artifacts: {},
    messages,
  }
}

export async function deleteSession(sid: string): Promise<void> {
  await request<void>(`/sessions/${encodeURIComponent(sid)}`, { method: 'DELETE' })
}

// ---------- Files ----------
export async function listFiles(_sid: string): Promise<WorkspaceFile[]> {
  return mockFiles
}

export async function readFile(_sid: string, _path: string): Promise<string> {
  return ''
}

export async function deleteFile(_sid: string, _path: string): Promise<void> {}

// ---------- Run / Compile ----------
export async function runOnce(_sid: string, _prompt: string): Promise<RunResult> {
  return {
    kind: 'natural',
    final_text: '',
    awaiting: null,
    usage: { input_tokens: 0, output_tokens: 0 },
    iterations: 0,
  }
}

export async function compileTex(_sid: string, _req: CompileReq): Promise<CompileResult> {
  return { success: true, pdf_path: 'review_leadfree_perovskite.pdf', size_kb: 871, message: '', errors: [], log_excerpt: '' }
}

export async function listArtifacts(_sid: string): Promise<Artifact[]> {
  return mockArtifacts
}

export { mockLogs }
export type { LogEntry }

// ---------- 流式 SSE ----------
// POST /api/sessions/{sid}/stream-sse：fetch + ReadableStream 手写 SSE 解析。
// 帧格式：每行 `data: {json}`，帧间以空行（\n\n）分隔；事件类型见 types.ts SSEEvent。
export interface StreamHandlers {
  onStart?: (frameId: string, taskSummary: string) => void
  onIteration?: (n: number) => void
  onThinking?: (text: string) => void
  onText?: (text: string) => void
  onToolCalls?: (calls: Extract<SSEEvent, { type: 'tool_calls' }>['calls']) => void
  onToolResults?: (results: Extract<SSEEvent, { type: 'tool_results' }>['results']) => void
  onPlanUpdate?: (plan: Plan) => void
  onNotice?: (event: string, detail: string) => void
  onComplete?: (e: Extract<SSEEvent, { type: 'complete' }>) => void
  onError?: (message: string) => void
}

function dispatchEvent(ev: SSEEvent, h: StreamHandlers) {
  switch (ev.type) {
    case 'start':
      h.onStart?.(ev.frame_id, ev.task_summary)
      break
    case 'iteration':
      h.onIteration?.(ev.n)
      break
    case 'thinking':
      h.onThinking?.(ev.text)
      break
    case 'text':
      h.onText?.(ev.text)
      break
    case 'tool_calls':
      h.onToolCalls?.(ev.calls)
      break
    case 'tool_results':
      h.onToolResults?.(ev.results)
      break
    case 'plan_update':
      h.onPlanUpdate?.(ev.plan)
      break
    case 'notice':
      h.onNotice?.(ev.event, ev.detail)
      break
    case 'complete':
      h.onComplete?.(ev)
      break
    case 'error':
      h.onError?.(ev.message)
      break
  }
}

/** 建立 SSE 流；返回 abort 函数（停止按钮 / 组件卸载时调用）。 */
export function connectSSE(sid: string, prompt: string, handlers: StreamHandlers): () => void {
  const ctrl = new AbortController()
  let terminated = false // 收到 complete/error 事件

  void (async () => {
    try {
      const res = await fetch(`${apiBase()}/sessions/${encodeURIComponent(sid)}/stream-sse`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', Accept: 'text/event-stream' },
        body: JSON.stringify({ prompt }),
        signal: ctrl.signal,
      })
      if (!res.ok || !res.body) {
        handlers.onError?.(`HTTP ${res.status} ${res.statusText}`.trim())
        return
      }
      const reader = res.body.getReader()
      const decoder = new TextDecoder()
      let buf = ''
      for (;;) {
        const { done, value } = await reader.read()
        if (done) break
        buf += decoder.decode(value, { stream: true })
        // 按空行分帧；一帧内可能有多行 data:
        let idx: number
        while ((idx = buf.indexOf('\n\n')) >= 0) {
          const frame = buf.slice(0, idx)
          buf = buf.slice(idx + 2)
          for (const line of frame.split('\n')) {
            if (!line.startsWith('data:')) continue
            const payload = line.slice(5).trim()
            if (!payload) continue
            let ev: SSEEvent
            try {
              ev = JSON.parse(payload) as SSEEvent
            } catch {
              continue // 忽略无法解析的帧
            }
            dispatchEvent(ev, handlers)
            if (ev.type === 'complete' || ev.type === 'error') terminated = true
          }
        }
      }
      // 契约要求 complete 必达；流提前结束视为错误
      if (!terminated && !ctrl.signal.aborted) {
        handlers.onError?.('连接中断：流在未收到 complete 时结束')
      }
    } catch (e) {
      if (ctrl.signal.aborted) return // 用户主动停止，不报错
      handlers.onError?.(e instanceof Error ? e.message : String(e))
    }
  })()

  return () => ctrl.abort()
}

// ---------- 日志 ----------

export interface LogQuery {
  session_id?: string
  source?: string
  /** 逗号分隔多值，如 "warn,error" */
  level?: string
  kind?: string
  since?: string
  until?: string
  limit?: number
  offset?: number
}

export async function listLogs(query: LogQuery = {}): Promise<{ logs: LogEntry[]; total: number }> {
  const qs = new URLSearchParams()
  if (query.session_id) qs.set('session_id', query.session_id)
  if (query.source) qs.set('source', query.source)
  if (query.level) qs.set('level', query.level)
  if (query.kind) qs.set('kind', query.kind)
  if (query.since) qs.set('since', query.since)
  if (query.until) qs.set('until', query.until)
  if (query.limit != null) qs.set('limit', String(query.limit))
  if (query.offset != null) qs.set('offset', String(query.offset))
  const q = qs.toString()
  try {
    return await request<{ logs: LogEntry[]; total: number }>(`/logs${q ? `?${q}` : ''}`)
  } catch {
    return { logs: [], total: 0 }
  }
}

export async function getLog(id: number): Promise<LogEntry | null> {
  try {
    return await request<LogEntry>(`/logs/${id}`)
  } catch {
    return null
  }
}

export async function clearLogs(before?: string): Promise<void> {
  const q = before ? `?before=${encodeURIComponent(before)}` : ''
  try {
    await request<{ deleted: number }>(`/logs${q}`, { method: 'DELETE' })
  } catch {
    /* ignore */
  }
}
