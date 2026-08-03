// 工作台页：三栏布局（左右栏宽可拖拽并持久化）+ 右侧 tab 系统 + 文件预览弹层 + Skills 弹层。
import { useEffect, useState } from 'react'
import { useParams } from 'react-router-dom'
import { mockArtifacts, mockFiles, mockProjects } from '../mock/data'
import type { Artifact, WorkspaceFile } from '../types'
import { connectSSE } from '../api/client'
import {
  appendStreamText,
  appendStreamThinking,
  appendStreamToolCall,
  appendStreamToolResult,
  completeStream,
  failStream,
  loadFromBackend,
  loadMessages,
  sendUserMessage,
  setStreamAborter,
  startStream,
  stopStream,
  useMessages,
  useSession,
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

  // 栏宽：localStorage 持久化
  const [leftW, setLeftW] = useState(() => readWidth(LEFT_KEY, 224))
  const [rightW, setRightW] = useState(() => readWidth(RIGHT_KEY, 420))
  useEffect(() => localStorage.setItem(LEFT_KEY, String(leftW)), [leftW])
  useEffect(() => localStorage.setItem(RIGHT_KEY, String(rightW)), [rightW])

  // 右栏 tab 状态（可全部关闭；全部关完显示浏览视图）
  const [tabs, setTabs] = useState<PanelTab[]>(DEFAULT_TABS)
  const [activeTab, setActiveTab] = useState<string | null>(DEFAULT_TABS[0].id)

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
  const openArtifact = (a: Artifact) =>
    openTab({ id: `a:${a.path}`, label: a.path, kind: a.kind === 'markdown' ? 'markdown' : a.kind === 'tex' ? 'tex' : 'pdf' })

  // 会话消息（store 驱动；新会话为空态）
  const session = useSession(sid)
  const messages = useMessages(sid)
  const stream = useStream(sid)

  // 进入会话时：刷新侧栏的 projects/sessions，并从后端恢复当前会话历史。
  useEffect(() => {
    void loadFromBackend()
    if (sid) void loadMessages(sid)
  }, [sid])

  // 发送：用户气泡立即上屏，然后走 SSE 流（POST /api/sessions/{sid}/stream-sse）
  const handleSend = (text: string) => {
    sendUserMessage(sid, text)
    startStream(sid)
    const abort = connectSSE(sid, text, {
      onThinking: (t) => appendStreamThinking(sid, t),
      onText: (t) => appendStreamText(sid, t),
      onToolCalls: (calls) => appendStreamToolCall(sid, calls),
      onToolResults: (results) => appendStreamToolResult(sid, results),
      onComplete: (e) =>
        completeStream(sid, e.usage ?? null, e.iterations ?? 0, e.kind, e.pending_ask ?? null),
      onError: (m) => failStream(sid, m),
    })
    setStreamAborter(sid, abort)
  }

  // 第一版：pid 仅用于选中态，内容全部来自 mock（TODO: 接后端后按 pid/sid 拉取）
  void mockProjects.find((p) => p.id === pid)

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
        onSend={handleSend}
        onStop={() => stopStream(sid)}
      />

      <ResizeHandle side="right" value={rightW} min={360} max={760} onChange={setRightW} />
      <div className="shrink-0 border-l border-border" style={{ width: rightW }}>
        <ArtifactPanel
          artifacts={mockArtifacts}
          files={mockFiles}
          tabs={tabs}
          activeTab={activeTab}
          onSelectTab={setActiveTab}
          onCloseTab={closeTab}
          onOpenArtifact={openArtifact}
          onPreviewFile={setPreviewFile}
        />
      </div>

      {showSkills && <SkillsModal onClose={() => setShowSkills(false)} />}
      {previewFile &&
        (previewFile.path.endsWith('.pdf') ? (
          <PdfPreviewModal
            artifact={{
              path: previewFile.path,
              size: previewFile.size,
              frame_id: '',
              kind: 'pdf',
              origin: 'upload',
              created_at: '',
            }}
            onClose={() => setPreviewFile(null)}
          />
        ) : (
          <FilePreviewModal file={previewFile} onClose={() => setPreviewFile(null)} />
        ))}
    </div>
  )
}
