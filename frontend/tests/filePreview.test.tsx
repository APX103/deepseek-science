import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import FilePreviewModal, {
  SourcePreview,
  copyPreviewSource,
  filePreviewMode,
  initialDisplayMode,
} from '../src/components/FilePreviewModal'
import { filePreviewKind } from '../src/api/filePreview'
import PreviewTitleBar from '../src/components/PreviewTitleBar'

function buttonMarkup(html: string, label: string): string {
  const button = (html.match(/<button\b[\s\S]*?<\/button>/g) ?? []).find((candidate) =>
    candidate.includes(label),
  )
  expect(button).toBeDefined()
  return button ?? ''
}

describe('file preview modes', () => {
  test('recognizes every supported Markdown extension case-insensitively', () => {
    for (const path of [
      'report.md',
      'REPORT.MD',
      'notes.MarkDown',
      'nested/research.MDX',
    ]) {
      expect(filePreviewMode(path)).toBe('markdown')
    }

    for (const path of ['main.tex', 'paper.pdf', 'data.json', 'README', 'notes.md.txt']) {
      expect(filePreviewMode(path)).toBeNull()
    }
  })

  test('defaults renderable Markdown to rendered and every other file to source', () => {
    expect(initialDisplayMode('report.md')).toBe('rendered')
    expect(initialDisplayMode('REPORT.MDX')).toBe('rendered')
    expect(initialDisplayMode('main.tex')).toBe('source')
    expect(initialDisplayMode('script.py')).toBe('source')
  })

  test('routes PDF and image extensions case-insensitively before the text fallback', () => {
    expect(filePreviewKind('paper.PDF')).toBe('pdf')
    expect(filePreviewKind('figures/result.WeBp')).toBe('image')
    expect(filePreviewKind('diagram.SVG')).toBe('image')
    expect(filePreviewKind('main.tex')).toBe('text')
  })

  test('marks Preview as the selected default in Markdown modal SSR', () => {
    const html = renderToStaticMarkup(
      <FilePreviewModal
        sid="session-preview"
        file={{ path: 'report.md', name: 'report.md', size: 128 }}
        onClose={() => {}}
      />,
    )

    expect(buttonMarkup(html, '预览')).toContain('aria-pressed="true"')
    expect(buttonMarkup(html, '源码')).toContain('aria-pressed="false"')
    expect(html).not.toContain('data-preview-title-bar')
  })

  test('renders the shared full-window title bar with a fixed inset and two bare drag surfaces', () => {
    const path = 'results/a-very-long-preview-name.pdf'
    const html = renderToStaticMarkup(
      <PreviewTitleBar path={path} size={1536}>
        <button type="button" aria-label="测试关闭">
          close
        </button>
      </PreviewTitleBar>,
    )

    expect(html).toContain('data-preview-title-bar="true"')
    expect(html).toContain('style="padding-left:76px"')
    expect(html.match(/data-tauri-drag-region="true"/g)).toHaveLength(2)
    expect(html).toContain(path)
    expect(html).toContain('1.5 KB')
    expect(buttonMarkup(html, '测试关闭')).not.toContain('data-tauri-drag-region')
  })
})

describe('file preview source copy', () => {
  test('renders source verbatim with an explicit copy-source button', () => {
    const source = '# title\n\nconst answer = 42 < 100'
    const html = renderToStaticMarkup(<SourcePreview content={source} />)

    expect(buttonMarkup(html, '复制源码')).toContain('复制源码')
    expect(html).toContain('# title')
    expect(html).toContain('const answer = 42 &lt; 100')
  })

  test('copies the exact source through the supplied clipboard', async () => {
    const writes: string[] = []
    const source = 'line one\nline two\n'

    await copyPreviewSource(source, {
      writeText: async (text: string) => {
        writes.push(text)
      },
    })

    expect(writes).toEqual([source])
  })

  test('propagates clipboard rejection instead of reporting a false success', async () => {
    const denied = new Error('clipboard permission denied')
    let observed: unknown

    try {
      await copyPreviewSource('sensitive source', {
        writeText: async () => {
          throw denied
        },
      })
    } catch (error) {
      observed = error
    }

    expect(observed).toBe(denied)
  })
})
