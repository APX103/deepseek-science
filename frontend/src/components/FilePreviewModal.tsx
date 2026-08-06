// 文本文件预览弹层：支持 Markdown 渲染/源码切换。
// 设计为可扩展：后续接入 LaTeX 编译器后可把 .tex 加入 filePreviewMode。
import { memo } from 'react'
import { useEffect, useMemo, useState } from 'react'
import Markdown from 'react-markdown'
import rehypeKatex from 'rehype-katex'
import remarkBreaks from 'remark-breaks'
import remarkGemoji from 'remark-gemoji'
import remarkGfm from 'remark-gfm'
import remarkMath from 'remark-math'
import { readFile } from '../api/client'
import type { WorkspaceFile } from '../types'
import Modal from './Modal'
import Toggle from './Toggle'
import 'katex/dist/katex.min.css'

interface Props {
  sid: string
  file: WorkspaceFile
  onClose: () => void
}

type PreviewMode = 'source' | 'markdown'

function filePreviewMode(path: string): PreviewMode | null {
  const ext = path.split('.').pop()?.toLowerCase()
  if (ext === 'md' || ext === 'markdown' || ext === 'mdx') return 'markdown'
  return null
}

const SAFE_FRAGMENT = /^#[A-Za-z0-9][A-Za-z0-9_.:%-]*$/

function safeMarkdownUrl(value: string): string {
  const trimmed = value.trim()
  if (SAFE_FRAGMENT.test(trimmed)) return trimmed
  if (!/^https?:\/\//i.test(trimmed)) return ''
  try {
    const parsed = new URL(trimmed)
    return parsed.protocol === 'http:' || parsed.protocol === 'https:' ? parsed.href : ''
  } catch {
    return ''
  }
}

const markdownComponents = {
  a({ href, children, ...props }: { href?: string; children?: React.ReactNode; className?: string }) {
    if (!href) {
      return <span className="agent-markdown-blocked-link">{children}</span>
    }
    if (href.startsWith('#')) {
      return (
        <a href={href} {...props}>
          {children}
        </a>
      )
    }
    return (
      <button
        type="button"
        role="link"
        className={`agent-markdown-external-link ${props.className ?? ''}`.trim()}
        title="在系统浏览器中打开"
        onClick={() => {
          if (typeof window !== 'undefined' && !('__TAURI_INTERNALS__' in window)) {
            window.open(href, '_blank', 'noopener,noreferrer')
          }
        }}
      >
        {children}
      </button>
    )
  },
  img({ alt }: { alt?: string }) {
    return (
      <span className="agent-markdown-image-placeholder" role="note">
        🖼 {alt?.trim() || '远程图片未自动加载'}
      </span>
    )
  },
  table({ children, ...props }: { children?: React.ReactNode; className?: string }) {
    return (
      <div className="agent-markdown-table-wrap" role="region" aria-label="可横向滚动的表格" tabIndex={0}>
        <table {...props}>{children}</table>
      </div>
    )
  },
}

const RenderedMarkdown = memo(function RenderedMarkdown({ content }: { content: string }) {
  return (
    <div className="agent-markdown" data-agent-markdown="true">
      <Markdown
        components={markdownComponents as any}
        remarkPlugins={[
          [remarkGfm, { singleTilde: false }],
          remarkBreaks,
          remarkGemoji,
          remarkMath,
        ]}
        rehypePlugins={[
          [
            rehypeKatex,
            {
              trust: false,
              strict: 'warn',
              throwOnError: false,
              maxExpand: 1000,
              maxSize: 20,
            },
          ],
        ]}
        skipHtml
        urlTransform={safeMarkdownUrl}
      >
        {content}
      </Markdown>
    </div>
  )
})

export default function FilePreviewModal({ sid, file, onClose }: Props) {
  const [content, setContent] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const defaultMode = useMemo(() => filePreviewMode(file.path), [file.path])
  const [rendered, setRendered] = useState(defaultMode !== null)

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

  const showRendered = rendered && defaultMode === 'markdown'

  return (
    <Modal title={file.path} onClose={onClose} width="max-w-3xl">
      <div className="flex max-h-[70vh] min-h-40 flex-col">
        {defaultMode && (
          <div className="flex items-center justify-end gap-2 border-b border-border px-4 py-2">
            <span className={`text-[12px] ${showRendered ? 'text-ink' : 'text-ink3'}`}>渲染</span>
            <Toggle checked={rendered} onChange={setRendered} />
            <span className={`text-[12px] ${!showRendered ? 'text-ink' : 'text-ink3'}`}>源码</span>
          </div>
        )}
        <div className="flex-1 overflow-auto p-4">
          {loading ? (
            <p className="text-[13px] text-ink3">正在加载文件内容…</p>
          ) : error ? (
            <div className="rounded-md border border-red-200 bg-red-50 p-3 text-[13px] text-red-700">
              无法读取该文件：{error}
            </div>
          ) : content === '' ? (
            <p className="text-[13px] text-ink3">该文件为空。</p>
          ) : showRendered ? (
            <RenderedMarkdown content={content ?? ''} />
          ) : (
            <pre className="whitespace-pre-wrap break-words font-mono text-[12px] leading-[1.7] text-ink2">
              {content ?? ''}
            </pre>
          )}
        </div>
      </div>
    </Modal>
  )
}
