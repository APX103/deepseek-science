// PDF 预览弹层：鉴权读取当前会话 workspace 中的真实 PDF，再用临时 Blob URL 展示。
import { useEffect, useState } from 'react'
import { readFileBlob } from '../api/client'
import type { Artifact } from '../types'
import { IconDownload, IconExpand, IconX } from './icons'

interface Props {
  sid: string
  artifact: Artifact
  onClose: () => void
}

export default function PdfPreviewModal({ sid, artifact, onClose }: Props) {
  const [src, setSrc] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const name = artifact.path.split('/').pop() || 'artifact.pdf'

  useEffect(() => {
    let cancelled = false
    let objectUrl: string | null = null
    setSrc(null)
    setError(null)

    void readFileBlob(sid, artifact.path)
      .then((blob) => {
        if (cancelled) return
        objectUrl = URL.createObjectURL(blob)
        setSrc(objectUrl)
      })
      .catch((cause: unknown) => {
        if (!cancelled) setError(cause instanceof Error ? cause.message : String(cause))
      })

    return () => {
      cancelled = true
      if (objectUrl) URL.revokeObjectURL(objectUrl)
    }
  }, [artifact.path, sid])

  return (
    <div className="fixed inset-0 z-50 flex flex-col bg-bg" onMouseDown={onClose}>
      <div
        className="flex h-11 shrink-0 items-center gap-3 border-b border-border px-4"
        onMouseDown={(event) => event.stopPropagation()}
      >
        <span className="min-w-0 truncate font-mono text-[13px] text-ink2">{artifact.path}</span>
        <span className="shrink-0 text-[12px] text-ink3">{formatSize(artifact.size)}</span>
        <div className="ml-auto flex shrink-0 items-center gap-1">
          {src ? (
            <>
              <a
                href={src}
                target="_blank"
                rel="noreferrer"
                className="btn-ghost rounded p-1.5"
                title="在新窗口打开"
                aria-label="在新窗口打开 PDF"
              >
                <IconExpand width={14} height={14} />
              </a>
              <a
                href={src}
                download={name}
                className="btn-ghost rounded p-1.5"
                title="下载 PDF"
                aria-label="下载 PDF"
              >
                <IconDownload width={14} height={14} />
              </a>
            </>
          ) : null}
          <button className="btn-ghost rounded p-1.5" onClick={onClose} aria-label="关闭">
            <IconX width={14} height={14} />
          </button>
        </div>
      </div>

      <div className="min-h-0 flex-1 bg-surface p-3" onMouseDown={(event) => event.stopPropagation()}>
        {src ? (
          <iframe
            key={src}
            title={`PDF preview: ${artifact.path}`}
            src={src}
            className="h-full w-full rounded-sm border border-border bg-bg shadow-subtle"
          />
        ) : (
          <div className="flex h-full items-center justify-center rounded-sm border border-border bg-bg text-[13px] text-ink3">
            {error ?? '正在安全加载 PDF…'}
          </div>
        )}
      </div>
    </div>
  )
}

function formatSize(size: number): string {
  if (size < 1024) return `${size} B`
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(size < 10 * 1024 ? 1 : 0)} KB`
  return `${(size / (1024 * 1024)).toFixed(1)} MB`
}
