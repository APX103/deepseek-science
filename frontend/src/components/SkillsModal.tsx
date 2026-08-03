// Skills 设置弹层：左侧导航（Capabilities / Workspace），右侧 skill 开关列表。纯前端态。
import { useState } from 'react'
import { listSkills } from '../api/client'
import { mockSkills } from '../mock/data'
import type { Skill } from '../types'
import Modal from './Modal'
import Toggle from './Toggle'
import {
  IconBook,
  IconCpu,
  IconDatabase,
  IconFile,
  IconKey,
  IconMemory,
  IconNetwork,
  IconPlus,
  IconSearch,
  IconSettings,
  IconShield,
  IconSliders,
  IconZap,
} from './icons'

const CAPABILITIES = [
  { key: 'skills', label: 'Skills', icon: <IconZap width={14} height={14} /> },
  { key: 'connectors', label: 'Connectors', icon: <IconNetwork width={14} height={14} /> },
  { key: 'specialists', label: 'Specialists', icon: <IconBook width={14} height={14} /> },
  { key: 'memory', label: 'Memory', icon: <IconMemory width={14} height={14} /> },
  { key: 'compute', label: 'Compute', icon: <IconCpu width={14} height={14} /> },
  { key: 'network', label: 'Network', icon: <IconNetwork width={14} height={14} /> },
]

const WORKSPACE = [
  { key: 'permissions', label: 'Permissions', icon: <IconShield width={14} height={14} /> },
  { key: 'credentials', label: 'Credentials', icon: <IconKey width={14} height={14} /> },
  { key: 'storage', label: 'Storage', icon: <IconDatabase width={14} height={14} /> },
  { key: 'usage', label: 'Usage', icon: <IconFile width={14} height={14} /> },
  { key: 'general', label: 'General', icon: <IconSettings width={14} height={14} /> },
]

interface Props {
  onClose: () => void
}

export default function SkillsModal({ onClose }: Props) {
  const [nav, setNav] = useState('skills')
  const [skills, setSkills] = useState<Skill[]>(() => {
    // 第一版走 mock；接口形状即 listSkills()（TODO: 接后端后换 useEffect 拉取）
    void listSkills
    return mockSkills
  })
  const [q, setQ] = useState('')

  const filtered = skills.filter((s) => s.name.toLowerCase().includes(q.trim().toLowerCase()))

  return (
    <Modal onClose={onClose} width="max-w-2xl">
      <div className="flex h-[480px]">
        {/* 左侧导航 */}
        <div className="w-44 shrink-0 overflow-y-auto border-r border-border bg-surface p-2">
          <div className="px-2 pb-1 pt-1 text-[11px] font-medium text-ink3">Capabilities</div>
          {CAPABILITIES.map((it) => (
            <NavItem
              key={it.key}
              label={it.label}
              icon={it.icon}
              active={nav === it.key}
              onClick={() => setNav(it.key)}
            />
          ))}
          <div className="px-2 pb-1 pt-3 text-[11px] font-medium text-ink3">Workspace</div>
          {WORKSPACE.map((it) => (
            <NavItem
              key={it.key}
              label={it.label}
              icon={it.icon}
              active={nav === it.key}
              onClick={() => setNav(it.key)}
            />
          ))}
        </div>

        {/* 右侧内容 */}
        <div className="flex min-w-0 flex-1 flex-col">
          {nav === 'skills' ? (
            <>
              <div className="flex items-center gap-2 border-b border-border px-4 py-2.5">
                <span className="text-[13px] font-medium">All ({skills.length})</span>
                <div className="ml-auto flex items-center gap-2 rounded-md border border-border px-2 py-1">
                  <IconSearch width={12} height={12} className="text-ink3" />
                  <input
                    value={q}
                    onChange={(e) => setQ(e.target.value)}
                    placeholder="Search skills…"
                    className="w-32 bg-transparent text-[12px] outline-none placeholder:text-ink3"
                  />
                  <kbd className="text-[11px] text-ink3">⌘K</kbd>
                </div>
                <button className="btn-primary">
                  <IconPlus width={12} height={12} /> Add skill
                </button>
              </div>
              <div className="flex-1 overflow-y-auto px-4 py-2">
                <div className="py-1 text-[11px] font-medium text-ink3">Research skills</div>
                <ul className="divide-y divide-border">
                  {filtered.map((s) => (
                    <li key={s.name} className="flex items-center gap-3 py-2.5">
                      <div className="min-w-0 flex-1">
                        <div className="text-[13px] font-medium text-ink">{s.name}</div>
                        <div className="truncate text-[12px] text-ink2">{s.description}</div>
                      </div>
                      <Toggle
                        checked={s.enabled}
                        onChange={(v) =>
                          setSkills((xs) => xs.map((x) => (x.name === s.name ? { ...x, enabled: v } : x)))
                        }
                      />
                    </li>
                  ))}
                </ul>
              </div>
            </>
          ) : (
            <div className="flex flex-1 items-center justify-center text-[13px] text-ink3">
              <IconSliders className="mr-2" width={14} height={14} />
              {nav} — 暂未实现（第一版仅占位）
            </div>
          )}
        </div>
      </div>
    </Modal>
  )
}

function NavItem({
  label,
  icon,
  active,
  onClick,
}: {
  label: string
  icon: React.ReactNode
  active: boolean
  onClick: () => void
}) {
  return (
    <button
      onClick={onClick}
      className={`flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-[13px] ${
        active ? 'bg-brandSoft font-medium text-brand' : 'text-ink2 hover:bg-surface2 hover:text-ink'
      }`}
    >
      {icon}
      {label}
    </button>
  )
}
