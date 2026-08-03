// 右侧产物面板：tab 栏（可全部关闭）+ tab 预览内容；无 tab 时显示 artifacts 浏览视图。
import { useMemo, useState } from 'react'
import type { Artifact, WorkspaceFile } from '../../types'
import { mockFileContents } from '../../mock/data'
import { IconChevronDown, IconFile, IconGrid, IconList, IconSearch, IconX } from '../icons'

export interface PanelTab {
  id: string
  label: string
  kind: 'markdown' | 'tex' | 'pdf' | 'notebook' | 'compute' | 'files'
}

export const FILES_TAB: PanelTab = { id: 'files', label: 'Files', kind: 'files' }

export const DEFAULT_TABS: PanelTab[] = [
  { id: 'a:review_leadfree_perovskite.md', label: 'review_leadfree_perovskite.md', kind: 'markdown' },
  { id: 'a:review_leadfree_perovskite.tex', label: 'review_leadfree_perovskite.tex', kind: 'tex' },
  { id: 'notebook', label: 'Notebook', kind: 'notebook' },
  { id: 'compute', label: 'Compute', kind: 'compute' },
]

interface Props {
  artifacts: Artifact[]
  files: WorkspaceFile[]
  tabs: PanelTab[]
  activeTab: string | null
  onSelectTab: (id: string) => void
  onCloseTab: (id: string) => void
  onOpenArtifact: (a: Artifact) => void
  onPreviewFile: (f: WorkspaceFile) => void
}

export default function ArtifactPanel({
  artifacts,
  files,
  tabs,
  activeTab,
  onSelectTab,
  onCloseTab,
  onOpenArtifact,
  onPreviewFile,
}: Props) {
  const active = tabs.find((t) => t.id === activeTab) ?? tabs[tabs.length - 1]

  return (
    <div className="flex h-full min-w-0 flex-1 flex-col bg-bg">
      {/* tab 栏：全部关完后整栏隐藏，进入浏览视图 */}
      {tabs.length > 0 && (
        <div className="flex shrink-0 items-center gap-0.5 overflow-x-auto border-b border-border px-2">
          {tabs.map((t) => (
            <div
              key={t.id}
              className={`group flex shrink-0 items-center gap-1 whitespace-nowrap border-b px-2 py-2 ${
                active?.id === t.id ? 'border-brand' : 'border-transparent'
              }`}
            >
              <button
                onClick={() => onSelectTab(t.id)}
                className={`font-mono text-[11px] ${
                  active?.id === t.id ? 'text-brand' : 'text-ink3 hover:text-ink2'
                }`}
              >
                {t.label}
              </button>
              <button
                onClick={() => onCloseTab(t.id)}
                className="rounded p-0.5 text-ink3 opacity-0 hover:bg-surface2 hover:text-ink group-hover:opacity-100"
                aria-label={`关闭 ${t.label}`}
              >
                <IconX width={10} height={10} />
              </button>
            </div>
          ))}
        </div>
      )}

      {/* 内容区 */}
      {tabs.length === 0 || !active ? (
        <BrowseView artifacts={artifacts} onOpenArtifact={onOpenArtifact} />
      ) : (
        <TabContent tab={active} files={files} onPreviewFile={onPreviewFile} />
      )}
    </div>
  )
}

/* ---------- tab 内容 ---------- */

function TabContent({
  tab,
  files,
  onPreviewFile,
}: {
  tab: PanelTab
  files: WorkspaceFile[]
  onPreviewFile: (f: WorkspaceFile) => void
}) {
  switch (tab.kind) {
    case 'files':
      return <FilesView files={files} onPreviewFile={onPreviewFile} />
    case 'markdown':
      return <MarkdownPlaceholder label={tab.label} />
    case 'tex':
      return <TexPlaceholder label={tab.label} />
    case 'pdf':
      return (
        <div className="flex min-h-0 flex-1 justify-center overflow-y-auto bg-surface py-8">
          <PdfPaper />
        </div>
      )
    default:
      return (
        <div className="flex flex-1 items-center justify-center text-[13px] text-ink3">
          {tab.label} — 占位（后续版本接入）
        </div>
      )
  }
}

/** md 预览：标题 + 正文灰行占位排版。 */
function MarkdownPlaceholder({ label }: { label: string }) {
  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 py-6">
      <h1 className="text-[18px] font-semibold leading-[1.3]">{label.replace(/\.md$/, '')}</h1>
      <p className="mt-1 text-[12px] text-ink3">Markdown 预览占位 — 后续接入 react-markdown</p>
      {[5, 7, 4, 6].map((lines, s) => (
        <div key={s} className="mt-6">
          <div className="h-3 w-40 rounded-sm bg-surface2" />
          <div className="mt-3 space-y-1.5">
            {Array.from({ length: lines }).map((_, i) => (
              <div
                key={i}
                className="h-2 rounded-sm bg-surface2"
                style={{ width: i === lines - 1 ? '58%' : '100%' }}
              />
            ))}
          </div>
        </div>
      ))}
    </div>
  )
}

/** tex 预览：等宽源码行（取 mock 内容，没有则灰行）。 */
function TexPlaceholder({ label }: { label: string }) {
  const src = mockFileContents[label]
  return (
    <div className="min-h-0 flex-1 overflow-auto bg-surface px-6 py-4">
      {src ? (
        <pre className="font-mono text-[12px] leading-[1.7] text-ink2">{src}</pre>
      ) : (
        <div className="space-y-1.5 pt-2">
          {Array.from({ length: 18 }).map((_, i) => (
            <div key={i} className="h-2 rounded-sm bg-surface2" style={{ width: `${88 - (i % 5) * 14}%` }} />
          ))}
        </div>
      )}
    </div>
  )
}

/** pdf 预览：灰底 + 白色纸张灰块占位。 */
export function PdfPaper() {
  return (
    <div className="h-fit w-[560px] shrink-0 rounded-sm border border-border bg-bg px-12 py-10 shadow-subtle">
      <div className="mx-auto h-3 w-2/3 rounded-sm bg-surface2" />
      <div className="mx-auto mt-2 h-3 w-1/2 rounded-sm bg-surface2" />
      <div className="mx-auto mt-4 h-2 w-24 rounded-sm bg-surface2" />
      {[6, 8, 5].map((lines, s) => (
        <div key={s} className="mt-7">
          <div className="h-2.5 w-32 rounded-sm bg-surface2" />
          <div className="mt-2.5 space-y-1.5">
            {Array.from({ length: lines }).map((_, i) => (
              <div
                key={i}
                className="h-2 rounded-sm bg-surface2"
                style={{ width: i === lines - 1 ? '61%' : '100%' }}
              />
            ))}
          </div>
        </div>
      ))}
      <p className="mt-8 text-center text-[11px] text-ink3">PDF 占位 — 后续接入 Tectonic + pdfjs</p>
    </div>
  )
}

/* ---------- Files 视图 ---------- */

function FilesView({
  files,
  onPreviewFile,
}: {
  files: WorkspaceFile[]
  onPreviewFile: (f: WorkspaceFile) => void
}) {
  return (
    <div className="min-h-0 flex-1 overflow-y-auto p-3">
      <div className="mb-2 text-[12px] font-medium text-ink">Workspace files</div>
      <div className="divide-y divide-border rounded-md border border-border">
        {files.map((f) => (
          <button
            key={f.path}
            className="flex w-full items-center gap-2.5 px-3 py-2 text-left hover:bg-surface"
            onClick={() => onPreviewFile(f)}
          >
            <IconFile width={14} height={14} className="shrink-0 text-ink3" />
            <span className="min-w-0 flex-1 truncate font-mono text-[12px] text-ink">{f.path}</span>
            <span className="shrink-0 text-[11px] text-ink3">{(f.size / 1024).toFixed(0)} KB</span>
          </button>
        ))}
      </div>
      <p className="mt-2 text-[11px] text-ink3">点击文件打开预览（文本走弹层，PDF 走预览弹层）</p>
    </div>
  )
}

/* ---------- artifacts 浏览视图（原卡片列表） ---------- */

function BrowseView({
  artifacts,
  onOpenArtifact,
}: {
  artifacts: Artifact[]
  onOpenArtifact: (a: Artifact) => void
}) {
  const [view, setView] = useState<'grid' | 'list'>('grid')
  const [q, setQ] = useState('')

  const uploads = artifacts.filter((a) => a.origin === 'upload')
  const created = useMemo(
    () => artifacts.filter((a) => a.origin === 'agent' && a.path.toLowerCase().includes(q.trim().toLowerCase())),
    [artifacts, q],
  )

  return (
    <>
      {/* 工具行 */}
      <div className="flex shrink-0 items-center gap-2 border-b border-border px-3 py-2">
        <button className="btn-ghost py-1 text-[12px] font-medium text-ink">
          Artifacts <IconChevronDown width={12} height={12} />
        </button>
        <div className="flex flex-1 items-center gap-1.5 rounded-md border border-border px-2 py-1">
          <IconSearch width={12} height={12} className="text-ink3" />
          <input
            value={q}
            onChange={(e) => setQ(e.target.value)}
            placeholder="Search artifacts…"
            className="w-full bg-transparent text-[12px] outline-none placeholder:text-ink3"
          />
        </div>
        <span className="whitespace-nowrap text-[11px] text-ink3">{created.length} artifacts · Created</span>
        <button
          className="btn-ghost rounded p-1"
          onClick={() => setView((v) => (v === 'grid' ? 'list' : 'grid'))}
          title="切换视图"
        >
          {view === 'grid' ? <IconList width={14} height={14} /> : <IconGrid width={14} height={14} />}
        </button>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto p-3">
        {uploads.length > 0 && (
          <>
            <SectionLabel left="Your uploads" right={`${uploads.length} file`} />
            <div className={view === 'grid' ? 'grid grid-cols-3 gap-2' : 'space-y-2'}>
              {uploads.map((a) => (
                <ArtifactCard key={a.path} artifact={a} view={view} onOpen={onOpenArtifact} />
              ))}
            </div>
          </>
        )}
        <SectionLabel
          left="研究一下 新型绿色无铅钙钛矿材料在太阳电池领域的应用，写一篇综述"
          right={`${created.length} artifacts`}
        />
        <div className={view === 'grid' ? 'grid grid-cols-3 gap-2' : 'space-y-2'}>
          {created.map((a) => (
            <ArtifactCard key={a.path} artifact={a} view={view} onOpen={onOpenArtifact} />
          ))}
        </div>
      </div>
    </>
  )
}

function SectionLabel({ left, right }: { left: string; right: string }) {
  return (
    <div className="mb-2 mt-4 flex items-center justify-between first:mt-0">
      <span className="truncate text-[12px] font-medium text-ink">{left}</span>
      <span className="shrink-0 text-[11px] text-ink3">{right}</span>
    </div>
  )
}

function ArtifactCard({
  artifact: a,
  view,
  onOpen,
}: {
  artifact: Artifact
  view: 'grid' | 'list'
  onOpen: (a: Artifact) => void
}) {
  const sub = `${(a.size / 1024).toFixed(0)} KB · ${a.origin === 'upload' ? 'Uploaded' : 'Created'}`

  if (view === 'list') {
    return (
      <button
        className="flex w-full items-center gap-2.5 rounded-md border border-border bg-bg px-3 py-2 text-left hover:bg-surface"
        onClick={() => onOpen(a)}
      >
        <IconFile width={14} height={14} className="shrink-0 text-ink3" />
        <span className="min-w-0 flex-1">
          <span className="block truncate font-mono text-[12px] text-ink">{a.path}</span>
          <span className="block text-[11px] text-ink3">{sub}</span>
        </span>
      </button>
    )
  }

  return (
    <button
      className="overflow-hidden rounded-md border border-border bg-bg text-left hover:border-borderStrong"
      onClick={() => onOpen(a)}
      title={a.path}
    >
      {/* 缩略图：灰色占位块模拟页面排版 */}
      <div className="flex h-24 items-center justify-center bg-surface p-2">
        <div className="h-full w-3/4 rounded-sm border border-border bg-bg p-1.5 shadow-subtle">
          <div className="mx-auto mb-1 h-1 w-2/3 rounded-full bg-surface2" />
          <div className="space-y-0.5">
            {Array.from({ length: 6 }).map((_, i) => (
              <div key={i} className="h-0.5 rounded-full bg-surface2" style={{ width: `${90 - i * 8}%` }} />
            ))}
          </div>
        </div>
      </div>
      <div className="border-t border-border px-2 py-1.5">
        <div className="truncate font-mono text-[11px] text-ink">{a.path}</div>
        <div className="text-[10px] text-ink3">{sub}</div>
      </div>
    </button>
  )
}
