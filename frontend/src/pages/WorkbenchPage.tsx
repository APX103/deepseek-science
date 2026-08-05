// 工作台页：三栏布局（左右栏宽可拖拽并持久化）+ 右侧 tab 系统 + 文件预览弹层 + Skills 弹层。
import { useCallback, useEffect, useMemo, useState } from 'react'
import { useParams } from 'react-router-dom'
import type { Artifact, WorkspaceFile } from '../types'
import {
  approvePlan,
  cancelSessionRun,
  connectSSE,
  deleteFile,
  listFiles,
  workspaceFileToArtifact,
} from '../api/client'
import {
  canExecutePlanNow,
  composerSendIntent,
  currentAwaitingKind,
} from '../api/planExecution'
import {
  appendStreamText,
  appendStreamThinking,
  appendStreamToolCall,
  appendStreamToolResult,
  advanceStreamIteration,
  resetStreamDraft,
  beginStreamStop,
  completeStream,
  failStream,
  failStreamStop,
  finishStreamStop,
  loadFromBackend,
  loadMessages,
  resumeStreamAfterLateStop,
  sendUserMessage,
  setStreamAborter,
  setStreamPlan,
  setStreamStart,
  startStream,
  useMessages,
  useSession,
  useSessionState,
  useStream,
} from '../store'
import FilePreviewModal from '../components/FilePreviewModal'
import PdfPreviewModal from '../components/PdfPreviewModal'
import SkillsModal from '../components/SkillsModal'
import ArtifactPanel, { DEFAULT_TABS, FILES_TAB, type PanelTab } from '../components/workbench/ArtifactPanel'
import ChatArea from '../components/workbench/ChatArea'
import ResizeHandle from '../components/workbench/ResizeHandle'
import Sidebar from '../components/workbench/Sidebar'

const LEFT_KEY = 'dss_left_w'
const RIGHT_KEY = 'dss_right_w'

function readWidth(key: string, fallback: number): number {
  const v = Number(localStorage.getItem(key))
  return Number.isFinite(v) && v > 0 ? v : fallback
}

export default function WorkbenchPage() {
  const { pid = '', sid = '' } = useParams()
  const [showSkills, setShowSkills] = useState(false)
  const [previewFile, setPreviewFile] = useState<WorkspaceFile | null>(null)
  const [files, setFiles] = useState<WorkspaceFile[]>([])
  const [filesLoading, setFilesLoading] = useState(false)
  const [filesError, setFilesError] = useState<string | null>(null)
  const [planMode, setPlanMode] = useState(false)
  const [approvingPlan, setApprovingPlan] = useState(false)
  const [planError, setPlanError] = useState<string | null>(null)

  // 栏宽：localStorage 持久化
  const [leftW, setLeftW] = useState(() => readWidth(LEFT_KEY, 224))
  const [rightW, setRightW] = useState(() => readWidth(RIGHT_KEY, 420))
  useEffect(() => localStorage.setItem(LEFT_KEY, String(leftW)), [leftW])
  useEffect(() => localStorage.setItem(RIGHT_KEY, String(rightW)), [rightW])

  // 右栏 tab 状态（可全部关闭；全部关完显示浏览视图）
  const [tabs, setTabs] = useState<PanelTab[]>(DEFAULT_TABS)
  const [activeTab, setActiveTab] = useState<string | null>(DEFAULT_TABS[0]?.id ?? null)

  const openTab = (t: PanelTab) => {
    setTabs((ts) => (ts.some((x) => x.id === t.id) ? ts : [...ts, t]))
    setActiveTab(t.id)
  }
  const closeTab = (id: string) => {
    setTabs((ts) => {
      const next = ts.filter((t) => t.id !== id)
      if (activeTab === id) setActiveTab(next.length > 0 ? next[next.length - 1].id : null)
      return next
    })
  }
  // 会话消息（store 驱动；新会话为空态）
  const session = useSession(sid)
  const sessionState = useSessionState(sid)
  const messages = useMessages(sid)
  const stream = useStream(sid)
  // A live stream owns the plan snapshot even when that snapshot is explicitly
  // null (ordinary prompts clear stale approved plans).
  const plan = stream ? stream.plan : sessionState?.plan ?? null
  const awaiting = currentAwaitingKind(stream, sessionState?.runs)
  const awaitingPlan = awaiting === 'plan_approval'
  const canExecutePlan = canExecutePlanNow(plan, awaiting, stream?.running ?? false)

  const refreshFiles = useCallback(async () => {
    if (!sid) return
    setFilesLoading(true)
    setFilesError(null)
    try {
      setFiles(await listFiles(sid))
    } catch (error) {
      setFiles([])
      setFilesError(error instanceof Error ? error.message : String(error))
    } finally {
      setFilesLoading(false)
    }
  }, [sid])

  const artifacts = useMemo<Artifact[]>(
    () => files.map(workspaceFileToArtifact),
    [files],
  )

  const openArtifact = (artifact: Artifact) => {
    const file = files.find((candidate) => candidate.path === artifact.path)
    if (file) setPreviewFile(file)
    else setFilesError(`工作区中找不到文件：${artifact.path}`)
  }

  // 进入会话时：刷新侧栏的 projects/sessions，并从后端恢复当前会话历史。
  useEffect(() => {
    void loadFromBackend()
    setTabs(DEFAULT_TABS)
    setActiveTab(DEFAULT_TABS[0]?.id ?? null)
    setPreviewFile(null)
    setPlanError(null)
    if (sid) {
      void loadMessages(sid)
      void refreshFiles()
    }
  }, [sid, refreshFiles])

  // 发送：用户气泡立即上屏，然后走 SSE 流（POST /api/sessions/{sid}/stream-sse）
  const handleSend = (
    text: string,
    requestedPlanMode = planMode,
    executePlan = false,
  ) => {
    if (!sid || stream?.running) return
    setPlanError(null)
    sendUserMessage(sid, text)
    const runId = startStream(sid, executePlan, requestedPlanMode)
    const abort = connectSSE(sid, text, {
      onStart: (frameId, taskSummary) => setStreamStart(sid, frameId, taskSummary, runId),
      onIteration: (iteration) => advanceStreamIteration(sid, iteration, runId),
      onThinking: (t) => appendStreamThinking(sid, t, runId),
      onText: (t) => appendStreamText(sid, t, runId),
      onDraftReset: () => resetStreamDraft(sid, runId),
      onToolCalls: (calls) => appendStreamToolCall(sid, calls, runId),
      onToolResults: (results) => {
        if (appendStreamToolResult(sid, results, runId)) void refreshFiles()
      },
      onPlanUpdate: (nextPlan) => setStreamPlan(sid, nextPlan, runId),
      onComplete: (e) => {
        const accepted = completeStream(
          sid,
          e.usage ?? null,
          e.iterations ?? 0,
          e.kind,
          e.pending_ask ?? null,
          e.awaiting ?? null,
          e.plan ?? null,
          e.error ?? null,
          e.artifacts,
          runId,
        )
        if (accepted) {
          void refreshFiles()
          // Terminal SSE is released only after the backend transaction commits.
          // Re-read the authoritative DB transcript so live and restored ordering match.
          void loadMessages(sid)
        }
      },
      onError: (m) => {
        if (failStream(sid, m, runId)) {
          void refreshFiles()
          void loadMessages(sid)
        }
      },
    }, { planMode: requestedPlanMode, executePlan, runId })
    setStreamAborter(sid, abort, runId)
  }

  const handleApprovePlan = async () => {
    if (!sid || approvingPlan) return
    setApprovingPlan(true)
    setPlanError(null)
    try {
      const approved = await approvePlan(sid)
      setStreamPlan(sid, { approved: approved.approved, steps: approved.steps })
      setPlanMode(false)
      handleSend('请按照已批准的计划开始执行。', false, true)
    } catch (error) {
      setPlanError(`批准计划失败：${error instanceof Error ? error.message : String(error)}`)
    } finally {
      setApprovingPlan(false)
    }
  }

  const handleExecutePlan = () => {
    if (!canExecutePlan) return
    setPlanMode(false)
    handleSend('请按照已批准的计划开始执行。', false, true)
  }

  const handleComposerSend = (text: string) => {
    const intent = composerSendIntent(awaiting, plan, planMode)
    handleSend(text, intent.planMode, intent.executePlan)
  }

  const handleStop = async () => {
    if (!sid) return
    const runId = beginStreamStop(sid)
    if (!runId) return
    try {
      const result = await cancelSessionRun(sid, runId)
      if (result.cancelled) {
        if (finishStreamStop(sid, runId)) void loadMessages(sid)
      }
      else resumeStreamAfterLateStop(sid, runId)
    } catch (error) {
      failStreamStop(sid, error instanceof Error ? error.message : String(error), runId)
    }
  }

  const handleDeleteFile = async (file: WorkspaceFile) => {
    if (!window.confirm(`确定删除工作区文件“${file.path}”吗？此操作无法撤销。`)) return
    await deleteFile(sid, file.path)
    if (previewFile?.path === file.path) setPreviewFile(null)
    await refreshFiles()
  }

  return (
    <div className="flex h-full">
      <Sidebar
        pid={pid}
        sid={sid}
        width={leftW}
        onOpenSkills={() => setShowSkills(true)}
        onOpenFiles={() => openTab(FILES_TAB)}
      />
      <ResizeHandle side="left" value={leftW} min={200} max={360} onChange={setLeftW} />

      <ChatArea
        messages={messages}
        failed={session?.status === 'failed'}
        stream={stream}
        plan={plan}
        awaitingPlan={awaitingPlan}
        canExecutePlan={canExecutePlan}
        approvingPlan={approvingPlan}
        planError={planError}
        planMode={planMode}
        onPlanModeChange={setPlanMode}
        onApprovePlan={() => void handleApprovePlan()}
        onExecutePlan={handleExecutePlan}
        onSend={handleComposerSend}
        onStop={() => void handleStop()}
      />

      <ResizeHandle side="right" value={rightW} min={360} max={760} onChange={setRightW} />
      <div className="shrink-0 border-l border-border" style={{ width: rightW }}>
        <ArtifactPanel
          artifacts={artifacts}
          files={files}
          filesLoading={filesLoading}
          filesError={filesError}
          taskLabel={session?.title ?? 'Session artifacts'}
          tabs={tabs}
          activeTab={activeTab}
          onSelectTab={setActiveTab}
          onCloseTab={closeTab}
          onOpenArtifact={openArtifact}
          onPreviewFile={setPreviewFile}
          onDeleteFile={handleDeleteFile}
        />
      </div>

      {showSkills && <SkillsModal onClose={() => setShowSkills(false)} />}
      {previewFile &&
        (previewFile.path.endsWith('.pdf') ? (
          <PdfPreviewModal
            sid={sid}
            artifact={{
              path: previewFile.path,
              size: previewFile.size,
              frame_id: null,
              kind: 'pdf',
              origin: 'unknown',
              created_at: null,
            }}
            onClose={() => setPreviewFile(null)}
          />
        ) : (
          <FilePreviewModal sid={sid} file={previewFile} onClose={() => setPreviewFile(null)} />
        ))}
    </div>
  )
}
