import { useCallback, useEffect, useState } from 'react'

export type Theme = 'light' | 'dark'
const KEY = 'dss_theme'

function readInitial(): Theme {
  const v = localStorage.getItem(KEY)
  return v === 'dark' ? 'dark' : 'light' // 默认亮色
}

/** 亮/暗主题切换：写 <html data-theme> + localStorage。 */
export function useTheme() {
  const [theme, setTheme] = useState<Theme>(readInitial)

  useEffect(() => {
    document.documentElement.dataset.theme = theme
    localStorage.setItem(KEY, theme)
  }, [theme])

  const toggle = useCallback(() => setTheme((t) => (t === 'light' ? 'dark' : 'light')), [])
  return { theme, toggle }
}
