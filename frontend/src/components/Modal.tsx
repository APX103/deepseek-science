// 通用弹层容器：遮罩 + 居中卡片，Esc 关闭。
import { useEffect, type ReactNode } from 'react'
import { IconX } from './icons'

interface Props {
  title?: string
  onClose: () => void
  children: ReactNode
  width?: string
}

export default function Modal({ title, onClose, children, width = 'max-w-lg' }: Props) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose()
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [onClose])

  return (
    <div
      className="fixed inset-0 z-50 flex items-start justify-center bg-black/20 pt-[12vh]"
      onMouseDown={onClose}
    >
      <div
        className={`w-full ${width} rounded-xl border border-border bg-bg shadow-overlay`}
        onMouseDown={(e) => e.stopPropagation()}
      >
        {title !== undefined && (
          <div className="flex items-center justify-between border-b border-border px-4 py-3">
            <h2 className="text-[14px] font-semibold">{title}</h2>
            <button className="btn-ghost rounded p-1" onClick={onClose} aria-label="关闭">
              <IconX width={14} height={14} />
            </button>
          </div>
        )}
        {children}
      </div>
    </div>
  )
}
