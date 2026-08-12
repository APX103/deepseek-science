// PDF 预览弹层：鉴权读取当前会话 workspace 中的真实 PDF，再用临时 Blob URL 展示。
import { useEffect, useState } from 'react'
import { readFileBlob } from '../api/client'
import type { Artifact } from '../types'
import { IconDownload, IconExpand, IconX } from './icons'
import PreviewTitleBar from './PreviewTitleBar'

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
    <div
      className="fixed inset-0 z-50 flex flex-col bg-bg"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose()
      }}
    >
      <PreviewTitleBar path={artifact.path} size={artifact.size}>
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
      </PreviewTitleBar>

      <div className="min-h-0 flex-1 bg-surface p-3">
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
