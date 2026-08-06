import type { ContentBlock, Message, SessionRun, Usage } from "../types";
import { sanitizeAssistantDisplayText } from "./assistantProtocol";

export interface PersistedSessionMessage {
  seq?: number | null;
  run_id?: string | null;
  role: string;
  content: unknown;
  harness_notice?: boolean | null;
}

type VisibleRole = "user" | "assistant" | "tool";
type ToolResultBlock = Extract<ContentBlock, { type: "tool_result" }>;

interface PersistedToolResult {
  block: ToolResultBlock;
  seq?: number;
}

interface PersistedMessageObject {
  role?: unknown;
  content?: unknown;
  tool_calls?: unknown;
  tool_call_id?: unknown;
  is_error?: unknown;
  reasoning_content?: unknown;
  usage?: unknown;
  harness_notice?: unknown;
  internal?: unknown;
  _internal?: unknown;
  metadata?: unknown;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function asMessageObject(value: unknown): PersistedMessageObject | null {
  return isRecord(value) ? value : null;
}

function normalizeVisibleRole(value: unknown): VisibleRole | null {
  if (typeof value !== "string") return null;
  const role = value.trim().toLowerCase();
  return role === "user" || role === "assistant" || role === "tool"
    ? role
    : null;
}

function isInternalObject(obj: PersistedMessageObject): boolean {
  if (obj.harness_notice === true || obj.internal === true || obj._internal === true) {
    return true;
  }
  return isRecord(obj.metadata) && obj.metadata.internal === true;
}

/**
 * Returns the only role that is safe to restore into the visible transcript.
 *
 * The database row role and serialized ChatMessage role must agree when both
 * exist. A mismatch is filtered instead of coercing an internal/system row to
 * `assistant`, which is the privacy-safe failure mode for corrupt/old data.
 */
function visibleRole(row: PersistedSessionMessage): VisibleRole | null {
  if (row.harness_notice) return null;
  const outerRole = normalizeVisibleRole(row.role);
  if (!outerRole) return null;

  const obj = asMessageObject(row.content);
  if (!obj) return outerRole;
  if (isInternalObject(obj)) return null;
  if (obj.role === undefined) return outerRole;

  const innerRole = normalizeVisibleRole(obj.role);
  return innerRole === outerRole ? outerRole : null;
}

function sanitizeUsage(value: unknown): Usage | null {
  if (!isRecord(value)) return null;
  const input = value.input_tokens;
  const output = value.output_tokens;
  if (
    typeof input !== "number" ||
    !Number.isFinite(input) ||
    input < 0 ||
    typeof output !== "number" ||
    !Number.isFinite(output) ||
    output < 0
  ) {
    return null;
  }
  return {
    input_tokens: input,
    output_tokens: output,
    cache_hit_tokens: finiteNonNegative(value.cache_hit_tokens),
    cache_miss_tokens: finiteNonNegative(value.cache_miss_tokens),
  };
}

function finiteNonNegative(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : 0;
}

function sanitizeBlock(value: unknown): ContentBlock | null {
  if (!isRecord(value) || typeof value.type !== "string") return null;
  switch (value.type) {
    case "thinking":
      return typeof value.thinking === "string"
        ? { type: "thinking", thinking: sanitizeAssistantDisplayText(value.thinking) }
        : null;
    case "text":
      return typeof value.text === "string" ? { type: "text", text: value.text } : null;
    case "tool_use":
      return typeof value.id === "string" && typeof value.name === "string"
        ? {
            type: "tool_use",
            id: value.id,
            name: value.name,
            input: isRecord(value.input) ? { ...value.input } : {},
          }
        : null;
    case "tool_result":
      return typeof value.tool_use_id === "string" && typeof value.content === "string"
        ? {
            type: "tool_result",
            tool_use_id: value.tool_use_id,
            content: value.content,
            is_error: value.is_error === true,
          }
        : null;
    default:
      return null;
  }
}

function parseToolInput(value: unknown): Record<string, unknown> {
  if (isRecord(value)) return { ...value };
  if (typeof value !== "string") return {};
  try {
    const parsed: unknown = JSON.parse(value || "{}");
    return isRecord(parsed) ? parsed : { _raw: value };
  } catch {
    return { _raw: value };
  }
}

function toolKey(runId: string | null | undefined, callId: string): string {
  return `${runId ?? "legacy"}\u0000${callId}`;
}

function takeToolResult(
  toolResults: Map<string, PersistedToolResult[]>,
  runId: string | null | undefined,
  callId: string,
): PersistedToolResult | undefined {
  const queue = toolResults.get(toolKey(runId, callId));
  return queue?.shift();
}

function appendObjectToolCalls(
  blocks: ContentBlock[],
  value: unknown,
  toolResults: Map<string, PersistedToolResult[]>,
  runId: string | null | undefined,
): number | undefined {
  if (!Array.isArray(value)) return undefined;
  let resultSeq: number | undefined;
  for (const candidate of value) {
    if (!isRecord(candidate) || typeof candidate.id !== "string") continue;
    const fn = isRecord(candidate.function) ? candidate.function : null;
    if (!fn || typeof fn.name !== "string") continue;
    const call: ContentBlock = {
      type: "tool_use",
      id: candidate.id,
      name: fn.name,
      input: parseToolInput(fn.arguments),
    };
    blocks.push(call);
    const result = takeToolResult(toolResults, runId, candidate.id);
    if (result) {
      blocks.push(result.block);
      if (result.seq !== undefined) resultSeq = Math.max(resultSeq ?? result.seq, result.seq);
    }
  }
  return resultSeq;
}

function collectToolResults(
  rows: PersistedSessionMessage[],
): Map<string, PersistedToolResult[]> {
  const results = new Map<string, PersistedToolResult[]>();
  const append = (row: PersistedSessionMessage, block: ToolResultBlock) => {
    const key = toolKey(row.run_id, block.tool_use_id);
    const queue = results.get(key) ?? [];
    queue.push({
      block,
      seq: typeof row.seq === "number" && Number.isFinite(row.seq) ? row.seq : undefined,
    });
    results.set(key, queue);
  };
  for (const row of rows) {
    const role = visibleRole(row);
    if (role !== "assistant" && role !== "tool") continue;

    if (Array.isArray(row.content)) {
      for (const candidate of row.content) {
        const block = sanitizeBlock(candidate);
        if (block?.type === "tool_result") append(row, block);
      }
      continue;
    }

    if (role !== "tool") continue;
    const obj = asMessageObject(row.content);
    const toolUseId = obj?.tool_call_id;
    const content = obj?.content;
    if (typeof toolUseId === "string" && typeof content === "string") {
      append(row, {
        type: "tool_result",
        tool_use_id: toolUseId,
        content,
        is_error: obj?.is_error === true,
      });
    }
  }
  return results;
}

function withSource(message: Message, row: PersistedSessionMessage, sourceSeq?: number): Message {
  if (sourceSeq !== undefined && Number.isFinite(sourceSeq)) message.source_seq = sourceSeq;
  if (row.run_id !== undefined) message.source_run_id = row.run_id;
  return message;
}

function restoreUserMessage(row: PersistedSessionMessage): Message | null {
  if (typeof row.content === "string") {
    return row.content.length > 0
      ? withSource({ role: "user", content: row.content }, row, row.seq ?? undefined)
      : null;
  }
  if (Array.isArray(row.content)) {
    const text = row.content
      .map(sanitizeBlock)
      .filter((block): block is Extract<ContentBlock, { type: "text" }> => block?.type === "text")
      .map((block) => block.text)
      .join("");
    return text.length > 0
      ? withSource({ role: "user", content: text }, row, row.seq ?? undefined)
      : null;
  }
  const obj = asMessageObject(row.content);
  return typeof obj?.content === "string" && obj.content.length > 0
    ? withSource({ role: "user", content: obj.content }, row, row.seq ?? undefined)
    : null;
}

function restoreAssistantMessage(
  row: PersistedSessionMessage,
  toolResults: Map<string, PersistedToolResult[]>,
): Message | null {
  const blocks: ContentBlock[] = [];
  let usage: Usage | null = null;
  let sourceSeq = typeof row.seq === "number" && Number.isFinite(row.seq) ? row.seq : undefined;

  if (typeof row.content === "string") {
    if (row.content.length > 0) {
      blocks.push({ type: "text", text: sanitizeAssistantDisplayText(row.content) });
    }
  } else if (Array.isArray(row.content)) {
    for (const candidate of row.content) {
      const restoredBlock = sanitizeBlock(candidate);
      const block =
        restoredBlock?.type === "text"
          ? { ...restoredBlock, text: sanitizeAssistantDisplayText(restoredBlock.text) }
          : restoredBlock;
      if (!block || block.type === "tool_result") continue;
      blocks.push(block);
      if (block.type === "tool_use") {
        const result = takeToolResult(toolResults, row.run_id, block.id);
        if (result) {
          blocks.push(result.block);
          if (result.seq !== undefined) sourceSeq = Math.max(sourceSeq ?? result.seq, result.seq);
        }
      }
    }
  } else {
    const obj = asMessageObject(row.content);
    if (typeof obj?.reasoning_content === "string" && obj.reasoning_content.length > 0) {
      blocks.push({
        type: "thinking",
        thinking: sanitizeAssistantDisplayText(obj.reasoning_content),
      });
    }
    if (typeof obj?.content === "string" && obj.content.length > 0) {
      blocks.push({ type: "text", text: sanitizeAssistantDisplayText(obj.content) });
    }
    const resultSeq = appendObjectToolCalls(blocks, obj?.tool_calls, toolResults, row.run_id);
    if (resultSeq !== undefined) sourceSeq = Math.max(sourceSeq ?? resultSeq, resultSeq);
    usage = sanitizeUsage(obj?.usage);
  }

  if (blocks.length === 0) return null;
  const content: string | ContentBlock[] =
    blocks.length === 1 && blocks[0].type === "text" ? blocks[0].text : blocks;
  return withSource({ role: "assistant", content, usage }, row, sourceSeq);
}

/**
 * Restores the user-visible transcript from persisted OpenAI-shaped rows.
 * System, developer, harness, internal, malformed-role, and unknown metadata
 * rows are discarded. Normal user/assistant messages and paired tool results
 * are rebuilt without carrying persistence-only metadata into React state.
 */
export function restoreVisibleMessages(rows: PersistedSessionMessage[]): Message[] {
  const toolResults = collectToolResults(rows);
  const messages: Message[] = [];
  for (const row of rows) {
    const role = visibleRole(row);
    if (role === "user") {
      const message = restoreUserMessage(row);
      if (message) messages.push(message);
    } else if (role === "assistant") {
      const message = restoreAssistantMessage(row, toolResults);
      if (message) messages.push(message);
    }
  }
  return messages;
}

/** Attach each persisted run footer to the last visible message in its seq range. */
export function attachSessionRuns(messages: Message[], runs: SessionRun[]): Message[] {
  if (runs.length === 0 || messages.length === 0) return messages;
  const restored = messages.map((message) => ({ ...message }));
  for (const run of [...runs].sort((a, b) => a.ordinal - b.ordinal)) {
    if (run.end_seq == null) continue;
    const lower = run.start_seq ?? 0;
    let target = -1;
    for (let index = 0; index < restored.length; index += 1) {
      const message = restored[index];
      if (message.source_seq == null) continue;
      if (message.source_seq < lower || message.source_seq > run.end_seq) continue;
      if (message.source_run_id && message.source_run_id !== run.run_id) continue;
      target = index;
    }
    if (target >= 0) restored[target] = { ...restored[target], run };
  }
  return restored;
}
