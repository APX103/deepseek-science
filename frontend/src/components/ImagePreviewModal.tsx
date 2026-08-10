// 图片预览弹层：鉴权读取当前会话 workspace 中的真实图片，并用临时 Blob URL 安全展示。
import { useEffect, useState } from 'react'
import { readFileBlob } from '../api/client'
import type { WorkspaceFile } from '../types'
import { IconDownload, IconX } from './icons'

interface Props {
  sid: string
  file: WorkspaceFile
  onClose: () => void
}

export default function ImagePreviewModal({ sid, file, onClose }: Props) {
  const [src, setSrc] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const name = file.name || file.path.split('/').pop() || 'image'

  useEffect(() => {
    let cancelled = false
    let objectUrl: string | null = null
    setSrc(null)
    setError(null)

    void readFileBlob(sid, file.path)
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
  }, [file.path, sid])

  return (
    <div className="fixed inset-0 z-50 flex flex-col bg-bg" onMouseDown={onClose}>
      <div
        className="flex h-11 shrink-0 items-center gap-3 border-b border-border px-4"
        onMouseDown={(event) => event.stopPropagation()}
      >
        <span className="min-w-0 truncate font-mono text-[13px] text-ink2">{file.path}</span>
        <span className="shrink-0 text-[12px] text-ink3">{formatSize(file.size)}</span>
        <div className="ml-auto flex shrink-0 items-center gap-1">
          {src && !error ? (
            <a
              href={src}
              download={name}
              className="btn-ghost rounded p-1.5"
              title="下载图片"
              aria-label={`下载图片 ${name}`}
            >
              <IconDownload width={14} height={14} />
            </a>
          ) : null}
          <button className="btn-ghost rounded p-1.5" onClick={onClose} aria-label="关闭图片预览">
            <IconX width={14} height={14} />
          </button>
        </div>
      </div>

      <div
        className="flex min-h-0 flex-1 items-center justify-center overflow-auto bg-surface p-6"
        onMouseDown={(event) => event.stopPropagation()}
      >
        {error ? (
          <div className="rounded-md border border-red-200 bg-red-50 p-3 text-[13px] text-red-700">
            无法预览该图片：{error}
          </div>
        ) : src ? (
          <img
            src={src}
            alt={`图片预览：${file.path}`}
            draggable={false}
            className="max-h-full max-w-full rounded-sm object-contain shadow-subtle"
            onError={() => setError('文件不是受支持的图片，或图片内容已损坏')}
          />
        ) : (
          <p className="text-[13px] text-ink3">正在安全加载图片…</p>
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
