export const A2A_TOOL_RESULT_SCHEMA = 'dss.a2a.tool-result.v1' as const

export type JsonRecord = Record<string, unknown>

export interface A2aResponseFrame extends JsonRecord {
  sequence: number
  operation: string
  received_at: string
  payload: unknown
}

export interface A2aToolResultEnvelope extends JsonRecord {
  schema: typeof A2A_TOOL_RESULT_SCHEMA
  agent: JsonRecord
  /** Present only when the remote Agent was selected through a Registry Resource. */
  registry: JsonRecord | null
  /** Absent when the mandatory pre-call Agent Card refresh itself failed. */
  card: JsonRecord | null
  request: JsonRecord
  responses: A2aResponseFrame[]
  terminal: JsonRecord
  warnings: unknown[]
}

export type A2aTaskInterruption = 'input_required' | 'auth_required' | 'interrupted'

export type A2aSemanticNode =
  | { kind: 'message'; value: JsonRecord }
  | { kind: 'task'; value: JsonRecord }
  | { kind: 'status'; value: JsonRecord }
  | { kind: 'artifact'; value: JsonRecord }

export function isJsonRecord(value: unknown): value is JsonRecord {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

/**
 * Parse only the canonical, versioned A2A envelope. Any other JSON — including
 * a future schema — deliberately falls back to ToolCallCard's generic view.
 */
export function parseA2aToolResult(content: string): A2aToolResultEnvelope | null {
  let value: unknown
  try {
    value = JSON.parse(content)
  } catch {
    return null
  }
  if (!isJsonRecord(value) || value.schema !== A2A_TOOL_RESULT_SCHEMA) return null
  const card = value.card === undefined || value.card === null
    ? null
    : isJsonRecord(value.card) ? value.card : undefined
  const registry = value.registry === undefined || value.registry === null
    ? null
    : isJsonRecord(value.registry) ? value.registry : undefined
  if (
    !isJsonRecord(value.agent)
    || registry === undefined
    || (registry !== null && (
      typeof registry.server !== 'string'
      || typeof registry.resource_uri !== 'string'
      || typeof registry.resource_name !== 'string'
    ))
    || card === undefined
    || !isJsonRecord(value.request)
    || !Array.isArray(value.responses)
    || !isJsonRecord(value.terminal)
    || (value.warnings !== undefined && !Array.isArray(value.warnings))
  ) {
    return null
  }

  const responses: A2aResponseFrame[] = []
  for (const frame of value.responses) {
    if (
      !isJsonRecord(frame)
      || !Number.isFinite(frame.sequence)
      || typeof frame.operation !== 'string'
      || typeof frame.received_at !== 'string'
      || !('payload' in frame)
    ) {
      return null
    }
    responses.push(frame as A2aResponseFrame)
  }

  return {
    ...value,
    schema: A2A_TOOL_RESULT_SCHEMA,
    agent: value.agent,
    registry,
    card,
    request: value.request,
    responses,
    terminal: value.terminal,
    warnings: value.warnings ?? [],
  } as A2aToolResultEnvelope
}

/**
 * Classify a resumable A2A Task interruption. The state-based branches intentionally also
 * recognize envelopes persisted by builds that represented these states as
 * `kind=task, success=false`.
 */
export function a2aTaskInterruption(
  result: A2aToolResultEnvelope,
): A2aTaskInterruption | null {
  const kind = typeof result.terminal.kind === 'string' ? result.terminal.kind : ''
  const state = typeof result.terminal.state === 'string' ? result.terminal.state : ''
  if (kind !== 'task' && kind !== 'task_interrupted') return null
  if (state === 'TASK_STATE_AUTH_REQUIRED' || state === 'auth-required') {
    return 'auth_required'
  }
  if (state === 'TASK_STATE_INPUT_REQUIRED' || state === 'input-required') {
    return 'input_required'
  }
  return kind === 'task_interrupted' ? 'interrupted' : null
}

function looksLikeMessage(value: JsonRecord): boolean {
  const kind = typeof value.kind === 'string' ? value.kind.toLowerCase() : ''
  return kind === 'message'
    || ((Array.isArray(value.parts) || Array.isArray(value.content))
      && (typeof value.messageId === 'string'
        || typeof value.message_id === 'string'
        || typeof value.role === 'string'))
}

function looksLikeTask(value: JsonRecord): boolean {
  const kind = typeof value.kind === 'string' ? value.kind.toLowerCase() : ''
  return kind === 'task'
    || (isJsonRecord(value.status)
      && (typeof value.id === 'string' || typeof value.taskId === 'string' || typeof value.task_id === 'string'))
}

function looksLikeStatus(value: JsonRecord): boolean {
  const kind = typeof value.kind === 'string' ? value.kind.toLowerCase() : ''
  return kind === 'status-update'
    || kind === 'statusupdate'
    || (isJsonRecord(value.status)
      && (typeof value.taskId === 'string' || typeof value.task_id === 'string')
      && !Array.isArray(value.artifacts))
}

function looksLikeArtifact(value: JsonRecord): boolean {
  const kind = typeof value.kind === 'string' ? value.kind.toLowerCase() : ''
  return kind === 'artifact-update'
    || kind === 'artifactupdate'
    || (Array.isArray(value.parts)
      && (typeof value.artifactId === 'string' || typeof value.artifact_id === 'string'))
}

/**
 * Locate the semantic A2A payload inside v1/v0.3 JSON-RPC and HTTP+JSON
 * wrappers. Rendering remains lossless because the complete frame is always
 * shown separately as raw JSON.
 */
export function semanticNodesForPayload(payload: unknown): A2aSemanticNode[] {
  const nodes: A2aSemanticNode[] = []
  const visited = new Set<unknown>()

  const add = (kind: A2aSemanticNode['kind'], value: unknown): boolean => {
    if (!isJsonRecord(value)) return false
    nodes.push({ kind, value } as A2aSemanticNode)
    return true
  }

  const visit = (value: unknown, depth: number) => {
    if (depth > 8 || visited.has(value)) return
    if (typeof value === 'string' && value.trimStart().startsWith('{')) {
      try {
        visit(JSON.parse(value), depth + 1)
      } catch {
        // Raw non-JSON response text remains visible in the frame inspector.
      }
      return
    }
    if (!isJsonRecord(value)) return
    visited.add(value)

    if ('result' in value) {
      visit(value.result, depth + 1)
      return
    }
    if ('body' in value && (isJsonRecord(value.body) || typeof value.body === 'string')) {
      visit(value.body, depth + 1)
      return
    }

    let matchedWrapper = false
    if ('message' in value) matchedWrapper = add('message', value.message) || matchedWrapper
    if ('task' in value) matchedWrapper = add('task', value.task) || matchedWrapper
    if ('statusUpdate' in value) matchedWrapper = add('status', value.statusUpdate) || matchedWrapper
    if ('status_update' in value) matchedWrapper = add('status', value.status_update) || matchedWrapper
    if ('artifactUpdate' in value) matchedWrapper = add('artifact', value.artifactUpdate) || matchedWrapper
    if ('artifact_update' in value) matchedWrapper = add('artifact', value.artifact_update) || matchedWrapper
    if (matchedWrapper) return

    if (looksLikeStatus(value)) {
      add('status', value)
    } else if (looksLikeArtifact(value)) {
      add('artifact', value)
    } else if (looksLikeTask(value)) {
      add('task', value)
    } else if (looksLikeMessage(value)) {
      add('message', value)
    }
  }

  visit(payload, 0)
  return nodes
}

export function textField(record: JsonRecord | null | undefined, ...names: string[]): string {
  if (!record) return ''
  for (const name of names) {
    if (typeof record[name] === 'string') return record[name] as string
  }
  return ''
}

export function recordField(record: JsonRecord | null | undefined, ...names: string[]): JsonRecord | null {
  if (!record) return null
  for (const name of names) {
    if (isJsonRecord(record[name])) return record[name] as JsonRecord
  }
  return null
}

export function arrayField(record: JsonRecord | null | undefined, ...names: string[]): unknown[] {
  if (!record) return []
  for (const name of names) {
    if (Array.isArray(record[name])) return record[name] as unknown[]
  }
  return []
}

export function prettyJson(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2) ?? String(value)
  } catch {
    return String(value)
  }
}
