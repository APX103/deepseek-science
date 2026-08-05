// 右侧产物面板：只展示当前会话实际返回的 artifacts / workspace files。
import { useMemo, useState } from 'react'
import type { Artifact, WorkspaceFile } from '../../types'
import { IconFile, IconGrid, IconList, IconSearch, IconTrash, IconX } from '../icons'

export interface PanelTab {
  id: string
  label: string
  kind: 'markdown' | 'tex' | 'pdf' | 'notebook' | 'compute' | 'files'
}

export const FILES_TAB: PanelTab = { id: 'files', label: 'Files', kind: 'files' }

/** 会话没有打开文件时直接进入产物浏览视图，不制造默认文档。 */
export const DEFAULT_TABS: PanelTab[] = []

interface Props {
  artifacts: Artifact[]
  files: WorkspaceFile[]
  tabs: PanelTab[]
  activeTab: string | null
  filesLoading?: boolean
  filesError?: string | null
  taskLabel?: string
  onSelectTab: (id: string) => void
  onCloseTab: (id: string) => void
  onOpenArtifact: (a: Artifact) => void
  onPreviewFile: (f: WorkspaceFile) => void
  onDeleteFile?: (f: WorkspaceFile) => void | Promise<void>
}

export default function ArtifactPanel({
  artifacts,
  files,
  tabs,
  activeTab,
  filesLoading = false,
  filesError = null,
  taskLabel = 'Session artifacts',
  onSelectTab,
  onCloseTab,
  onOpenArtifact,
  onPreviewFile,
  onDeleteFile,
}: Props) {
  const active = tabs.find((t) => t.id === activeTab) ?? tabs[tabs.length - 1]

  return (
    <div className="flex h-full min-w-0 flex-1 flex-col bg-bg">
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

      {tabs.length === 0 || !active ? (
        <BrowseView
          artifacts={artifacts}
          loading={filesLoading}
          error={filesError}
          taskLabel={taskLabel}
          onOpenArtifact={onOpenArtifact}
        />
      ) : (
        <TabContent
          tab={active}
          files={files}
          loading={filesLoading}
          error={filesError}
          onPreviewFile={onPreviewFile}
          onDeleteFile={onDeleteFile}
        />
      )}
    </div>
  )
}

function TabContent({
  tab,
  files,
  loading,
  error,
  onPreviewFile,
  onDeleteFile,
}: {
  tab: PanelTab
  files: WorkspaceFile[]
  loading: boolean
  error: string | null
  onPreviewFile: (f: WorkspaceFile) => void
  onDeleteFile?: (f: WorkspaceFile) => void | Promise<void>
}) {
  if (tab.kind === 'files') {
    return (
      <FilesView
        files={files}
        loading={loading}
        error={error}
        onPreviewFile={onPreviewFile}
        onDeleteFile={onDeleteFile}
      />
    )
  }

  const tabPath = tab.id.replace(/^a:/, '')
  const file = files.find((candidate) => candidate.path === tabPath || candidate.path === tab.label)

  if (loading) return <PanelStatus>正在加载文件…</PanelStatus>
  if (error) return <PanelStatus tone="error">文件加载失败：{error}</PanelStatus>
  if (!file) return <PanelStatus tone="error">工作区中找不到文件：{tabPath}</PanelStatus>

  return (
    <div className="flex min-h-0 flex-1 items-center justify-center p-6">
      <div className="w-full max-w-sm rounded-md border border-border bg-surface p-4">
        <div className="flex items-start gap-3">
          <IconFile width={18} height={18} className="mt-0.5 shrink-0 text-ink3" />
          <div className="min-w-0 flex-1">
            <p className="break-all font-mono text-[12px] text-ink">{file.path}</p>
            <p className="mt-1 text-[11px] text-ink3">{formatSize(file.size)}</p>
          </div>
        </div>
        <button className="btn-primary mt-4 w-full justify-center" onClick={() => onPreviewFile(file)}>
          打开真实文件预览
        </button>
      </div>
    </div>
  )
}

function FilesView({
  files,
  loading,
  error,
  onPreviewFile,
  onDeleteFile,
}: {
  files: WorkspaceFile[]
  loading: boolean
  error: string | null
  onPreviewFile: (f: WorkspaceFile) => void
  onDeleteFile?: (f: WorkspaceFile) => void | Promise<void>
}) {
  const [deletingPath, setDeletingPath] = useState<string | null>(null)
  const [deleteError, setDeleteError] = useState<string | null>(null)

  const removeFile = async (file: WorkspaceFile) => {
    if (!onDeleteFile || deletingPath) return
    setDeletingPath(file.path)
    setDeleteError(null)
    try {
      await onDeleteFile(file)
    } catch (err) {
      setDeleteError(err instanceof Error ? err.message : String(err))
    } finally {
      setDeletingPath(null)
    }
  }

  return (
    <div className="min-h-0 flex-1 overflow-y-auto p-3">
      <div className="mb-2 flex items-center justify-between gap-2">
        <span className="text-[12px] font-medium text-ink">Workspace files</span>
        {!loading && !error && <span className="text-[11px] text-ink3">{files.length} files</span>}
      </div>

      {loading ? (
        <PanelStatus>正在加载工作区文件…</PanelStatus>
      ) : error ? (
        <PanelStatus tone="error">文件加载失败：{error}</PanelStatus>
      ) : files.length === 0 ? (
        <PanelStatus>当前工作区没有文件。</PanelStatus>
      ) : (
        <div className="divide-y divide-border rounded-md border border-border">
          {files.map((file) => (
            <div key={file.path} className="group flex items-center gap-1 px-1 hover:bg-surface">
              <button
                className="flex min-w-0 flex-1 items-center gap-2.5 px-2 py-2 text-left"
                onClick={() => onPreviewFile(file)}
              >
                <IconFile width={14} height={14} className="shrink-0 text-ink3" />
                <span className="min-w-0 flex-1 truncate font-mono text-[12px] text-ink">{file.path}</span>
                <span className="shrink-0 text-[11px] text-ink3">{formatSize(file.size)}</span>
              </button>
              {onDeleteFile && (
                <button
                  className="rounded p-1.5 text-ink3 opacity-0 hover:bg-surface2 hover:text-red-600 focus:opacity-100 group-hover:opacity-100 disabled:opacity-40"
                  onClick={() => void removeFile(file)}
                  disabled={deletingPath !== null}
                  aria-label={`删除 ${file.path}`}
                  title={`删除 ${file.path}`}
                >
                  <IconTrash width={13} height={13} />
                </button>
              )}
            </div>
          ))}
        </div>
      )}

      {deleteError && <p className="mt-2 text-[11px] text-red-600">删除失败：{deleteError}</p>}
      {!loading && !error && files.length > 0 && (
        <p className="mt-2 text-[11px] text-ink3">点击文件打开真实内容预览。</p>
      )}
    </div>
  )
}

function BrowseView({
  artifacts,
  loading,
  error,
  taskLabel,
  onOpenArtifact,
}: {
  artifacts: Artifact[]
  loading: boolean
  error: string | null
  taskLabel: string
  onOpenArtifact: (a: Artifact) => void
}) {
  const [view, setView] = useState<'grid' | 'list'>('grid')
  const [q, setQ] = useState('')
  const normalizedQuery = q.trim().toLowerCase()
  const filtered = useMemo(
    () => artifacts.filter((artifact) => artifact.path.toLowerCase().includes(normalizedQuery)),
    [artifacts, normalizedQuery],
  )
  const uploads = filtered.filter((artifact) => artifact.origin === 'upload')
  const created = filtered.filter((artifact) => artifact.origin === 'agent')
  const unknown = filtered.filter((artifact) => artifact.origin === 'unknown')

  return (
    <>
      <div className="flex shrink-0 items-center gap-2 border-b border-border px-3 py-2">
        <span className="py-1 text-[12px] font-medium text-ink">Files &amp; artifacts</span>
        <div className="flex flex-1 items-center gap-1.5 rounded-md border border-border px-2 py-1">
          <IconSearch width={12} height={12} className="text-ink3" />
          <input
            value={q}
            onChange={(event) => setQ(event.target.value)}
            placeholder="Search files…"
            className="w-full bg-transparent text-[12px] outline-none placeholder:text-ink3"
          />
        </div>
        <span className="whitespace-nowrap text-[11px] text-ink3">
          {formatCount(filtered.length, 'file')}
        </span>
        <button
          className="btn-ghost rounded p-1"
          onClick={() => setView((current) => (current === 'grid' ? 'list' : 'grid'))}
          title="切换视图"
        >
          {view === 'grid' ? <IconList width={14} height={14} /> : <IconGrid width={14} height={14} />}
        </button>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto p-3">
        {loading ? (
          <PanelStatus>正在加载工作区文件…</PanelStatus>
        ) : error ? (
          <PanelStatus tone="error">文件加载失败：{error}</PanelStatus>
        ) : filtered.length === 0 ? (
          <PanelStatus>{normalizedQuery ? '没有匹配的文件。' : '当前工作区没有文件。'}</PanelStatus>
        ) : (
          <>
            {uploads.length > 0 && (
              <ArtifactSection
                label="Your uploads"
                artifacts={uploads}
                countKind="file"
                view={view}
                onOpenArtifact={onOpenArtifact}
              />
            )}
            {created.length > 0 && (
              <ArtifactSection
                label={taskLabel || 'Session artifacts'}
                artifacts={created}
                countKind="artifact"
                view={view}
                onOpenArtifact={onOpenArtifact}
              />
            )}
            {unknown.length > 0 && (
              <ArtifactSection
                label="Workspace files (origin not recorded)"
                artifacts={unknown}
                countKind="file"
                view={view}
                onOpenArtifact={onOpenArtifact}
              />
            )}
          </>
        )}
      </div>
    </>
  )
}

function ArtifactSection({
  label,
  artifacts,
  countKind,
  view,
  onOpenArtifact,
}: {
  label: string
  artifacts: Artifact[]
  countKind: 'file' | 'artifact'
  view: 'grid' | 'list'
  onOpenArtifact: (a: Artifact) => void
}) {
  return (
    <section className="mb-4 last:mb-0">
      <SectionLabel left={label} right={formatCount(artifacts.length, countKind)} />
      <div className={view === 'grid' ? 'grid grid-cols-3 gap-2' : 'space-y-2'}>
        {artifacts.map((artifact) => (
          <ArtifactCard key={artifact.path} artifact={artifact} view={view} onOpen={onOpenArtifact} />
        ))}
      </div>
    </section>
  )
}

function SectionLabel({ left, right }: { left: string; right: string }) {
  return (
    <div className="mb-2 flex items-center justify-between gap-2">
      <span className="truncate text-[12px] font-medium text-ink">{left}</span>
      <span className="shrink-0 text-[11px] text-ink3">{right}</span>
    </div>
  )
}

function ArtifactCard({
  artifact,
  view,
  onOpen,
}: {
  artifact: Artifact
  view: 'grid' | 'list'
  onOpen: (artifact: Artifact) => void
}) {
  const sub = `${formatSize(artifact.size)} · ${artifactOriginLabel(artifact.origin)}`

  if (view === 'list') {
    return (
      <button
        className="flex w-full items-center gap-2.5 rounded-md border border-border bg-bg px-3 py-2 text-left hover:bg-surface"
        onClick={() => onOpen(artifact)}
      >
        <IconFile width={14} height={14} className="shrink-0 text-ink3" />
        <span className="min-w-0 flex-1">
          <span className="block truncate font-mono text-[12px] text-ink">{artifact.path}</span>
          <span className="block text-[11px] text-ink3">{sub}</span>
        </span>
      </button>
    )
  }

  return (
    <button
      className="overflow-hidden rounded-md border border-border bg-bg text-left hover:border-borderStrong"
      onClick={() => onOpen(artifact)}
      title={artifact.path}
    >
      <div className="flex h-24 flex-col items-center justify-center gap-2 bg-surface p-2 text-ink3">
        <IconFile width={28} height={28} />
        <span className="rounded bg-surface2 px-1.5 py-0.5 font-mono text-[10px] uppercase">{artifact.kind}</span>
      </div>
      <div className="border-t border-border px-2 py-1.5">
        <div className="truncate font-mono text-[11px] text-ink">{artifact.path}</div>
        <div className="text-[10px] text-ink3">{sub}</div>
      </div>
    </button>
  )
}

export function artifactOriginLabel(origin: Artifact['origin']): string {
  switch (origin) {
    case 'agent':
      return 'Created by agent'
    case 'upload':
      return 'Uploaded'
    case 'unknown':
      return 'Workspace file'
    default: {
      const exhaustive: never = origin
      return exhaustive
    }
  }
}

function PanelStatus({ children, tone = 'muted' }: { children: React.ReactNode; tone?: 'muted' | 'error' }) {
  return (
    <div
      className={`rounded-md border border-dashed p-6 text-center text-[12px] ${
        tone === 'error' ? 'border-red-200 text-red-600' : 'border-border text-ink3'
      }`}
    >
      {children}
    </div>
  )
}

function formatSize(size: number): string {
  if (size < 1024) return `${size} B`
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(size < 10 * 1024 ? 1 : 0)} KB`
  return `${(size / (1024 * 1024)).toFixed(1)} MB`
}

function formatCount(count: number, kind: 'file' | 'artifact'): string {
  return `${count} ${kind}${count === 1 ? '' : 's'}`
}
