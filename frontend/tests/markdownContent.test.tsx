import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import AgentMarkdown, { safeMarkdownUrl } from '../src/components/workbench/AgentMarkdown'
import dsmlDisplayCorpus from '../../test-fixtures/dsml-display-corpus.json'
import {
  HIDDEN_ASSISTANT_PROTOCOL_NOTICE,
  MAX_ASSISTANT_DISPLAY_TEXT_LENGTH,
  sanitizeAssistantDisplayText,
} from '../src/api/assistantProtocol'

const RESEARCH_MARKDOWN = `# 研究结论 🚀

第一行  
第二行含 **粗体**、*斜体*、~~旧结论~~，尺度约为 ~5 nm，并支持 :microscope:。

## 结构

> 可重复性优先。

- 无序列表
  - 嵌套项
- [x] 已验证
- [ ] 待验证

1. 第一步
2. 第二步

| 指标 | 结果 |
| --- | ---: |
| 准确率 | 99% |

行内代码 \`const n = 1\` 与公式 $E = mc^2$。

\`\`\`typescript
const answer: number = 42
\`\`\`

$$
\\int_0^1 x^2 \\, dx = \\frac{1}{3}
$$

[官方资料](https://example.com/docs?q=markdown) 与脚注[^1]。

[^1]: 脚注正文。

---
`

const RAW_PYTHON_DSML = `<｜｜DSML｜｜tool_calls>
<｜｜DSML｜｜invoke name="python">
<｜｜DSML｜｜parameter name="code" string="true">
# 3. agenda checks
print("must remain hidden")
</｜｜DSML｜｜parameter>
</｜｜DSML｜｜invoke>
</｜｜DSML｜｜tool_calls>`

const COMMONMARK_SHIELD_CASES = [
  {
    source: dsmlDisplayCorpus.regressions.paragraph_indented_protocol,
    secret: 'INDENT_SECRET',
  },
  {
    source: dsmlDisplayCorpus.regressions.escaped_backticks_protocol,
    secret: 'ESCAPED_SECRET',
  },
]

describe('AgentMarkdown', () => {
  test('renders CommonMark, GFM, emoji, line breaks, code and scientific math', () => {
    const html = renderToStaticMarkup(<AgentMarkdown content={RESEARCH_MARKDOWN} />)

    expect(html).toContain('<h1>研究结论 🚀</h1>')
    expect(html).toContain('<h2>结构</h2>')
    expect(html).toContain('<strong>粗体</strong>')
    expect(html).toContain('<em>斜体</em>')
    expect(html).toContain('<del>旧结论</del>')
    expect(html).toContain('尺度约为 ~5 nm')
    expect(html).toContain('🔬')
    expect(html).toContain('<blockquote>')
    expect(html).toContain('<ol>')
    expect(html).toContain('type="checkbox"')
    expect(html).toContain('disabled=""')
    expect(html).toContain('agent-markdown-table-wrap')
    expect(html).toContain('<table>')
    expect(html).toContain('<hr/>')
    expect(html).toContain('<br/>')
    expect(html).toContain('data-markdown-code-language="typescript"')
    expect(html).toContain('class="katex"')
    expect(html).toContain('class="katex-display"')
    expect(html).toContain('href="#user-content-fn-1"')
    expect(html).toContain('data-external-url="https://example.com/docs?q=markdown"')
    expect(html).not.toContain('href="https://example.com/docs?q=markdown"')
  })

  test('drops raw HTML, blocks unsafe links and never fetches Markdown images', () => {
    const attack = `
<script>alert('raw')</script>
<img src=x onerror=alert('html')>
[js](javascript:alert(1))
[data](data:text/html,boom)
[file](file:///etc/passwd)
[blob](blob:https://example.com/id)
[relative](/api/settings)
[protocol-relative](//example.com/path)
![tracker](https://example.com/track.png)
[safe](https://example.com/path)
`
    const html = renderToStaticMarkup(<AgentMarkdown content={attack} />)

    expect(html).not.toContain('<script')
    expect(html).not.toContain('<img')
    // Raw HTML may remain visible as escaped source, but it is never materialized as a DOM node.
    expect(html).toContain('&lt;img src=x onerror=alert')
    expect(html).not.toContain('javascript:')
    expect(html).not.toContain('data:text')
    expect(html).not.toContain('file:///')
    expect(html).not.toContain('blob:https')
    expect(html).not.toContain('href="/api/settings"')
    expect(html).not.toContain('href="//example.com/path"')
    expect(html).toContain('data-markdown-link-blocked="true"')
    expect(html).toContain('data-markdown-image-blocked="true"')
    expect(html).toContain('data-external-url="https://example.com/path"')
    expect(html).not.toContain('href="https://example.com/path"')
  })

  test('strictly transforms URLs while retaining same-document footnotes', () => {
    expect(safeMarkdownUrl('https://example.com/a')).toBe('https://example.com/a')
    expect(safeMarkdownUrl('HTTP://EXAMPLE.COM/a')).toBe('http://example.com/a')
    expect(safeMarkdownUrl('#user-content-fn-1')).toBe('#user-content-fn-1')
    expect(safeMarkdownUrl('javascript:alert(1)')).toBe('')
    expect(safeMarkdownUrl('data:text/html,boom')).toBe('')
    expect(safeMarkdownUrl('file:///etc/passwd')).toBe('')
    expect(safeMarkdownUrl('/relative')).toBe('')
    expect(safeMarkdownUrl('//example.com')).toBe('')
    expect(safeMarkdownUrl('https://')).toBe('')
  })

  test('hides raw DSML envelopes before their Python body can become Markdown', () => {
    const html = renderToStaticMarkup(
      <AgentMarkdown content={`## 可信前言\n\n${RAW_PYTHON_DSML}\n\n**可信结论**`} />,
    )

    expect(html).toContain('<h2>可信前言</h2>')
    expect(html).toContain('<strong>可信结论</strong>')
    expect(html).toContain('<blockquote>')
    expect(html).toContain('已隐藏一段损坏的历史工具调用协议')
    expect(html).not.toContain('DSML')
    expect(html).not.toContain('agenda checks')
    expect(html).not.toContain('must remain hidden')
  })

  test('fails closed for every recognized streaming prefix of a protocol marker', () => {
    const stream = `${RAW_PYTHON_DSML}\n\nnever trusted after a malformed prefix`
    const recognizedAt = stream.indexOf('D') + 1
    for (let index = 0; index <= stream.length; index += 1) {
      const prefix = stream.slice(0, index)
      expect(() => renderToStaticMarkup(<AgentMarkdown content={prefix} />)).not.toThrow()
      if (index >= recognizedAt) {
        const html = renderToStaticMarkup(<AgentMarkdown content={prefix} />)
        expect(html).not.toContain('DSML')
        expect(html).not.toContain('agenda checks')
        expect(html).not.toContain('must remain hidden')
      }
    }
  })

  test('preserves complete fenced and inline protocol documentation verbatim', () => {
    const documentation = [
      'Inline `<||DSML||tool_calls>` remains visible.',
      '',
      '~~~text',
      '<｜DSML｜tool_calls>',
      '<｜DSML｜invoke name="python">',
      '~~~',
    ].join('\n')

    expect(sanitizeAssistantDisplayText(documentation)).toBe(documentation)
    const html = renderToStaticMarkup(<AgentMarkdown content={documentation} />)
    expect(html).toContain('DSML')
    expect(html).toContain('data-markdown-code-language="text"')
  })

  test('does not let unmatched code delimiters conceal a real protocol marker', () => {
    for (const prefix of ['`unclosed inline\n', '```text\nunclosed fence\n']) {
      const html = renderToStaticMarkup(<AgentMarkdown content={`${prefix}${RAW_PYTHON_DSML}`} />)
      expect(html).not.toContain('DSML')
      expect(html).not.toContain('agenda checks')
      expect(html).not.toContain('must remain hidden')
    }
  })

  test('does not mistake DSML-like prose tags for the bar-delimited protocol', () => {
    const prose = '<DSMLDataset>reactor measurements</DSMLDataset>'
    expect(sanitizeAssistantDisplayText(prose)).toBe(prose)
  })

  test('preserves CommonMark indented protocol examples', () => {
    const documentation = [
      'Example:',
      '',
      '    <｜DSML｜tool_calls>',
      '    <｜DSML｜invoke name="python">',
      '    </｜DSML｜invoke>',
      '    </｜DSML｜tool_calls>',
    ].join('\n')
    expect(sanitizeAssistantDisplayText(documentation)).toBe(documentation)
  })

  test('matches the shared backend/frontend DSML display corpus', () => {
    for (const source of dsmlDisplayCorpus.plain) {
      expect(sanitizeAssistantDisplayText(source)).toBe(source)
    }
    for (const source of dsmlDisplayCorpus.quarantined) {
      expect(sanitizeAssistantDisplayText(source)).toContain(HIDDEN_ASSISTANT_PROTOCOL_NOTICE)
    }
    for (const source of Object.values(dsmlDisplayCorpus.regressions)) {
      expect(sanitizeAssistantDisplayText(source)).toContain(HIDDEN_ASSISTANT_PROTOCOL_NOTICE)
    }
  })

  test('does not let abandoned blockquote or list fences conceal top-level protocol', () => {
    for (const [source, secret] of [
      [dsmlDisplayCorpus.regressions.abandoned_blockquote_fence, 'QUOTE_SECRET'],
      [dsmlDisplayCorpus.regressions.abandoned_list_fence, 'LIST_SECRET'],
    ] as const) {
      const hidden = sanitizeAssistantDisplayText(source)
      expect(hidden).toContain(HIDDEN_ASSISTANT_PROTOCOL_NOTICE)
      expect(hidden).not.toContain(secret)
    }
  })

  test('DOM hides paragraph-indented and escaped-backtick protocol payloads', () => {
    for (const { source, secret } of COMMONMARK_SHIELD_CASES) {
      const html = renderToStaticMarkup(<AgentMarkdown content={source} />)
      expect(html).toContain('已隐藏一段损坏的历史工具调用协议')
      expect(html).not.toContain('DSML')
      expect(html).not.toContain(secret)
    }
  })

  test('preserves an escaped-leading-backtick multiline code-span example', () => {
    const source = dsmlDisplayCorpus.plain.at(-1)!
    const html = renderToStaticMarkup(<AgentMarkdown content={source} />)
    expect(sanitizeAssistantDisplayText(source)).toBe(source)
    expect(html).toContain('<code>')
    expect(html).toContain('DSML')
    expect(html).toContain('CODE_SPAN_SECRET')
  })

  test('does not let inline-code delimiters cross CommonMark block boundaries', () => {
    for (const source of [
      ['`open', '', RAW_PYTHON_DSML, 'tail`'].join('\n'),
      ['# `open', RAW_PYTHON_DSML, 'tail`'].join('\n'),
      ['Title `open', '===', RAW_PYTHON_DSML, 'tail`'].join('\n'),
    ]) {
      const hidden = sanitizeAssistantDisplayText(source)
      const html = renderToStaticMarkup(<AgentMarkdown content={source} />)
      expect(hidden).toContain(HIDDEN_ASSISTANT_PROTOCOL_NOTICE)
      expect(html).not.toContain('DSML')
      expect(html).not.toContain('must remain hidden')
    }
  })

  test('treats escaped-looking backticks inside an open code span as real closers', () => {
    for (const source of [
      ['a `open\\`', RAW_PYTHON_DSML, '`tail'].join('\n'),
      ['a ``open\\``', RAW_PYTHON_DSML, '``tail'].join('\n'),
    ]) {
      const hidden = sanitizeAssistantDisplayText(source)
      const html = renderToStaticMarkup(<AgentMarkdown content={source} />)
      expect(hidden).toContain(HIDDEN_ASSISTANT_PROTOCOL_NOTICE)
      expect(html).not.toContain('DSML')
      expect(html).not.toContain('must remain hidden')
    }
  })

  test('uses parser-confirmed code ranges for tabbed container fences', () => {
    const source = [
      '>\t  ```lang',
      `>\t${RAW_PYTHON_DSML}`,
      '>\t```',
    ].join('\n')
    const hidden = sanitizeAssistantDisplayText(source)
    const html = renderToStaticMarkup(<AgentMarkdown content={source} />)
    expect(hidden).toContain(HIDDEN_ASSISTANT_PROTOCOL_NOTICE)
    expect(html).not.toContain('DSML')
    expect(html).not.toContain('must remain hidden')
  })

  test('DOM never exposes protocol payloads from lazy containers or raw HTML', () => {
    for (const [key, secret] of [
      ['lazy_blockquote_continuation', 'LAZY_QUOTE_SECRET'],
      ['lazy_list_continuation', 'LAZY_LIST_SECRET'],
      ['raw_html_div_block', 'HTML_DIV_SECRET'],
      ['raw_html_pre_block', 'HTML_PRE_SECRET'],
      ['raw_html_table_block', 'HTML_TABLE_SECRET'],
      ['raw_html_script_block', 'HTML_SCRIPT_SECRET'],
      ['raw_html_comment_block', 'HTML_COMMENT_SECRET'],
      ['raw_html_processing_instruction_block', 'HTML_PI_SECRET'],
      ['raw_html_declaration_block', 'HTML_DECL_SECRET'],
      ['raw_html_cdata_block', 'HTML_CDATA_SECRET'],
      ['raw_html_type7_tag_block', 'HTML_TAG_SECRET'],
      ['inline_html_comment_block', 'INLINE_COMMENT_SECRET'],
      ['inline_html_processing_instruction_block', 'INLINE_PI_SECRET'],
      ['inline_html_declaration_block', 'INLINE_DECL_SECRET'],
      ['inline_html_cdata_block', 'INLINE_CDATA_SECRET'],
      ['multiline_link_reference_definition', 'LINK_REFERENCE_SECRET'],
    ] as const) {
      const source = dsmlDisplayCorpus.regressions[key]
      const html = renderToStaticMarkup(<AgentMarkdown content={source} />)
      expect(html).not.toContain('DSML')
      expect(html).not.toContain(secret)
    }
  })

  test('only accepts fenced-code closers with at most three leading spaces', () => {
    const documented = ['```text', '<｜DSML｜tool_calls>', '   ```'].join('\n')
    expect(sanitizeAssistantDisplayText(documented)).toBe(documented)

    const malformedFence = ['```text', 'example', '    ```', RAW_PYTHON_DSML].join('\n')
    const hidden = sanitizeAssistantDisplayText(malformedFence)
    expect(hidden).toContain(HIDDEN_ASSISTANT_PROTOCOL_NOTICE)
    expect(hidden).not.toContain('must remain hidden')
  })

  test('bounds adversarial marker scans and fails closed above the display cap', () => {
    const manyUnmatchedOpeners = `${'<not-a-marker '.repeat(5_000)}safe tail`
    expect(sanitizeAssistantDisplayText(manyUnmatchedOpeners)).toBe(manyUnmatchedOpeners)

    const twoMiBOfOrdinaryAngles = '<'.repeat(MAX_ASSISTANT_DISPLAY_TEXT_LENGTH)
    expect(sanitizeAssistantDisplayText(twoMiBOfOrdinaryAngles)).toBe(twoMiBOfOrdinaryAngles)

    const oversized = 'x'.repeat(MAX_ASSISTANT_DISPLAY_TEXT_LENGTH + 1)
    expect(sanitizeAssistantDisplayText(oversized)).toBe(HIDDEN_ASSISTANT_PROTOCOL_NOTICE)

    for (const nestedSize of [65_536, 256 * 1024]) {
      const pathologicalNesting = `${'>'.repeat(nestedSize)}<｜D`
      const startedAt = performance.now()
      expect(sanitizeAssistantDisplayText(pathologicalNesting)).toBe(HIDDEN_ASSISTANT_PROTOCOL_NOTICE)
      expect(performance.now() - startedAt).toBeLessThan(250)
    }

    for (const pathologicalLists of [
      `${'- '.repeat(4_094)}<｜D`,
      `${'1. '.repeat(2_729)}<｜D`,
      `${'> - '.repeat(2_047)}<｜D`,
      `${'[*_'.repeat(2_729)}<｜D`,
    ]) {
      const startedAt = performance.now()
      expect(sanitizeAssistantDisplayText(pathologicalLists)).toBe(HIDDEN_ASSISTANT_PROTOCOL_NOTICE)
      expect(performance.now() - startedAt).toBeLessThan(250)
    }
  })

  test('renders every incomplete streaming prefix without throwing', () => {
    const stream = '# 标题\n\n**粗体**\n\n| A | B |\n| - | - |\n| 1 | 2 |\n\n```js\nconst x = 1\n```\n\n$$x^2$$\n:rocket:'
    for (let index = 0; index <= stream.length; index += 1) {
      expect(() => renderToStaticMarkup(<AgentMarkdown content={stream.slice(0, index)} />)).not.toThrow()
    }
  })
})
