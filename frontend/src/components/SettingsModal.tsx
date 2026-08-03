// Settings 弹层：左侧导航（LLM Providers / MCP Servers / General）。
// 全部前端态：读写走 api/client 桩（localStorage 持久化），TODO: 接后端 /api/settings。
import { useEffect, useState } from 'react'
import type { AppSettings, AppSettingsProvider, McpServer } from '../types'
import { getSettings, listMcpServers, saveMcpServers, saveSettings } from '../api/client'
import { useApp } from '../App'
import Modal from './Modal'
import Toggle from './Toggle'
import { IconCpu, IconKey, IconSettings } from './icons'

type SectionId = 'llm' | 'mcp' | 'general'

const SECTIONS: { id: SectionId; label: string; icon: React.ReactNode }[] = [
  { id: 'llm', label: 'LLM Providers', icon: <IconKey width={14} height={14} /> },
  { id: 'mcp', label: 'MCP Servers', icon: <IconCpu width={14} height={14} /> },
  { id: 'general', label: 'General', icon: <IconSettings width={14} height={14} /> },
]

export default function SettingsModal({ onClose }: { onClose: () => void }) {
  const [section, setSection] = useState<SectionId>('llm')
  return (
    <Modal title="Settings" onClose={onClose} width="max-w-2xl">
      <div className="flex min-h-[380px]">
        {/* 左侧导航 */}
        <nav className="w-44 shrink-0 space-y-0.5 border-r border-border p-2">
          {SECTIONS.map((s) => (
            <button
              key={s.id}
              onClick={() => setSection(s.id)}
              className={`flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-[13px] ${
                section === s.id ? 'bg-brandSoft font-medium text-brand' : 'text-ink2 hover:bg-surface2 hover:text-ink'
              }`}
            >
              {s.icon}
              {s.label}
            </button>
          ))}
        </nav>
        <div className="min-w-0 flex-1 p-4">
          {section === 'llm' && <LlmSection />}
          {section === 'mcp' && <McpSection />}
          {section === 'general' && <GeneralSection />}
        </div>
      </div>
    </Modal>
  )
}

// ---------- LLM Providers ----------
function LlmSection() {
  const [providers, setProviders] = useState<AppSettingsProvider[] | null>(null)
  // 本次输入的 API key（留空 = 不修改，保留已保存的脱敏值）
  const [keyDrafts, setKeyDrafts] = useState<Record<string, string>>({})
  const [saved, setSaved] = useState(false)

  useEffect(() => {
    void getSettings().then((s: AppSettings) => setProviders(s.providers))
  }, [])

  if (!providers) return <div className="py-8 text-center text-[12px] text-ink3">加载中…</div>

  const patch = (name: string, p: Partial<AppSettingsProvider>) =>
    setProviders((xs) => xs!.map((x) => (x.name === name ? { ...x, ...p } : x)))

  const save = async () => {
    const next = providers.map((p) => {
      const draft = keyDrafts[p.name]?.trim()
      // 输入了新 key → 保存脱敏形态；留空 → 保留原值
      return draft ? { ...p, api_key_masked: 'sk-…****' } : p
    })
    // TODO: 接后端 POST /api/settings（需 ≥1 启用的 provider）
    await saveSettings({ providers: next })
    setProviders(next)
    setKeyDrafts({})
    setSaved(true)
    setTimeout(() => setSaved(false), 1500)
  }

  return (
    <div className="space-y-4">
      {providers.map((p) => (
        <div key={p.name} className="card p-3">
          <div className="flex items-center justify-between">
            <span className="text-[13px] font-medium text-ink">{p.name}</span>
            <Toggle checked={p.enabled} onChange={(v) => patch(p.name, { enabled: v })} />
          </div>
          <div className="mt-3 space-y-2">
            <label className="block">
              <span className="mb-1 block text-[11px] text-ink3">Base URL</span>
              <input className="input py-1.5" value={p.base_url} onChange={(e) => patch(p.name, { base_url: e.target.value })} />
            </label>
            <label className="block">
              <span className="mb-1 block text-[11px] text-ink3">Model</span>
              <input
                className="input py-1.5"
                value={p.model ?? ''}
                onChange={(e) => patch(p.name, { model: e.target.value })}
              />
            </label>
            <label className="block">
              <span className="mb-1 block text-[11px] text-ink3">API Key</span>
              <input
                className="input py-1.5 font-mono"
                type="password"
                placeholder={p.api_key_masked || 'sk-…'}
                value={keyDrafts[p.name] ?? ''}
                onChange={(e) => setKeyDrafts((d) => ({ ...d, [p.name]: e.target.value }))}
              />
            </label>
          </div>
        </div>
      ))}
      <div className="flex items-center justify-end gap-2">
        {saved && <span className="text-[12px] text-success">已保存</span>}
        <button className="btn-primary" onClick={() => void save()}>
          保存
        </button>
      </div>
    </div>
  )
}

// ---------- MCP Servers ----------
function McpSection() {
  const [servers, setServers] = useState<McpServer[] | null>(null)
  const [name, setName] = useState('')
  const [url, setUrl] = useState('')

  useEffect(() => {
    void listMcpServers().then(setServers)
  }, [])

  if (!servers) return <div className="py-8 text-center text-[12px] text-ink3">加载中…</div>

  const persist = (next: McpServer[]) => {
    setServers(next)
    void saveMcpServers(next) // TODO: 接后端
  }

  const add = () => {
    const n = name.trim()
    const u = url.trim()
    if (!n || !u) return
    persist([...servers, { name: n, url: u, enabled: true, connected: false }])
    setName('')
    setUrl('')
  }

  return (
    <div className="space-y-3">
      <div className="card divide-y divide-border">
        {servers.map((s) => (
          <div key={s.name} className="flex items-center gap-3 px-3 py-2.5">
            <span
              className={`h-1.5 w-1.5 shrink-0 rounded-full ${s.connected ? 'bg-success' : 'bg-borderStrong'}`}
              title={s.connected ? 'connected' : 'disconnected'}
            />
            <div className="min-w-0 flex-1">
              <div className="truncate text-[13px] font-medium text-ink">{s.name}</div>
              <div className="truncate font-mono text-[11px] text-ink3">{s.url}</div>
            </div>
            <span className="text-[11px] text-ink3">{s.connected ? 'connected' : 'disconnected'}</span>
            <Toggle
              checked={s.enabled}
              onChange={(v) => persist(servers.map((x) => (x.name === s.name ? { ...x, enabled: v } : x)))}
            />
          </div>
        ))}
        {servers.length === 0 && <div className="px-3 py-6 text-center text-[12px] text-ink3">暂无 MCP server</div>}
      </div>

      {/* 添加表单（前端态） */}
      <div className="card space-y-2 p-3">
        <div className="text-[12px] font-medium text-ink2">Add server</div>
        <input className="input py-1.5" placeholder="名称，如 literature-search" value={name} onChange={(e) => setName(e.target.value)} />
        <input
          className="input py-1.5 font-mono"
          placeholder="URL，如 http://127.0.0.1:8901/sse"
          value={url}
          onChange={(e) => setUrl(e.target.value)}
        />
        <div className="flex justify-end">
          <button className="btn-outline" disabled={!name.trim() || !url.trim()} onClick={add}>
            添加
          </button>
        </div>
      </div>
    </div>
  )
}

// ---------- General ----------
function GeneralSection() {
  const { theme, toggleTheme } = useApp()
  return (
    <div className="space-y-4">
      <div className="card p-3">
        <div className="text-[13px] font-medium text-ink">主题</div>
        <div className="mt-2 flex gap-2">
          {(['light', 'dark'] as const).map((t) => (
            <button
              key={t}
              onClick={() => theme !== t && toggleTheme()}
              className={`rounded-md border px-3 py-1.5 text-[12px] ${
                theme === t ? 'border-brand bg-brandSoft font-medium text-brand' : 'border-border text-ink2 hover:bg-surface2'
              }`}
            >
              {t === 'light' ? '亮色' : '深色'}
            </button>
          ))}
        </div>
      </div>
      <div className="card p-3">
        <div className="text-[13px] font-medium text-ink">数据目录</div>
        <input className="input mt-2 py-1.5 font-mono" value="~/.deepseek-science" readOnly />
        <p className="mt-1 text-[11px] text-ink3">本地优先：所有会话、日志与设置均保存在此目录（只读展示）。</p>
      </div>
    </div>
  )
}
