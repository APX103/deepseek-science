import { Children, isValidElement, memo } from 'react'
import Markdown, { type Components } from 'react-markdown'
import rehypeKatex from 'rehype-katex'
import remarkBreaks from 'remark-breaks'
import remarkGemoji from 'remark-gemoji'
import remarkGfm from 'remark-gfm'
import remarkMath from 'remark-math'
import { openUrl } from '@tauri-apps/plugin-opener'
import 'katex/dist/katex.min.css'

interface Props {
  content: string
}

const SAFE_FRAGMENT = /^#[A-Za-z0-9][A-Za-z0-9_.:%-]*$/

/**
 * Markdown links must never become navigation inside the privileged App webview.
 * Absolute HTTP(S) URLs open through the scoped Tauri opener; local fragments are
 * retained for generated footnotes. Everything else becomes non-clickable text.
 */
export function safeMarkdownUrl(value: string): string {
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

async function openExternalUrl(url: string) {
  try {
    await openUrl(url)
  } catch {
    // Browser-only development has no Tauri IPC. Keep the packaged App fail-closed,
    // but preserve a useful preview workflow without ever navigating this tab.
    if (typeof window !== 'undefined' && !('__TAURI_INTERNALS__' in window)) {
      window.open(url, '_blank', 'noopener,noreferrer')
    }
  }
}

const components: Components = {
  a({ node: _node, href, children, ...props }) {
    if (!href) {
      return (
        <span className="agent-markdown-blocked-link" data-markdown-link-blocked="true">
          {children}
        </span>
      )
    }
    if (href.startsWith('#')) {
      return (
        <a {...props} href={href}>
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
        data-external-url={href}
        onClick={() => void openExternalUrl(href)}
      >
        {children}
      </button>
    )
  },
  img({ node: _node, alt }) {
    return (
      <span className="agent-markdown-image-placeholder" data-markdown-image-blocked="true" role="note">
        🖼 {alt?.trim() || '远程图片未自动加载'}
      </span>
    )
  },
  input({ node: _node, ...props }) {
    return <input {...props} disabled />
  },
  table({ node: _node, children, ...props }) {
    return (
      <div className="agent-markdown-table-wrap" role="region" aria-label="可横向滚动的表格" tabIndex={0}>
        <table {...props}>{children}</table>
      </div>
    )
  },
  pre({ node: _node, children, ...props }) {
    const child = Children.toArray(children)[0]
    const className = isValidElement<{ className?: string }>(child) ? child.props.className : undefined
    const language = /(?:^|\s)language-([^\s]+)/.exec(className ?? '')?.[1]?.slice(0, 32) || 'text'
    return (
      <div className="agent-markdown-code-frame" data-markdown-code-language={language}>
        <div className="agent-markdown-code-label">{language}</div>
        <pre {...props}>{children}</pre>
      </div>
    )
  },
  code({ node: _node, ...props }) {
    return <code {...props} />
  },
}

/** Safe Markdown presentation shared by chat and workspace-file previews. */
function MarkdownContent({ content }: Props) {
  return (
    <div className="agent-markdown" data-agent-markdown="true">
      <Markdown
        components={components}
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
}

export default memo(MarkdownContent)
