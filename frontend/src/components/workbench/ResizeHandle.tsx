// 栏宽拖拽 handle：贴在分隔线上的 5px 热区，hover/拖拽时显示品牌蓝 1px 线。
import { useRef, useState } from 'react'

interface Props {
  /** left：拖右缘调左栏宽；right：拖左缘调右栏宽 */
  side: 'left' | 'right'
  value: number
  min: number
  max: number
  onChange: (v: number) => void
}

export default function ResizeHandle({ side, value, min, max, onChange }: Props) {
  const [dragging, setDragging] = useState(false)
  const start = useRef<{ x: number; w: number } | null>(null)

  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    e.preventDefault()
    start.current = { x: e.clientX, w: value }
    setDragging(true)
    document.body.style.userSelect = 'none'

    const move = (ev: PointerEvent) => {
      if (!start.current) return
      const dx = ev.clientX - start.current.x
      const w = side === 'left' ? start.current.w + dx : start.current.w - dx
      onChange(Math.min(max, Math.max(min, Math.round(w))))
    }
    const up = () => {
      start.current = null
      setDragging(false)
      document.body.style.userSelect = ''
      window.removeEventListener('pointermove', move)
    }
    window.addEventListener('pointermove', move)
    window.addEventListener('pointerup', up, { once: true })
  }

  return (
    <div
      className="group relative z-10 -mx-[2px] w-[5px] shrink-0 cursor-col-resize"
      onPointerDown={onPointerDown}
    >
      <div
        className={`absolute inset-y-0 left-1/2 w-px ${
          dragging ? 'bg-brand' : 'bg-transparent group-hover:bg-brand'
        }`}
      />
    </div>
  )
}
