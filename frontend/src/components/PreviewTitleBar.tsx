import type { ReactNode } from 'react'
import { MAC_TRAFFIC_LIGHT_INSET } from '../windowChrome'

interface Props {
  path: string
  size: number
  children: ReactNode
}

export default function PreviewTitleBar({ path, size, children }: Props) {
  return (
    <div
      data-preview-title-bar
      data-tauri-drag-region
      className="flex h-11 shrink-0 select-none items-center gap-3 border-b border-border pr-4"
      style={{ paddingLeft: MAC_TRAFFIC_LIGHT_INSET }}
    >
      <span className="min-w-0 truncate font-mono text-[13px] text-ink2">{path}</span>
      <span className="shrink-0 text-[12px] text-ink3">{formatSize(size)}</span>
      <div className="h-full flex-1" data-tauri-drag-region />
      <div className="ml-auto flex shrink-0 items-center gap-1">{children}</div>
    </div>
  )
}

function formatSize(size: number): string {
  if (size < 1024) return `${size} B`
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(size < 10 * 1024 ? 1 : 0)} KB`
  return `${(size / (1024 * 1024)).toFixed(1)} MB`
}
