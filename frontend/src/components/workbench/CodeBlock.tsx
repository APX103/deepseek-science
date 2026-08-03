// 代码块：等宽字体 + 极简手工语法着色（python / json），不引 highlight 库。
interface Props {
  code: string
  lang?: string
}

const KEYWORDS = new Set([
  'import', 'from', 'def', 'return', 'for', 'in', 'if', 'else', 'elif', 'while',
  'print', 'None', 'True', 'False', 'with', 'as', 'class', 'try', 'except',
])

export default function CodeBlock({ code, lang = 'python' }: Props) {
  return (
    <div className="overflow-hidden rounded-md border border-border bg-surface">
      <div className="flex items-center justify-between border-b border-border px-3 py-1">
        <span className="text-[10px] font-medium uppercase tracking-wide text-ink3">{lang}</span>
        <span className="text-[10px] text-ink3">copy</span>
      </div>
      <pre className="overflow-x-auto p-3 font-mono text-[12px] leading-[1.6] text-ink">
        <code>{highlight(code)}</code>
      </pre>
    </div>
  )
}

// 逐 token 着色：字符串 / 注释 / 关键字 / 数字。
function highlight(code: string): React.ReactNode[] {
  const re = /("""[\s\S]*?"""|f?"[^"\n]*"|'[^'\n]*'|#[^\n]*|\b\d+(?:\.\d+)?\b|\b[A-Za-z_]\w*\b)/g
  const out: React.ReactNode[] = []
  let last = 0
  let m: RegExpExecArray | null
  let i = 0
  while ((m = re.exec(code)) !== null) {
    if (m.index > last) out.push(code.slice(last, m.index))
    const tok = m[0]
    let cls = ''
    if (tok.startsWith('"') || tok.startsWith("'") || tok.startsWith('f"')) cls = 'text-success'
    else if (tok.startsWith('#')) cls = 'text-ink3'
    else if (KEYWORDS.has(tok)) cls = 'text-brand'
    else if (/^\d/.test(tok)) cls = 'text-danger'
    out.push(
      <span key={i++} className={cls}>
        {tok}
      </span>,
    )
    last = m.index + tok.length
  }
  if (last < code.length) out.push(code.slice(last))
  return out
}
