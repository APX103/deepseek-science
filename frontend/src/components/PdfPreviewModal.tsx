// PDF 预览弹层：点 artifact 卡片打开。第一版用占位排版模拟 PDF 渲染（不接 pdfjs）。
import type { Artifact } from '../types'
import { IconDownload, IconX } from './icons'

interface Props {
  artifact: Artifact
  onClose: () => void
}

export default function PdfPreviewModal({ artifact, onClose }: Props) {
  return (
    <div className="fixed inset-0 z-50 flex flex-col bg-bg" onMouseDown={onClose}>
      {/* 顶栏 */}
      <div
        className="flex h-11 shrink-0 items-center gap-3 border-b border-border px-4"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <span className="font-mono text-[13px] text-ink2">{artifact.path}</span>
        <span className="text-[12px] text-ink3">{(artifact.size / 1024).toFixed(0)} KB</span>
        <div className="ml-auto flex items-center gap-1">
          <button className="btn-ghost rounded p-1.5" title="下载（TODO）">
            <IconDownload width={14} height={14} />
          </button>
          <button className="btn-ghost rounded p-1.5" onClick={onClose} aria-label="关闭">
            <IconX width={14} height={14} />
          </button>
        </div>
      </div>

      {/* 页面区：灰底 + 白色纸张占位排版 */}
      <div
        className="flex flex-1 justify-center overflow-y-auto bg-surface py-8"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <div className="h-fit w-[640px] shrink-0 rounded-sm border border-border bg-bg px-14 py-12 shadow-subtle">
          <h1 className="text-center text-[20px] font-semibold leading-[1.3]">
            新型绿色无铅钙钛矿材料
            <br />
            在太阳电池领域的应用研究进展
          </h1>
          <p className="mt-3 text-center text-[13px] text-ink2">综述</p>
          <p className="text-center text-[12px] text-ink3">2026年7月</p>

          <PlaceholderParagraph title="摘要" lines={5} />
          <PlaceholderParagraph title="1 引言" lines={7} />
          <PlaceholderParagraph title="2 Sn 基钙钛矿" lines={4} />
          <PlaceholderParagraph title="2.1 结构与光电性质" lines={6} />

          <p className="mt-8 text-center text-[11px] text-ink3">
            占位预览 — 后续版本接入 Tectonic 编译产物 + pdfjs 渲染
          </p>
        </div>
      </div>
    </div>
  )
}

function PlaceholderParagraph({ title, lines }: { title: string; lines: number }) {
  return (
    <div className="mt-6">
      <h2 className="text-[14px] font-semibold">{title}</h2>
      <div className="mt-2 space-y-1.5">
        {Array.from({ length: lines }).map((_, i) => (
          <div
            key={i}
            className="h-2 rounded-sm bg-surface2"
            style={{ width: i === lines - 1 ? '62%' : '100%' }}
          />
        ))}
      </div>
    </div>
  )
}
