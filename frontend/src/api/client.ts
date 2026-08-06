// API 客户端：核心用户路径全部走真实后端。
import type {
  AppConfig,
  AppSettings,
  AppSettingsUpdate,
  Artifact,
  BackendStatus,
  HealthResponse,
  LogEntry,
  Memory,
  Plan,
  Project,
  ProjectDetail,
  SessionRun,
  SessionState,
  SessionSummary,
  Skill,
  SSEEvent,
  TemplateInfo,
  WorkspaceFile,
} from "../types";
import {
  attachSessionRuns,
  restoreVisibleMessages,
  type PersistedSessionMessage,
} from "./sessionMessages";
import { buildRunPayload, type StreamRunOptions } from "./sessionRun";
import { withApiToken } from "./auth";
import {
  mockLogs,
  mockTemplates,
} from "../mock/data";

const DEFAULT_BACKEND_PORT = "17896";

/** API 基址：Tauri 注入端口优先，浏览器开发走 Vite 代理（/api）。
 *  生产环境（非 Vite dev）若注入/localStorage 都失败，回退默认 17896。 */
export function apiBase(): string {
  const w = window as unknown as { __BACKEND_PORT__?: number };
  const isDev = (import.meta as any).env?.DEV === true;
  const port =
    w.__BACKEND_PORT__ ??
    localStorage.getItem("dss_backend_port") ??
    (isDev ? null : DEFAULT_BACKEND_PORT);
  return port ? `http://127.0.0.1:${port}/api` : "/api";
}

function apiToken(): string | undefined {
  return (window as unknown as { __DSS_API_TOKEN__?: string })
    .__DSS_API_TOKEN__;
}

/** Every backend fetch, including streaming and binary files, uses the same capability header. */
function apiFetch(input: RequestInfo | URL, init: RequestInit = {}): Promise<Response> {
  return fetch(input, {
    ...init,
    headers: withApiToken(apiToken(), init.headers),
  });
}

/** 统一 fetch 封装：非 ok 抛错；ok 解 JSON。 */
async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const r = await apiFetch(`${apiBase()}${path}`, init);
  if (!r.ok) {
    let msg = `${r.status} ${r.statusText}`;
    try {
      const body = await r.json();
      if (body?.error) msg = String(body.error);
    } catch {
      // 非 JSON 错误体
    }
    throw new Error(msg);
  }
  if (r.status === 204) return undefined as unknown as T;
  return (await r.json()) as T;
}

// ---------- 系统 / 配置 ----------
export async function getHealth(): Promise<HealthResponse> {
  return request<HealthResponse>("/health");
}

export async function getConfig(): Promise<AppConfig> {
  // 后端 GET /api/config 返回运行中 LLM 快照字段；补其余默认值给前端。
  const c = await request<{
    llm_configured: boolean;
    model: string;
    base_url: string;
    revision: number;
    overridden_fields: AppConfig["overridden_fields"];
  }>("/config");
  return {
    llm_configured: c.llm_configured,
    model: c.model,
    base_url: c.base_url,
    revision: c.revision,
    overridden_fields: c.overridden_fields,
    context_window: 128_000,
    has_mcp: false,
    mcp_count: 0,
    api_keys_configured: c.llm_configured ? ["deepseek"] : [],
    default_workspace: "~/deepseek-science",
    host: "127.0.0.1",
    port: 17896,
  };
}

export async function getSettings(): Promise<AppSettings> {
  return request<AppSettings>("/settings");
}

export async function saveSettings(
  settings: AppSettingsUpdate,
): Promise<AppSettings> {
  return request<AppSettings>("/settings", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(settings),
  });
}

// ---------- MCP ----------
// MCP server 配置统一走后端 /api/settings（见 SettingsModal McpSection），不再使用 localStorage。

// ---------- 记忆（Claim Store 治理 API）----------
export async function listMemories(opts?: {
  entity?: string
  project_id?: string
  status?: string
}): Promise<Memory[]> {
  const params = new URLSearchParams();
  if (opts?.entity) params.set("entity", opts.entity);
  if (opts?.project_id) params.set("project_id", opts.project_id);
  if (opts?.status) params.set("status", opts.status);
  const qs = params.toString();
  return request<Memory[]>(`/memories${qs ? "?" + qs : ""}`);
}

export async function createMemory(body: {
  body: string
  scope?: string
  project_id?: string
  claim_type?: string
  confidence?: number
}): Promise<Memory> {
  return request<Memory>("/memories", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
}

export async function getMemory(id: string): Promise<Memory> {
  return request<Memory>(`/memories/${encodeURIComponent(id)}`);
}

export async function editMemory(id: string, body: string): Promise<void> {
  await request<void>(`/memories/${encodeURIComponent(id)}`, {
    method: "PATCH",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ body }),
  });
}

export async function deleteMemory(id: string): Promise<void> {
  // 软删除（status=deleted，保留审计）。
  await request<void>(`/memories/${encodeURIComponent(id)}`, { method: "DELETE" });
}

export async function approveMemory(id: string): Promise<void> {
  await request<void>(`/memories/${encodeURIComponent(id)}/approve`, { method: "POST" });
}

export async function rejectMemory(id: string): Promise<void> {
  await request<void>(`/memories/${encodeURIComponent(id)}/reject`, { method: "POST" });
}

export async function getMemoryHistory(id: string): Promise<import("../types").MemoryEvent[]> {
  return request<import("../types").MemoryEvent[]>(
    `/memories/${encodeURIComponent(id)}/history`,
  );
}

// ---------- Skills / 模板 ----------
export async function listSkills(): Promise<Skill[]> {
  return request<Skill[]>("/skills");
}

export async function listTemplates(): Promise<TemplateInfo[]> {
  try {
    return await request<TemplateInfo[]>("/templates");
  } catch {
    return mockTemplates;
  }
}

export async function getTemplate(templateId: string): Promise<string> {
  try {
    const r = await apiFetch(
      `${apiBase()}/templates/${encodeURIComponent(templateId)}`,
    );
    if (!r.ok) throw new Error(`${r.status}`);
    return await r.text();
  } catch {
    return "\\documentclass{ctexart}\n\\begin{document}\n\\end{document}\n";
  }
}

// ---------- 后端行 → 前端类型映射 ----------

/** 后端 projects 行（dss-db ProjectRow 序列化）。 */
interface ProjectRowBE {
  id: string;
  name: string;
  description: string | null;
  agent_context: string | null;
  last_session_id: string | null;
  archived: boolean;
  created_at: string;
  updated_at: string;
}

function mapProject(r: ProjectRowBE): Project {
  return {
    id: r.id,
    name: r.name,
    description: r.description ?? "",
    agent_context: r.agent_context ?? "",
    last_session_id: r.last_session_id,
    session_count: 0,
    pinned: false,
    archived: r.archived,
    created_at: r.created_at,
    updated_at: r.updated_at,
  };
}

// ---------- Projects ----------
export async function listProjects(archived = false): Promise<Project[]> {
  const rows = await request<ProjectRowBE[]>(
    `/projects?archived=${archived ? "true" : "false"}`,
  );
  return rows.map(mapProject);
}

export async function createProject(input: {
  name: string;
  description?: string;
  agent_context?: string;
}): Promise<Project> {
  const row = await request<ProjectRowBE>("/projects", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      name: input.name,
      description: input.description ?? null,
      agent_context: input.agent_context ?? null,
    }),
  });
  return mapProject(row);
}

export async function updateProject(
  pid: string,
  patch: Partial<Project>,
): Promise<Project> {
  const row = await request<ProjectRowBE>(
    `/projects/${encodeURIComponent(pid)}`,
    {
      method: "PATCH",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        name: patch.name,
        description: patch.description,
        agent_context: patch.agent_context,
      }),
    },
  );
  return mapProject(row);
}

export async function archiveProject(pid: string): Promise<Project> {
  const row = await request<ProjectRowBE>(
    `/projects/${encodeURIComponent(pid)}/archive`,
    {
      method: "POST",
    },
  );
  return mapProject(row);
}

export async function unarchiveProject(pid: string): Promise<Project> {
  const row = await request<ProjectRowBE>(
    `/projects/${encodeURIComponent(pid)}/unarchive`,
    {
      method: "POST",
    },
  );
  return mapProject(row);
}

export async function deleteProject(pid: string, force = false): Promise<void> {
  await request<void>(
    `/projects/${encodeURIComponent(pid)}?force=${force ? "true" : "false"}`,
    {
      method: "DELETE",
    },
  );
}

export async function getProject(pid: string): Promise<ProjectDetail> {
  // 后端返回 { project: ProjectRow, sessions: SessionRow[] }
  const data = await request<{
    project: ProjectRowBE;
    sessions: SessionRowBE[];
  }>(`/projects/${encodeURIComponent(pid)}`);
  const p = mapProject(data.project);
  p.session_count = data.sessions.length;
  return { ...p, sessions: data.sessions.map(mapSession) };
}

// ---------- Sessions ----------
/** 后端 sessions 行（dss-db SessionRow 序列化 + GET /sessions 列表项的 live）。 */
interface SessionRowBE {
  id: string;
  title: string | null;
  workspace: string;
  model: string | null;
  plan_mode?: boolean;
  status: string;
  project_id: string | null;
  live?: boolean;
  created_at: string;
  updated_at: string;
}

function mapSession(r: SessionRowBE): SessionSummary {
  return {
    id: r.id,
    project_id: r.project_id ?? "proj_default",
    title: r.title ?? "New session",
    status: normalizeSessionStatus(r.status),
    live: r.live ?? false,
    created_at: r.created_at,
    updated_at: r.updated_at,
  };
}

export function normalizeSessionStatus(status: string): SessionSummary["status"] {
  const statusMap: Record<string, SessionSummary["status"]> = {
    active: "processing",
    processing: "processing",
    completed: "completed",
    success: "completed",
    failed: "failed",
    error: "failed",
    errored: "failed",
    max_iters: "failed",
    max_iterations: "failed",
    exhausted: "failed",
    timeout: "failed",
    timed_out: "failed",
    aborted: "failed",
    awaiting: "awaiting",
    awaiting_plan_approval: "awaiting",
    awaiting_plan_execution: "awaiting",
    awaiting_user_response: "awaiting",
    cancelled: "interrupted",
    canceled: "interrupted",
    interrupted: "interrupted",
    stopped: "interrupted",
  };
  return statusMap[status.trim().toLowerCase()] ?? "failed";
}

export interface SessionRunBE {
  run_id: string;
  ordinal: number;
  frame_id: string;
  task_summary: string;
  plan_mode: boolean;
  status: string;
  kind: SessionRun["kind"];
  awaiting?: SessionRun["awaiting"];
  pending_ask?: SessionRun["pending_ask"];
  error?: string | null;
  usage?: SessionRun["usage"];
  iterations?: number;
  plan?: Plan | null;
  start_seq?: number | null;
  end_seq?: number | null;
  started_at: string;
  completed_at?: string | null;
}

export function mapSessionRun(run: SessionRunBE): SessionRun {
  const error =
    run.error ??
    (run.kind === "error"
      ? "Agent 执行失败，后端未返回详细原因。"
      : run.kind === "max_iters"
        ? "Agent 达到迭代上限，后端未返回详细原因。"
        : null);
  return {
    run_id: run.run_id,
    ordinal: run.ordinal,
    frame_id: run.frame_id,
    task_summary: run.task_summary,
    plan_mode: run.plan_mode,
    status:
      run.kind === "error" || run.kind === "max_iters" || error
        ? "failed"
        : normalizeSessionStatus(run.status),
    kind: run.kind,
    awaiting: run.awaiting ?? null,
    pending_ask: run.pending_ask ?? null,
    error,
    usage: run.usage ?? { input_tokens: 0, output_tokens: 0 },
    iterations: run.iterations ?? 0,
    plan: run.plan ?? null,
    start_seq: run.start_seq ?? null,
    end_seq: run.end_seq ?? null,
    started_at: run.started_at,
    completed_at: run.completed_at ?? null,
  };
}

export async function listSessions(): Promise<SessionSummary[]> {
  const rows = await request<SessionRowBE[]>("/sessions");
  return rows.map(mapSession);
}

/** 后端探测：GET /api/health + /api/config，判断在线与 LLM 配置状态。 */
export async function probeBackend(): Promise<BackendStatus> {
  try {
    const base = apiBase();
    console.log("[probeBackend] probing", base);
    const h = await apiFetch(`${base}/health`);
    console.log("[probeBackend] health", h.status);
    if (!h.ok) return { online: false, llmConfigured: false };
    const c = await apiFetch(`${base}/config`);
    console.log("[probeBackend] config", c.status);
    const cfg = c.ok ? ((await c.json()) as AppConfig) : null;
    return {
      online: true,
      llmConfigured: cfg?.llm_configured ?? false,
      model: cfg?.model,
      baseUrl: cfg?.base_url,
      revision: cfg?.revision,
      overriddenFields: cfg?.overridden_fields,
    };
  } catch (e) {
    console.error("[probeBackend] failed", e);
    return { online: false, llmConfigured: false };
  }
}

/** 真实建会话：POST /api/sessions → {id, frame_id, model, workspace}。 */
export async function createSessionApi(
  projectId: string,
): Promise<{
  id: string;
  frame_id: string;
  mcp_tools: string[];
  model: string;
  workspace: string;
}> {
  const r = await apiFetch(`${apiBase()}/sessions`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ project_id: projectId }),
  });
  if (!r.ok) throw new Error(`POST /api/sessions failed: ${r.status}`);
  return (await r.json()) as {
    id: string;
    frame_id: string;
    mcp_tools: string[];
    model: string;
    workspace: string;
  };
}

export async function getSession(sid: string): Promise<SessionState> {
  const data = await request<{
    id: string;
    frame_id?: string;
    title: string | null;
    workspace: string;
    model: string | null;
    status: string;
    plan_mode: boolean;
    plan?: Plan | null;
    artifacts?: Record<string, Artifact>;
    project_id: string | null;
    messages: PersistedSessionMessage[];
    runs?: SessionRunBE[];
  }>(`/sessions/${encodeURIComponent(sid)}`);
  const runs = (data.runs ?? []).map(mapSessionRun);
  const messages = attachSessionRuns(restoreVisibleMessages(data.messages), runs);

  return {
    id: data.id,
    frame_id: data.frame_id ?? "",
    status: normalizeSessionStatus(data.status),
    task_summary: data.title ?? "",
    plan_mode: data.plan_mode,
    plan: data.plan ?? null,
    artifacts: data.artifacts ?? {},
    messages,
    runs,
  };
}

export async function deleteSession(sid: string): Promise<void> {
  await request<void>(`/sessions/${encodeURIComponent(sid)}`, {
    method: "DELETE",
  });
}

/** 批准当前挂起的研究计划；执行由下一次显式 execute_plan stream 启动。 */
export async function approvePlan(
  sid: string,
): Promise<{ approved: boolean; steps: Plan["steps"] }> {
  return request<{ approved: boolean; steps: Plan["steps"] }>(
    `/sessions/${encodeURIComponent(sid)}/approve`,
    { method: "POST" },
  );
}

/** Request cancellation and wait until the backend has released the session.
 * `cancelled=false` means normal completion already won the race and its SSE
 * terminal event should be allowed to update the UI. */
export async function cancelSessionRun(
  sid: string,
  runId: string,
): Promise<{ cancelled: boolean }> {
  return request<{ cancelled: boolean }>(
    `/sessions/${encodeURIComponent(sid)}/cancel`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ run_id: runId }),
    },
  );
}

// ---------- Files ----------
export async function listFiles(sid: string): Promise<WorkspaceFile[]> {
  const data = await request<{ files: WorkspaceFile[] }>(
    `/sessions/${encodeURIComponent(sid)}/files`,
  );
  return data.files;
}

/**
 * A workspace directory scan proves only that a file exists. Until artifact
 * provenance is persisted, it must not be attributed to the current frame,
 * an upload, or an Agent run.
 */
export function workspaceFileToArtifact(file: WorkspaceFile): Artifact {
  return {
    path: file.path,
    size: file.size,
    frame_id: null,
    kind: workspaceArtifactKind(file.path),
    origin: "unknown",
    created_at: null,
  };
}

function workspaceArtifactKind(path: string): Artifact["kind"] {
  const ext = path.split(".").pop()?.toLowerCase();
  if (ext === "md") return "markdown";
  if (ext === "tex") return "tex";
  if (ext === "pdf") return "pdf";
  if (["png", "jpg", "jpeg", "gif", "svg", "webp"].includes(ext ?? "")) return "image";
  if (["csv", "tsv", "json", "xlsx", "xls"].includes(ext ?? "")) return "data";
  return "other";
}

function encodeWorkspacePath(path: string): string {
  return path
    .split("/")
    .filter((segment) => segment.length > 0)
    .map(encodeURIComponent)
    .join("/");
}

function fileUrl(sid: string, path: string): string {
  return `${apiBase()}/sessions/${encodeURIComponent(sid)}/files/${encodeWorkspacePath(path)}`;
}

export async function readFile(sid: string, path: string): Promise<string> {
  const r = await apiFetch(fileUrl(sid, path));
  if (!r.ok) throw new Error(`读取文件失败：HTTP ${r.status}`);
  return r.text();
}

export async function readFileBlob(sid: string, path: string): Promise<Blob> {
  const r = await apiFetch(fileUrl(sid, path));
  if (!r.ok) throw new Error(`读取文件失败：HTTP ${r.status}`);
  return r.blob();
}

export async function deleteFile(sid: string, path: string): Promise<void> {
  const r = await apiFetch(fileUrl(sid, path), { method: "DELETE" });
  if (!r.ok) throw new Error(`删除文件失败：HTTP ${r.status}`);
}

export { mockLogs };
export type { LogEntry };

// ---------- 流式 SSE ----------
// POST /api/sessions/{sid}/stream-sse：fetch + ReadableStream 手写 SSE 解析。
// 帧格式：每行 `data: {json}`，帧间以空行（\n\n）分隔；事件类型见 types.ts SSEEvent。
export interface StreamHandlers {
  onStart?: (frameId: string, taskSummary: string) => void;
  onIteration?: (n: number) => void;
  onThinking?: (text: string) => void;
  onText?: (text: string) => void;
  onDraftReset?: (reason: string) => void;
  onToolCalls?: (
    calls: Extract<SSEEvent, { type: "tool_calls" }>["calls"],
  ) => void;
  onToolResults?: (
    results: Extract<SSEEvent, { type: "tool_results" }>["results"],
  ) => void;
  onPlanUpdate?: (plan: Plan) => void;
  onNotice?: (event: string, detail: string) => void;
  onComplete?: (e: Extract<SSEEvent, { type: "complete" }>) => void;
  onError?: (message: string) => void;
}

function dispatchEvent(ev: SSEEvent, h: StreamHandlers) {
  switch (ev.type) {
    case "start":
      h.onStart?.(ev.frame_id, ev.task_summary);
      break;
    case "iteration":
      h.onIteration?.(ev.n);
      break;
    case "thinking":
      h.onThinking?.(ev.text);
      break;
    case "text":
      h.onText?.(ev.text);
      break;
    case "draft_reset":
      h.onDraftReset?.(ev.reason);
      break;
    case "tool_calls":
      h.onToolCalls?.(ev.calls);
      break;
    case "tool_results":
      h.onToolResults?.(ev.results);
      break;
    case "plan_update":
      h.onPlanUpdate?.(ev.plan);
      break;
    case "notice":
      h.onNotice?.(ev.event, ev.detail);
      break;
    case "complete":
      h.onComplete?.(ev);
      break;
    case "error":
      h.onError?.(ev.message);
      break;
  }
}

/** 建立 SSE 流；返回 abort 函数（停止按钮 / 组件卸载时调用）。 */
export function connectSSE(
  sid: string,
  prompt: string,
  handlers: StreamHandlers,
  options: StreamRunOptions = {},
): () => void {
  const ctrl = new AbortController();
  let terminated = false; // 收到 complete/error 事件

  void (async () => {
    try {
      const res = await apiFetch(
        `${apiBase()}/sessions/${encodeURIComponent(sid)}/stream-sse`,
        {
          method: "POST",
          headers: {
            "Content-Type": "application/json",
            Accept: "text/event-stream",
          },
          body: JSON.stringify(buildRunPayload(prompt, options)),
          signal: ctrl.signal,
        },
      );
      if (!res.ok || !res.body) {
        handlers.onError?.(`HTTP ${res.status} ${res.statusText}`.trim());
        return;
      }
      const reader = res.body.getReader();
      const decoder = new TextDecoder();
      let buf = "";
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        buf += decoder.decode(value, { stream: true });
        // 按空行分帧；一帧内可能有多行 data:
        let idx: number;
        while ((idx = buf.indexOf("\n\n")) >= 0) {
          const frame = buf.slice(0, idx);
          buf = buf.slice(idx + 2);
          for (const line of frame.split("\n")) {
            if (!line.startsWith("data:")) continue;
            const payload = line.slice(5).trim();
            if (!payload) continue;
            let ev: SSEEvent;
            try {
              ev = JSON.parse(payload) as SSEEvent;
            } catch {
              continue; // 忽略无法解析的帧
            }
            if (ev.type === "complete" || ev.type === "error") {
              // The first terminal frame owns the run. Mark it before invoking
              // user code so a throwing terminal handler cannot be reported as
              // a second transport error, then stop reading immediately.
              terminated = true;
              const cancelReader = reader.cancel().catch(() => {
                // The peer may already have closed after its terminal frame.
              });
              dispatchEvent(ev, handlers);
              await cancelReader;
              return;
            }
            dispatchEvent(ev, handlers);
          }
        }
      }
      // 契约要求 complete 必达；流提前结束视为错误
      if (!terminated && !ctrl.signal.aborted) {
        handlers.onError?.("连接中断：流在未收到 complete 时结束");
      }
    } catch (e) {
      if (terminated || ctrl.signal.aborted) return; // 已终止 / 用户主动停止，不报错
      handlers.onError?.(e instanceof Error ? e.message : String(e));
    }
  })();

  return () => ctrl.abort();
}

// ---------- 日志 ----------

export interface LogQuery {
  session_id?: string;
  source?: string;
  /** 逗号分隔多值，如 "warn,error" */
  level?: string;
  kind?: string;
  since?: string;
  until?: string;
  limit?: number;
  offset?: number;
}

export async function listLogs(
  query: LogQuery = {},
): Promise<{ logs: LogEntry[]; total: number }> {
  const qs = new URLSearchParams();
  if (query.session_id) qs.set("session_id", query.session_id);
  if (query.source) qs.set("source", query.source);
  if (query.level) qs.set("level", query.level);
  if (query.kind) qs.set("kind", query.kind);
  if (query.since) qs.set("since", query.since);
  if (query.until) qs.set("until", query.until);
  if (query.limit != null) qs.set("limit", String(query.limit));
  if (query.offset != null) qs.set("offset", String(query.offset));
  const q = qs.toString();
  try {
    return await request<{ logs: LogEntry[]; total: number }>(
      `/logs${q ? `?${q}` : ""}`,
    );
  } catch {
    return { logs: [], total: 0 };
  }
}

export async function getLog(id: number): Promise<LogEntry | null> {
  try {
    return await request<LogEntry>(`/logs/${id}`);
  } catch {
    return null;
  }
}

export async function clearLogs(before?: string): Promise<void> {
  const q = before ? `?before=${encodeURIComponent(before)}` : "";
  try {
    await request<{ deleted: number }>(`/logs${q}`, { method: "DELETE" });
  } catch {
    /* ignore */
  }
}
