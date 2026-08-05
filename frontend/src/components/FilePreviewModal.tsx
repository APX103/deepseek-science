// 文本文件预览弹层：按会话从真实 workspace file API 加载内容。
import { useEffect, useState } from 'react'
import { readFile } from '../api/client'
import type { WorkspaceFile } from '../types'
import Modal from './Modal'

interface Props {
  sid: string
  file: WorkspaceFile
  onClose: () => void
}

export default function FilePreviewModal({ sid, file, onClose }: Props) {
  const [content, setContent] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    let cancelled = false
    setContent(null)
    setLoading(true)
    setError(null)

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

  return (
    <Modal title={file.path} onClose={onClose} width="max-w-3xl">
      <div className="max-h-[70vh] min-h-40 overflow-auto p-4">
        {loading ? (
          <p className="text-[13px] text-ink3">正在加载文件内容…</p>
        ) : error ? (
          <div className="rounded-md border border-red-200 bg-red-50 p-3 text-[13px] text-red-700">
            无法读取该文件：{error}
          </div>
        ) : content === '' ? (
          <p className="text-[13px] text-ink3">该文件为空。</p>
        ) : (
          <pre className="whitespace-pre-wrap break-words font-mono text-[12px] leading-[1.7] text-ink2">
            {content ?? ''}
          </pre>
        )}
      </div>
    </Modal>
  )
}
