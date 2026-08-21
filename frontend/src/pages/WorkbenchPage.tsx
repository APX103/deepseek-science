// 工作台页：三栏布局（左右栏宽可拖拽并持久化）+ 右侧 tab 系统 + 文件预览弹层 + Skills 弹层。
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useParams } from 'react-router-dom'
import type { Artifact, WorkspaceFile } from '../types'
import {
  approvePlan,
  claimBotJob,
  cancelSessionRun,
  connectSSE,
  deleteFile,
  deleteBotJob,
  editBotJob,
  enqueueBotJob,
  finishBotJob,
  listBotJobs,
  listFiles,
  reorderBotJobs,
  workspaceFileToArtifact,
} from '../api/client'
import {
  canExecutePlanNow,
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
  getSessionStateSnapshot,
  getStreamSnapshot,
  loadFromBackend,
  loadMessages,
  retireStreamAfterBackendFinish,
  useProjects,
  useBots,
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
import {
  beginQueuedPromptSteering,
  claimNextQueuedPrompt,
  claimQueuedPrompt,
  clearQueuedPromptSteering,
  deleteQueuedPrompt,
  editQueuedPrompt,
  enqueuePrompt,
  getPromptQueue,
  reorderQueuedPrompt,
  replacePromptQueue,
  usePromptQueue,
} from '../promptQueueStore'
import type { QueuedPrompt } from '../api/promptQueue'
import {
  coordinateManualPromptStop,
  coordinateQueuedPromptSteer,
  promptRunGate,
  resolvePromptRunIntent,
  restoreAndMaybeDrainPromptQueue,
  type PromptRunLease,
} from '../api/promptRunCoordinator'
import { filePreviewKind } from '../api/filePreview'
import FilePreviewModal from '../components/FilePreviewModal'
import ImagePreviewModal from '../components/ImagePreviewModal'
import PdfPreviewModal from '../components/PdfPreviewModal'
import SkillsModal from '../components/SkillsModal'
import ArtifactPanel, { DEFAULT_TABS, FILES_TAB, type PanelTab } from '../components/workbench/ArtifactPanel'
import ChatArea from '../components/workbench/ChatArea'
import ResizeHandle from '../components/workbench/ResizeHandle'
import Sidebar from '../components/workbench/Sidebar'

const LEFT_KEY = 'dss_left_w'
const RIGHT_KEY = 'dss_right_w'
const LEFT_COLLAPSED_KEY = 'dss_left_collapsed'
const RIGHT_COLLAPSED_KEY = 'dss_right_collapsed'

function readWidth(key: string, fallback: number): number {
  const v = Number(localStorage.getItem(key))
  return Number.isFinite(v) && v > 0 ? v : fallback
}

function readBool(key: string, fallback: boolean): boolean {
  const v = localStorage.getItem(key)
  return v === null ? fallback : v === '1'
}

export default function WorkbenchPage() {
  const { pid = '', sid = '' } = useParams()
  const [showSkills, setShowSkills] = useState(false)
  const [previewFile, setPreviewFile] = useState<WorkspaceFile | null>(null)
  const [files, setFiles] = useState<WorkspaceFile[]>([])
  const [filesLoading, setFilesLoading] = useState(false)
  const [filesError, setFilesError] = useState<string | null>(null)
  const [planModes, setPlanModes] = useState<Record<string, boolean | undefined>>({})
  const [approvingPlans, setApprovingPlans] = useState<Record<string, boolean | undefined>>({})
  const [planErrors, setPlanErrors] = useState<Record<string, string | undefined>>({})
  const [queueErrors, setQueueErrors] = useState<Record<string, string | undefined>>({})
  const visibleSidRef = useRef(sid)
  visibleSidRef.current = sid
  const planMode = planModes[sid] ?? false
  const approvingPlan = approvingPlans[sid] ?? false
  const planError = planErrors[sid] ?? null

  const updatePlanMode = (targetSid: string, enabled: boolean) => {
    setPlanModes((current) => ({ ...current, [targetSid]: enabled }))
  }
  const updateApprovingPlan = (targetSid: string, approving: boolean) => {
    setApprovingPlans((current) => ({ ...current, [targetSid]: approving }))
  }
  const updatePlanError = (targetSid: string, error: string | null) => {
    setPlanErrors((current) => {
      if (current[targetSid] === (error ?? undefined)) return current
      const next = { ...current }
      if (error) next[targetSid] = error
      else delete next[targetSid]
      return next
    })
  }

  // 栏宽：localStorage 持久化
  const [leftW, setLeftW] = useState(() => readWidth(LEFT_KEY, 224))
  const [rightW, setRightW] = useState(() => readWidth(RIGHT_KEY, 420))
  useEffect(() => localStorage.setItem(LEFT_KEY, String(leftW)), [leftW])
  useEffect(() => localStorage.setItem(RIGHT_KEY, String(rightW)), [rightW])

  // 左右栏收起状态：localStorage 持久化
  const [leftCollapsed, setLeftCollapsed] = useState(() => readBool(LEFT_COLLAPSED_KEY, false))
  const [rightCollapsed, setRightCollapsed] = useState(() => readBool(RIGHT_COLLAPSED_KEY, false))
  useEffect(() => localStorage.setItem(LEFT_COLLAPSED_KEY, leftCollapsed ? '1' : '0'), [leftCollapsed])
  useEffect(() => localStorage.setItem(RIGHT_COLLAPSED_KEY, rightCollapsed ? '1' : '0'), [rightCollapsed])

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
  const projects = useProjects()
  const bots = useBots()
  const projectName = projects.find((p) => p.id === pid)?.name ?? ''
  const bot = session?.bot_id ? bots.find((candidate) => candidate.id === session.bot_id) : undefined
  const sessionState = useSessionState(sid)
  const messages = useMessages(sid)
  const stream = useStream(sid)
  const promptQueue = usePromptQueue(sid)
  // Only an active or classified terminal stream owns plan/awaiting. An
  // unclassified retired shell must yield to the canonical restored session.
  const classifiedStream = stream && (stream.running || stream.kind !== null)
    ? stream
    : undefined
  const plan = classifiedStream ? classifiedStream.plan : sessionState?.plan ?? null
  const awaiting = currentAwaitingKind(classifiedStream, sessionState?.runs)
  const awaitingPlan = awaiting === 'plan_approval'
  const canExecutePlan = canExecutePlanNow(plan, awaiting, classifiedStream?.running ?? false)

  const refreshFilesForSession = useCallback(async (targetSid: string) => {
    if (!targetSid) return
    const visible = visibleSidRef.current === targetSid
    if (visible) {
      setFilesLoading(true)
      setFilesError(null)
    }
    try {
      const nextFiles = await listFiles(targetSid)
      if (visibleSidRef.current === targetSid) setFiles(nextFiles)
    } catch (error) {
      if (visibleSidRef.current === targetSid) {
        setFiles([])
        setFilesError(error instanceof Error ? error.message : String(error))
      }
    } finally {
      if (visibleSidRef.current === targetSid) setFilesLoading(false)
    }
  }, [])

  const refreshFiles = useCallback(
    () => refreshFilesForSession(sid),
    [refreshFilesForSession, sid],
  )

  const refreshDurableQueue = useCallback(async (targetSid: string) => {
    try {
      const jobs = await listBotJobs(targetSid)
      replacePromptQueue(
        targetSid,
        jobs
          .filter((job) => job.status === 'queued')
          .map((job) => ({
            id: job.id,
            revision: job.revision,
            text: job.prompt,
            createdAt: job.created_at,
            requestedPlanMode: job.requested_plan_mode,
          })),
      )
    } catch (reason) {
      updateQueueError(
        targetSid,
        `恢复 Agent 任务队列失败：${reason instanceof Error ? reason.message : String(reason)}`,
      )
    }
  }, [])

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
    if (sid) {
      void loadMessages(sid)
      void refreshFiles()
    }
  }, [sid, refreshFiles])

  useEffect(() => {
    if (sid && session?.bot_id) void refreshDurableQueue(sid)
  }, [sid, session?.bot_id, refreshDurableQueue])

  function resolveComposerIntent(targetSid: string, requestedPlanMode: boolean) {
    const live = getStreamSnapshot(targetSid)
    const restored = getSessionStateSnapshot(targetSid)
    return resolvePromptRunIntent(live, restored, requestedPlanMode)
  }

  // The only local run launcher. Durable queue ownership is confirmed before
  // opening SSE so the claim event and run acceptance never contend for the
  // same append-only session-event sequence.
  function launchSessionRun(
    targetSid: string,
    text: string,
    requestedPlanMode: boolean,
    executePlanOverride?: boolean,
    lease?: PromptRunLease,
    durableJob?: QueuedPrompt,
  ): boolean {
    const prompt = text.trim()
    const ownsGate = !!lease && promptRunGate.isCurrent(lease)
    if (
      !targetSid ||
      !prompt ||
      getStreamSnapshot(targetSid)?.running ||
      (promptRunGate.isBlocked(targetSid) && !ownsGate)
    ) return false

    const intent = executePlanOverride === undefined
      ? resolveComposerIntent(targetSid, requestedPlanMode)
      : { planMode: requestedPlanMode, executePlan: executePlanOverride }
    updatePlanError(targetSid, null)
    sendUserMessage(targetSid, prompt)
    const runId = startStream(targetSid, intent.executePlan, intent.planMode)
    const durableClaim = durableJob && bot
      ? claimBotJob(durableJob.id, durableJob.revision, runId)
      : null

    const connectClaimedRun = () => {
      const current = getStreamSnapshot(targetSid)
      if (!current?.running || current.runId !== runId) return false
      const abort = connectSSE(targetSid, prompt, {
        onStart: (frameId, taskSummary) =>
          setStreamStart(targetSid, frameId, taskSummary, runId),
        onIteration: (iteration) => advanceStreamIteration(targetSid, iteration, runId),
        onThinking: (t) => appendStreamThinking(targetSid, t, runId),
        onText: (t) => appendStreamText(targetSid, t, runId),
        onDraftReset: () => resetStreamDraft(targetSid, runId),
        onToolCalls: (calls) => appendStreamToolCall(targetSid, calls, runId),
        onToolResults: (results) => {
          if (appendStreamToolResult(targetSid, results, runId)) {
            void refreshFilesForSession(targetSid)
          }
        },
        onPlanUpdate: (nextPlan) => setStreamPlan(targetSid, nextPlan, runId),
        onComplete: (e) => {
          const beforeTerminal = getStreamSnapshot(targetSid)
          const queueBeforeTerminal = getPromptQueue(targetSid)
          const accepted = completeStream(
            targetSid,
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
            if (durableJob && durableClaim) {
              void durableClaim
                .then(() => finishBotJob(
                  durableJob.id,
                  runId,
                  e.kind === 'natural' || e.kind === 'awaiting',
                  e.error ?? null,
                ))
                .then(() => refreshDurableQueue(targetSid))
                .catch(() => refreshDurableQueue(targetSid))
            }
            void refreshFilesForSession(targetSid)
            void restoreAndMaybeDrainPromptQueue(
              targetSid,
              runId,
              {
                kind: e.kind,
                awaiting: e.awaiting ?? null,
                wasStopping: beforeTerminal?.stopping ?? false,
                steering: queueBeforeTerminal.steering,
              },
              {
                gate: promptRunGate,
                loadMessages,
                isRunActive: (sessionId) => !!getStreamSnapshot(sessionId)?.running,
                claimAndLaunchNext,
              },
            )
          }
        },
        onError: (m) => {
          if (durableJob && durableClaim) {
            void durableClaim
              .then(() => finishBotJob(durableJob.id, runId, false, m))
              .then(() => refreshDurableQueue(targetSid))
              .catch(() => refreshDurableQueue(targetSid))
          }
          if (failStream(targetSid, m, runId)) {
            void refreshFilesForSession(targetSid)
            // A transport failure is not proof that the backend session mutex is
            // free, so retain the queue and never auto-drain from this path.
            void loadMessages(targetSid)
          }
        },
      }, { planMode: intent.planMode, executePlan: intent.executePlan, runId })
      setStreamAborter(targetSid, abort, runId)
      return true
    }

    if (durableClaim) {
      void durableClaim
        .then(() => {
          if (!connectClaimedRun() && durableJob) {
            return finishBotJob(
              durableJob.id,
              runId,
              false,
              'Local run retired before the claimed job could connect',
            )
          }
          return undefined
        })
        .then(() => refreshDurableQueue(targetSid))
        .catch((reason) => {
          updateQueueError(
            targetSid,
            `Agent 任务领取未确认：${reason instanceof Error ? reason.message : String(reason)}`,
          )
          if (failStream(targetSid, 'Agent 任务领取失败，请从队列重试。', runId)) {
            void loadMessages(targetSid)
          }
          void refreshDurableQueue(targetSid)
        })
    } else {
      connectClaimedRun()
    }
    return true
  }

  function launchQueuedPrompt(targetSid: string, prompt: QueuedPrompt): boolean {
    return launchSessionRun(targetSid, prompt.text, prompt.requestedPlanMode, undefined, undefined, prompt)
  }

  function claimAndLaunchNext(
    targetSid: string,
    lease: PromptRunLease,
  ): QueuedPrompt | null {
    if (!promptRunGate.isCurrent(lease) || getStreamSnapshot(targetSid)?.running) return null
    const prompt = claimNextQueuedPrompt(targetSid)
    if (!prompt) return null
    return launchSessionRun(
      targetSid,
      prompt.text,
      prompt.requestedPlanMode,
      undefined,
      lease,
      prompt,
    ) ? prompt : null
  }

  function claimAndLaunchSelected(
    targetSid: string,
    itemId: string,
    expectedRevision: number,
    lease: PromptRunLease,
  ): QueuedPrompt | null {
    if (!promptRunGate.isCurrent(lease) || getStreamSnapshot(targetSid)?.running) return null
    const prompt = claimQueuedPrompt(targetSid, itemId, expectedRevision)
    if (!prompt) return null
    return launchSessionRun(
      targetSid,
      prompt.text,
      prompt.requestedPlanMode,
      undefined,
      lease,
      prompt,
    ) ? prompt : null
  }

  function launchNextQueuedPrompt(targetSid: string): boolean {
    if (
      getStreamSnapshot(targetSid)?.running ||
      getPromptQueue(targetSid).steering ||
      promptRunGate.isBlocked(targetSid)
    ) return false
    const prompt = claimNextQueuedPrompt(targetSid)
    return prompt ? launchQueuedPrompt(targetSid, prompt) : false
  }

  const handleApprovePlan = async () => {
    if (!sid || approvingPlan) return
    const targetSid = sid
    updateApprovingPlan(targetSid, true)
    updatePlanError(targetSid, null)
    try {
      const approved = await approvePlan(targetSid)
      setStreamPlan(targetSid, { approved: approved.approved, steps: approved.steps })
      updatePlanMode(targetSid, false)
      if (!launchSessionRun(targetSid, '请按照已批准的计划开始执行。', false, true)) {
        updatePlanError(targetSid, '计划已批准，但会话仍在恢复；请稍后点击“执行计划/重试”。')
      }
    } catch (error) {
      updatePlanError(
        targetSid,
        `批准计划失败：${error instanceof Error ? error.message : String(error)}`,
      )
    } finally {
      updateApprovingPlan(targetSid, false)
    }
  }

  const handleExecutePlan = () => {
    if (!canExecutePlan) return
    const targetSid = sid
    updatePlanMode(targetSid, false)
    if (!launchSessionRun(targetSid, '请按照已批准的计划开始执行。', false, true)) {
      updatePlanError(targetSid, '会话仍在恢复，暂时无法启动计划；请稍后重试。')
    }
  }

  const handleComposerSend = (text: string) => {
    if (!sid || !text.trim()) return
    const live = getStreamSnapshot(sid)
    const queue = getPromptQueue(sid)
    if (live?.running || queue.items.length > 0 || promptRunGate.isBlocked(sid)) {
      const queued = enqueuePrompt(sid, { text, requestedPlanMode: planMode })
      if (queued) {
        updateQueueError(sid, null)
        if (bot) {
          void enqueueBotJob(sid, {
            id: queued.id,
            bot_id: bot.id,
            prompt: queued.text,
            plan_mode: queued.requestedPlanMode,
          }).catch((reason) => {
            deleteQueuedPrompt(sid, queued.id, queued.revision)
            updateQueueError(
              sid,
              `保存 Agent 任务队列失败：${reason instanceof Error ? reason.message : String(reason)}`,
            )
          })
        }
        if (!live?.running) {
          // If a terminal restore currently owns this idle session, record the
          // explicit resume intent. Its lease will claim FIFO after the GET;
          // direct launch stays blocked so stale history cannot overwrite it.
          promptRunGate.requestDrain(sid)
          launchNextQueuedPrompt(sid)
        }
      }
      return
    }
    launchSessionRun(sid, text, planMode)
  }

  const updateQueueError = (targetSid: string, error: string | null) => {
    setQueueErrors((current) => {
      if (current[targetSid] === (error ?? undefined)) return current
      const next = { ...current }
      if (error) next[targetSid] = error
      else delete next[targetSid]
      return next
    })
  }

  const handleQueueActivate = (item: QueuedPrompt) => {
    if (!sid) return
    const targetSid = sid
    const live = getStreamSnapshot(targetSid)
    updateQueueError(targetSid, null)

    if (!live?.running) {
      if (promptRunGate.isBlocked(targetSid)) {
        updateQueueError(targetSid, '正在恢复上一轮消息，请稍后重试。')
        return
      }
      const claimed = claimQueuedPrompt(targetSid, item.id, item.revision)
      if (!claimed) {
        updateQueueError(targetSid, '队列消息已变化，请重试。')
        return
      }
      if (!launchQueuedPrompt(targetSid, claimed)) {
        updateQueueError(targetSid, '未能启动队列消息。')
      }
      return
    }

    if (live.stopping) {
      updateQueueError(targetSid, '当前运行正在停止，请稍后重试。')
      return
    }
    if (!beginQueuedPromptSteering(targetSid, item.id, item.revision)) {
      updateQueueError(targetSid, '队列消息已变化，请重试。')
      return
    }

    const selected = { itemId: item.id, revision: item.revision }
    void coordinateQueuedPromptSteer(targetSid, live.runId, selected, {
      gate: promptRunGate,
      beginStop: (sessionId, runId) => beginStreamStop(sessionId, runId),
      cancelRun: cancelSessionRun,
      finishCancelledRun: (sessionId, runId) => {
        finishStreamStop(sessionId, runId)
      },
      retireNormallyFinishedRun: (sessionId, runId) => {
        retireStreamAfterBackendFinish(sessionId, runId)
      },
      failStop: (sessionId, runId, error) => {
        failStreamStop(sessionId, error, runId)
      },
      loadMessages,
      claimAndLaunchSelected,
      clearSteering: (sessionId, itemId, revision) => {
        clearQueuedPromptSteering(sessionId, itemId, revision)
      },
    }).then((result) => {
      if (result.status === 'failed') {
        updateQueueError(targetSid, `调整方向失败：${result.error}`)
      } else if (result.status === 'stale') {
        updateQueueError(targetSid, '当前运行或队列消息已变化，请重试。')
      } else {
        updateQueueError(targetSid, null)
      }
    })
  }

  const handleStop = async () => {
    if (!sid) return
    const runId = beginStreamStop(sid)
    if (!runId) return
    await coordinateManualPromptStop(sid, runId, {
      cancelRun: cancelSessionRun,
      finishCancelledRun: (sessionId, expectedRunId) =>
        finishStreamStop(sessionId, expectedRunId),
      retireNormallyFinishedRun: (sessionId, expectedRunId) =>
        retireStreamAfterBackendFinish(sessionId, expectedRunId),
      failStop: (sessionId, expectedRunId, error) => {
        failStreamStop(sessionId, error, expectedRunId)
      },
      loadMessages,
    })
  }

  const handleDeleteFile = async (file: WorkspaceFile) => {
    if (!window.confirm(`确定删除工作区文件“${file.path}”吗？此操作无法撤销。`)) return
    await deleteFile(sid, file.path)
    if (previewFile?.path === file.path) setPreviewFile(null)
    await refreshFiles()
  }

  return (
    <div className="flex h-full">
      {!leftCollapsed && (
        <>
          <Sidebar
            pid={pid}
            sid={sid}
            width={leftW}
            onOpenSkills={() => setShowSkills(true)}
            onOpenFiles={() => openTab(FILES_TAB)}
          />
          <ResizeHandle side="left" value={leftW} min={200} max={360} onChange={setLeftW} />
        </>
      )}

      <ChatArea
        sessionId={sid}
        messages={messages}
        failed={session?.status === 'failed'}
        stream={stream}
        plan={plan}
        awaitingPlan={awaitingPlan}
        canExecutePlan={canExecutePlan}
        approvingPlan={approvingPlan}
        planError={planError}
        queue={promptQueue}
        queueError={queueErrors[sid] ?? null}
        planMode={planMode}
        onPlanModeChange={(enabled) => updatePlanMode(sid, enabled)}
        onApprovePlan={() => void handleApprovePlan()}
        onExecutePlan={handleExecutePlan}
        onQueueReorder={(itemId, targetId) => {
          const changed = reorderQueuedPrompt(sid, itemId, targetId)
          if (changed) {
            updateQueueError(sid, null)
            if (bot) {
              const orderedIds = getPromptQueue(sid).items.map((item) => item.id)
              void reorderBotJobs(sid, orderedIds)
                .then((jobs) => replacePromptQueue(sid, jobs.filter((job) => job.status === 'queued').map((job) => ({
                  id: job.id,
                  revision: job.revision,
                  text: job.prompt,
                  createdAt: job.created_at,
                  requestedPlanMode: job.requested_plan_mode,
                }))))
                .catch(() => refreshDurableQueue(sid))
            }
          }
          return changed
        }}
        onQueueEdit={(itemId, revision, text) => {
          const changed = editQueuedPrompt(sid, itemId, revision, text)
          if (changed) {
            updateQueueError(sid, null)
            if (bot) void editBotJob(itemId, { revision, prompt: text, plan_mode: getPromptQueue(sid).items.find((item) => item.id === itemId)?.requestedPlanMode ?? false })
              .catch(() => refreshDurableQueue(sid))
          }
          return changed
        }}
        onQueueDelete={(itemId, revision) => {
          const changed = deleteQueuedPrompt(sid, itemId, revision)
          if (changed) {
            updateQueueError(sid, null)
            if (bot) void deleteBotJob(itemId, revision).catch(() => refreshDurableQueue(sid))
          }
          return changed
        }}
        onQueueActivate={handleQueueActivate}
        onSend={handleComposerSend}
        onStop={() => void handleStop()}
        title={bot ? `${bot.avatar} ${bot.name}` : projectName}
        leftCollapsed={leftCollapsed}
        rightCollapsed={rightCollapsed}
        onToggleLeft={() => setLeftCollapsed((v) => !v)}
        onToggleRight={() => setRightCollapsed((v) => !v)}
      />

      {!rightCollapsed && (
        <>
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
        </>
      )}

      {showSkills && <SkillsModal onClose={() => setShowSkills(false)} />}
      {previewFile &&
        (filePreviewKind(previewFile.path) === 'pdf' ? (
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
        ) : filePreviewKind(previewFile.path) === 'image' ? (
          <ImagePreviewModal sid={sid} file={previewFile} onClose={() => setPreviewFile(null)} />
        ) : (
          <FilePreviewModal sid={sid} file={previewFile} onClose={() => setPreviewFile(null)} />
        ))}
    </div>
  )
}
