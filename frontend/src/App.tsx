import { createContext, useContext, useEffect, useState, type ReactNode } from 'react'
import { Route, Routes } from 'react-router-dom'
import HomePage from './pages/HomePage'
import WorkbenchPage from './pages/WorkbenchPage'
import LogsPage from './pages/LogsPage'
import CommandPalette from './components/CommandPalette'
import SettingsModal from './components/SettingsModal'
import { probeBackend } from './api/client'
import type { BackendStatus } from './types'
import { useTheme, type Theme } from './theme'

interface AppCtx {
  theme: Theme
  toggleTheme: () => void
  openCommandPalette: () => void
  openSettings: () => void
  /** 启动时探测的后端状态（/api/health + /api/config） */
  backend: BackendStatus
}

const Ctx = createContext<AppCtx | null>(null)

export function useApp(): AppCtx {
  const v = useContext(Ctx)
  if (!v) throw new Error('useApp must be used within App')
  return v
}

export default function App(): ReactNode {
  const { theme, toggle } = useTheme()
  const [paletteOpen, setPaletteOpen] = useState(false)
  const [settingsOpen, setSettingsOpen] = useState(false)
  const [backend, setBackend] = useState<BackendStatus>({ online: false, llmConfigured: false })

  // 启动探测后端在线状态（一次性；离线时输入框禁用，见 ChatArea）
  useEffect(() => {
    void probeBackend().then(setBackend)
  }, [])

  // ⌘K / Ctrl+K 唤起全局搜索
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') {
        e.preventDefault()
        setPaletteOpen((v) => !v)
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [])

  return (
    <Ctx.Provider
      value={{
        theme,
        toggleTheme: toggle,
        openCommandPalette: () => setPaletteOpen(true),
        openSettings: () => setSettingsOpen(true),
        backend,
      }}
    >
      <Routes>
        <Route path="/" element={<HomePage />} />
        <Route path="/p/:pid/s/:sid" element={<WorkbenchPage />} />
        <Route path="/logs" element={<LogsPage />} />
      </Routes>
      {paletteOpen && <CommandPalette onClose={() => setPaletteOpen(false)} />}
      {settingsOpen && <SettingsModal onClose={() => setSettingsOpen(false)} />}
    </Ctx.Provider>
  )
}
