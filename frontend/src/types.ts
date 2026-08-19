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
  revision: number
  overridden_fields: LlmOverriddenField[]
  context_window: number
  has_mcp: boolean
  mcp_count: number
  api_keys_configured: string[]
  default_workspace: string
  host: string
  port: number
}

export interface AppSettingsProvider {
  id: string
  name: string
  base_url: string
  api_key_masked: string
  enabled: boolean
  /** 仅在保存设置时发送；GET /settings 永不返回明文 key。 */
  api_key?: string
  /** 契约外的本地扩展：每个 provider 记录默认模型（Settings 弹层可编辑）。 */
  model?: string
}

export interface A2aAgentInterfaceSummary {
  url: string
  protocol_binding: string
  protocol_version: string
}

export interface A2aAgentCardSummary {
  name: string
  description: string
  version: string
  protocol_version: string
  skills: string[]
  supported_interfaces: A2aAgentInterfaceSummary[]
}

/**
 * A configured outbound A2A peer. Runtime/card fields are observations from
 * the backend and are never written back as authoritative configuration.
 */
export interface A2aAgentSettings {
  id: string
  name: string
  endpoint: string
  enabled: boolean
  timeout_seconds: number
  bearer_token_masked: string
  /** Only present in the single outbound settings save containing a new token. */
  bearer_token?: string
  /** Outbound-only explicit credential removal; never trusted from a server response. */
  clear_bearer_token?: boolean
  status: string
  last_error?: string | null
  last_refreshed_at?: string | null
  tool_name: string
  card_summary?: A2aAgentCardSummary | null
}

export interface A2aAgentSettingsUpdate {
  id: string
  name: string
  endpoint: string
  enabled: boolean
  timeout_seconds: number
  bearer_token?: string
  clear_bearer_token?: boolean
}

export type LlmOverriddenField = 'api_key' | 'base_url' | 'model'

/** Public settings expose only an empty value or the backend's fixed credential mask. */
export type MaskedApiKey = '' | '••••••••'

/** Skill 发现配置：内置开关 + 外部目录纳入 + 自定义目录。 */
export interface SkillSettingsValue {
  /** 被禁用的 skill 名称。 */
  disabled: string[]
  include_claude: boolean
  include_codex: boolean
  include_cursor: boolean
  custom_dirs: string[]
}

/** Persisted reasoning budget for compatible LLM providers. */
export type ThinkingEffort = 'low' | 'high' | 'max'

export interface ThinkingSettingsValue {
  enabled: boolean
  effort: ThinkingEffort
}

export interface AppSettings {
  providers: AppSettingsProvider[]
  /** GET /settings always returns this list; optional keeps older saved mocks loadable. */
  a2a_agents?: A2aAgentSettings[]
  model: string
  default_workspace: string
  /** 兼容旧后端；当前热更新成功时为 false。 */
  restart_required?: boolean
  /** 运行中 LLM 快照的单调版本。 */
  revision: number
  /** 仅暴露覆盖来源，不包含任何环境变量值。 */
  overridden_fields: LlmOverriddenField[]
  /** Skill 发现配置；旧后端可能不返回。 */
  skills?: SkillSettingsValue
  /** MCP server 列表（含连接状态）；旧后端可能不返回。 */
  mcp_servers?: McpServer[]
  /** 数据源 API keys（GET 时只能为空值或固定 mask）；旧后端可能不返回。 */
  api_keys_masked?: Record<string, MaskedApiKey>
  /** 日志保留天数（D-T07）；旧后端可能不返回。 */
  log_retention_days?: number
  /** 日志最大条数（D-T07）；旧后端可能不返回。 */
  log_max_rows?: number
  /** 每次 Agent 运行的模型/工具最大迭代次数；旧后端可能不返回。 */
  max_iterations?: number
  /** Think 开关与单次模型调用的推理强度；旧后端可能不返回。 */
  thinking?: ThinkingSettingsValue
}

/** 可提交字段；运行时 revision/覆盖来源只由后端产生。 */
export interface AppSettingsUpdate {
  providers: AppSettingsProvider[]
  a2a_agents: A2aAgentSettingsUpdate[]
  model: string
  default_workspace: string
  /** Optimistic concurrency guard; stale full-form saves receive HTTP 409. */
  revision: number
  skills?: SkillSettingsValue
  mcp_servers?: McpServerUpdate[]
  /** 数据源 API keys。mask 占位（••••••••）后端保留旧值；空串清除该 key。 */
  api_keys?: Record<string, string>
  /** 日志保留天数（D-T07）。 */
  log_retention_days: number
  /** 日志最大条数（D-T07）。 */
  log_max_rows: number
  /** 每次 Agent 运行的模型/工具最大迭代次数。 */
  max_iterations: number
  /** Think 开关与单次模型调用的推理强度。 */
  thinking: ThinkingSettingsValue
}

// ---------- MCP ----------
export interface McpServer {
  name: string
  url: string
  enabled: boolean
  /** 实时连接状态（仅响应，不回传）。 */
  connected: boolean
  /** 已发现的工具数（仅响应；未连接时为 null/缺省）。 */
  tool_count?: number | null
}

/** MCP server 的可提交字段（连接状态/工具数只由后端产生）。 */
export interface McpServerUpdate {
  name: string
  url: string
  enabled: boolean
}

// ---------- 记忆 ----------
// 对齐后端 dss_db::repo::MemoryRow（完整 Claim Store 字段）。
export interface Memory {
  id: string
  entity: string
  scope: string | null
  entity_type: string
  body: string
  project_id: string | null
  confidence: number
  created_at: string
  updated_at: string
  last_surfaced_at: string | null
  status: string // active | candidate | superseded | expired | deleted
  claim_type: string // fact | preference | decision | procedure | repo | note
  evidence_refs: string | null
  origin: string // auto | explicit | imported
  superseded_by: string | null
  valid_from: string | null
  valid_until: string | null
  deleted_at: string | null
  source_hash: string | null
}

// 记忆生命周期事件（memory_events）。
export interface MemoryEvent {
  id: string
  memory_id: string
  event_type: string // created | approved | rejected | superseded | deleted | surfaced | edited | expired
  actor: string | null
  detail: string | null
  created_at: string
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
  /** 后端持久化的最近会话；打开项目时应优先恢复它。 */
  last_session_id: string | null
  session_count: number
  pinned: boolean
  archived: boolean
  created_at: string
  updated_at: string
}

export interface ProjectDetail extends Project {
  sessions: SessionSummary[]
}

// ---------- Bot Mode ----------
export interface Bot {
  id: string
  name: string
  role: string
  instructions: string
  avatar: string
  color: string
  project_id: string | null
  model: string | null
  thinking_enabled: boolean | null
  thinking_effort: ThinkingEffort | null
  enabled: boolean
  revision: number
  created_at: string
  updated_at: string
}

export interface BotJob {
  id: string
  bot_id: string
  session_id: string
  prompt: string
  requested_plan_mode: boolean
  priority: number
  position: number
  revision: number
  status: 'queued' | 'running' | 'failed' | 'completed'
  run_id: string | null
  last_error: string | null
  created_at: string
  updated_at: string
  claimed_at: string | null
  completed_at: string | null
}

// ---------- Sessions ----------
export type SessionStatus = 'processing' | 'completed' | 'failed' | 'interrupted' | 'awaiting'

export interface SessionSummary {
  id: string
  project_id: string
  title: string
  status: SessionStatus
  live: boolean
  bot_id?: string | null
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
  /** 后端持久化的逐条 assistant token 用量，供刷新后恢复展示。 */
  usage?: Usage | null
  /** 持久化消息在 session 内覆盖到的最后 seq；仅恢复/挂接 run 元数据使用。 */
  source_seq?: number
  /** 新 schema 下消息所属 run；legacy 行可以为空。 */
  source_run_id?: string | null
  /** 结束于本消息后的持久化 run 终态。 */
  run?: SessionRun
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
  /** null when the workspace scan has no persisted creation-frame metadata. */
  frame_id: string | null
  kind: 'markdown' | 'tex' | 'pdf' | 'image' | 'data' | 'other'
  /** `unknown` means the file was found by workspace scan; its origin was not persisted. */
  origin: 'agent' | 'upload' | 'unknown'
  /** null when the backend has not persisted an artifact creation timestamp. */
  created_at: string | null
}

export interface Usage {
  input_tokens: number
  output_tokens: number
  /** DeepSeek 前缀缓存命中 token（可选，缺失视为 0）。 */
  cache_hit_tokens?: number
  /** 前缀缓存未命中 token。 */
  cache_miss_tokens?: number
}

/** 一次用户请求的持久化终态；和 canonical messages 一起按 session id 恢复。 */
export interface SessionRun {
  run_id: string
  ordinal: number
  frame_id: string
  task_summary: string
  plan_mode: boolean
  status: SessionStatus
  kind: RunKind
  awaiting: AwaitingKind
  pending_ask: PendingAsk | null
  error: string | null
  usage: Usage
  iterations: number
  plan: Plan | null
  start_seq: number | null
  end_seq: number | null
  started_at: string
  completed_at: string | null
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
  runs: SessionRun[]
  bot_id?: string | null
}

// ---------- Files ----------
export interface WorkspaceFile {
  path: string
  size: number
  name: string
}

// ---------- Run / Compile ----------
export type RunKind = 'natural' | 'awaiting' | 'max_iters' | 'error' | 'cancelled'
export type AwaitingKind = 'user_response' | 'plan_approval' | 'plan_execution' | null

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
  run_id: string
  prompt: string
  plan_mode?: boolean
  execute_plan?: boolean
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
  | { type: 'draft_reset'; reason: string }
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
      artifacts?: Record<string, Artifact>
    }
  | { type: 'error'; message: string }

// ---------- 后端探测 ----------
export interface BackendStatus {
  online: boolean
  llmConfigured: boolean
  model?: string
  baseUrl?: string
  revision?: number
  overriddenFields?: LlmOverriddenField[]
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
