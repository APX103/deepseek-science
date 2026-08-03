// 简单 dropdown：点击外部关闭。
import { useEffect, useRef, useState, type ReactNode } from 'react'

export interface DropdownItem {
  label: string
  icon?: ReactNode
  danger?: boolean
  onClick: () => void
}

interface Props {
  trigger: ReactNode
  items: DropdownItem[]
  align?: 'left' | 'right'
}

export default function Dropdown({ trigger, items, align = 'right' }: Props) {
  const [open, setOpen] = useState(false)
  const ref = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!open) return
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false)
    }
    document.addEventListener('mousedown', onDown)
    return () => document.removeEventListener('mousedown', onDown)
  }, [open])

  return (
    <div className="relative" ref={ref}>
      <div onClick={() => setOpen((v) => !v)}>{trigger}</div>
      {open && (
        <div
          className={`absolute z-40 mt-1 min-w-[160px] rounded-md border border-border bg-bg py-1 shadow-overlay ${
            align === 'right' ? 'right-0' : 'left-0'
          }`}
        >
          {items.map((it) => (
            <button
              key={it.label}
              className={`flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13px] hover:bg-surface2 ${
                it.danger ? 'text-danger' : 'text-ink'
              }`}
              onClick={() => {
                setOpen(false)
                it.onClick()
              }}
            >
              {it.icon}
              {it.label}
            </button>
          ))}
        </div>
      )}
    </div>
  )
}
