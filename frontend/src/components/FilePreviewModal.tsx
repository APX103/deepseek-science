// 文本文件预览弹层：可渲染格式默认预览，并可切换到可复制源码。
import { useEffect, useMemo, useState } from 'react'
import { readFile } from '../api/client'
import type { WorkspaceFile } from '../types'
import MarkdownContent from './MarkdownContent'
import Modal from './Modal'

interface Props {
  sid: string
  file: WorkspaceFile
  onClose: () => void
}

type PreviewRenderer = 'markdown'
type DisplayMode = 'rendered' | 'source'

export function filePreviewMode(path: string): PreviewRenderer | null {
  const ext = path.split('.').pop()?.toLowerCase()
  if (ext === 'md' || ext === 'markdown' || ext === 'mdx') return 'markdown'
  return null
}

export function initialDisplayMode(path: string): DisplayMode {
  return filePreviewMode(path) ? 'rendered' : 'source'
}

interface ClipboardWriter {
  writeText(text: string): Promise<void>
}

export async function copyPreviewSource(content: string, clipboard?: ClipboardWriter): Promise<void> {
  const writer = clipboard ?? (typeof navigator !== 'undefined' ? navigator.clipboard : undefined)
  if (!writer?.writeText) throw new Error('clipboard unavailable')
  await writer.writeText(content)
}

export function SourcePreview({ content }: { content: string }) {
  const [copyState, setCopyState] = useState<'idle' | 'copying' | 'copied' | 'error'>('idle')

  useEffect(() => {
    if (copyState !== 'copied') return
    const timer = window.setTimeout(() => setCopyState('idle'), 1800)
    return () => window.clearTimeout(timer)
  }, [copyState])

  const copySource = async () => {
    setCopyState('copying')
    try {
      await copyPreviewSource(content)
      setCopyState('copied')
    } catch {
      setCopyState('error')
    }
  }

  return (
    <div className="overflow-hidden rounded-md border border-border bg-surface" data-file-source="true">
      <div className="flex min-h-9 items-center justify-between gap-3 border-b border-border px-3 py-1.5">
        <span className="text-[10px] font-medium uppercase tracking-wide text-ink3">源码</span>
        <div className="flex items-center gap-2">
          {copyState === 'error' && (
            <span className="text-[11px] text-red-600" role="status">
              复制失败，请检查剪贴板权限
            </span>
          )}
          <button
            type="button"
            className="btn-ghost rounded px-2 py-1 text-[11px]"
            onClick={() => void copySource()}
            disabled={copyState === 'copying'}
            aria-label="复制源码"
          >
            {copyState === 'copied' ? '已复制' : copyState === 'copying' ? '正在复制…' : '复制源码'}
          </button>
        </div>
      </div>
      <pre className="overflow-auto p-3 font-mono text-[12px] leading-[1.7] text-ink2">
        <code>{content}</code>
      </pre>
    </div>
  )
}

export default function FilePreviewModal({ sid, file, onClose }: Props) {
  const [content, setContent] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const renderer = useMemo(() => filePreviewMode(file.path), [file.path])
  const [displayMode, setDisplayMode] = useState<DisplayMode>(() => initialDisplayMode(file.path))

  useEffect(() => {
    let cancelled = false
    setContent(null)
    setLoading(true)
    setError(null)
    setDisplayMode(initialDisplayMode(file.path))

    void readFile(sid, file.path)
      .then((text) => {
        if (!cancelled) setContent(text)
      })
      .catch((err: unknown) => {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err))
      })
      .finally(() => {
        if (!cancelled) setLoading(false)
      })

    return () => {
      cancelled = true
    }
  }, [sid, file.path])

  const showRendered = displayMode === 'rendered' && renderer === 'markdown'

  return (
    <Modal title={file.path} onClose={onClose} width="max-w-3xl">
      <div className="flex max-h-[70vh] min-h-40 flex-col">
        {renderer && (
          <div className="flex items-center justify-end border-b border-border px-4 py-2">
            <div
              className="inline-flex rounded-md border border-border bg-surface p-0.5"
              role="group"
              aria-label="文件显示模式"
            >
              <button
                type="button"
                className={`rounded px-2.5 py-1 text-[12px] transition-colors ${
                  showRendered ? 'bg-bg font-medium text-ink shadow-subtle' : 'text-ink3 hover:text-ink2'
                }`}
                aria-pressed={showRendered}
                onClick={() => setDisplayMode('rendered')}
              >
                预览
              </button>
              <button
                type="button"
                className={`rounded px-2.5 py-1 text-[12px] transition-colors ${
                  !showRendered ? 'bg-bg font-medium text-ink shadow-subtle' : 'text-ink3 hover:text-ink2'
                }`}
                aria-pressed={!showRendered}
                onClick={() => setDisplayMode('source')}
              >
                源码
              </button>
            </div>
          </div>
        )}
        <div className="flex-1 overflow-auto p-4" data-preview-mode={showRendered ? 'rendered' : 'source'}>
          {loading ? (
            <p className="text-[13px] text-ink3">正在加载文件内容…</p>
          ) : error ? (
            <div className="rounded-md border border-red-200 bg-red-50 p-3 text-[13px] text-red-700">
              无法读取该文件：{error}
            </div>
          ) : content === '' ? (
            <p className="text-[13px] text-ink3">该文件为空。</p>
          ) : showRendered ? (
            <MarkdownContent content={content ?? ''} />
          ) : (
            <SourcePreview content={content ?? ''} />
          )}
        </div>
      </div>
    </Modal>
  )
}
