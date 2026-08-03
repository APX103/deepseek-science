// 类型定义：与 docs/api-contract.md 对齐（字段名即契约）。

// ---------- 系统 / 配置 ----------
export interface HealthResponse {
  status: 'ok'
  version: string
}

export interface AppConfig {
  llm_configured: boolean
  model: string
  base_url: string
  context_window: number
  has_mcp: boolean
  mcp_count: number
  api_keys_configured: string[]
  default_workspace: string
  host: string
  port: number
}

export interface AppSettingsProvider {
  name: string
  base_url: string
  api_key_masked: string
  enabled: boolean
  /** 契约外的本地扩展：每个 provider 记录默认模型（Settings 弹层可编辑）。 */
  model?: string
}

export interface AppSettings {
  providers: AppSettingsProvider[]
  model: string
  default_workspace: string
}

// ---------- MCP ----------
export interface McpServer {
  name: string
  url: string
  enabled: boolean
  connected: boolean
}

// ---------- 记忆 ----------
export interface Memory {
  id: string
  entity: string
  content: string
  updated_at: string
}

// ---------- Skills / 模板 ----------
export interface Skill {
  name: string
  description: string
  source: string
  enabled: boolean
}

export interface TemplateInfo {
  id: string
  name: string
  description: string
  documentclass: string
  columns: 1 | 2
}

// ---------- Projects ----------
export interface Project {
  id: string // proj_<8hex>
  name: string
  description: string
  agent_context: string
  session_count: number
  pinned: boolean
  archived: boolean
  created_at: string
  updated_at: string
}

export interface ProjectDetail extends Project {
  sessions: SessionSummary[]
}

// ---------- Sessions ----------
export type SessionStatus = 'processing' | 'completed' | 'failed' | 'interrupted' | 'awaiting'

export interface SessionSummary {
  id: string
  project_id: string
  title: string
  status: SessionStatus
  live: boolean
  created_at: string
  updated_at: string
}

// content block 形态（_serialize_content）
export type ContentBlock =
  | { type: 'thinking'; thinking: string }
  | { type: 'text'; text: string }
  | { type: 'tool_use'; id: string; name: string; input: Record<string, unknown> }
  | { type: 'tool_result'; tool_use_id: string; content: string; is_error: boolean }

export interface Message {
  role: 'user' | 'assistant'
  content: string | ContentBlock[]
  harness_notice?: boolean | null
}

export interface PlanStep {
  title: string
  status: 'pending' | 'running' | 'done' | 'failed'
}

export interface Plan {
  steps: PlanStep[]
  approved: boolean
}

export interface Artifact {
  path: string
  size: number
  frame_id: string
  kind: 'markdown' | 'tex' | 'pdf' | 'image' | 'data' | 'other'
  origin: 'agent' | 'upload'
  created_at: string
}

export interface Usage {
  input_tokens: number
  output_tokens: number
}

export interface SessionState {
  id: string
  frame_id: string
  status: SessionStatus
  task_summary: string
  plan_mode: boolean
  plan: Plan | null
  artifacts: Record<string, Artifact>
  messages: Message[]
}

// ---------- Files ----------
export interface WorkspaceFile {
  path: string
  size: number
  name: string
}

// ---------- Run / Compile ----------
export type RunKind = 'natural' | 'awaiting' | 'max_iters' | 'error' | 'cancelled'
export type AwaitingKind = 'user_response' | 'plan_approval' | null

/** ask_user 工具挂起的提问（complete.pending_ask）。 */
export interface PendingAskOption {
  label: string
  description?: string
}
export interface PendingAsk {
  question: string
  options?: PendingAskOption[]
  header?: string
}

export interface RunReq {
  prompt: string
  plan_mode?: boolean
  deep_review?: boolean
}

export interface RunResult {
  kind: RunKind
  final_text: string
  awaiting: AwaitingKind
  pending_ask?: PendingAsk
  error?: string
  usage: Usage
  iterations: number
}

export interface CompileReq {
  path: string
  out_name?: string
}

export interface CompileResult {
  success: boolean
  pdf_path: string
  size_kb: number
  message: string
  errors: string[]
  log_excerpt: string
}

// ---------- SSE 事件 ----------
export type SSEEvent =
  | { type: 'start'; frame_id: string; task_summary: string }
  | { type: 'iteration'; n: number }
  | { type: 'thinking'; text: string }
  | { type: 'text'; text: string }
  | { type: 'tool_calls'; calls: { id: string; name: string; input: Record<string, unknown> }[] }
  | { type: 'tool_results'; results: { tool_use_id: string; content: string; is_error: boolean }[] }
  | { type: 'plan_update'; plan: Plan }
  | { type: 'notice'; event: string; detail: string }
  | {
      type: 'complete'
      kind: RunKind
      final_text: string
      awaiting?: AwaitingKind
      pending_ask?: PendingAsk
      error?: string
      usage: Usage
      iterations: number
      frame_status: string
      plan?: Plan
      artifacts: Record<string, Artifact>
    }
  | { type: 'error'; message: string }

// ---------- 后端探测 ----------
export interface BackendStatus {
  online: boolean
  llmConfigured: boolean
  model?: string
}

// ---------- 日志 ----------
export type LogLevel = 'debug' | 'info' | 'warn' | 'error'
export type LogSource = 'system' | 'agent'

export interface LogEntry {
  id: number
  ts: string
  level: LogLevel
  source: LogSource
  kind: string
  session_id?: string
  frame_id?: string
  iteration?: number
  message: string
  detail?: Record<string, unknown>
  trace_id?: string
}
