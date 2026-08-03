/** @type {import('tailwindcss').Config} */
// 设计 token 通过 CSS 变量驱动（见 docs/design-system.md），
// Tailwind 颜色名直接映射到 var(--*)，亮/暗主题只换 :root / [data-theme="dark"] 的变量值。
export default {
  content: ['./index.html', './src/**/*.{ts,tsx}'],
  theme: {
    extend: {
      colors: {
        brand: 'var(--brand)',
        brandHover: 'var(--brand-hover)',
        brandSoft: 'var(--brand-soft)',
        bg: 'var(--bg)',
        surface: 'var(--surface)',
        surface2: 'var(--surface-2)',
        border: 'var(--border)',
        borderStrong: 'var(--border-strong)',
        ink: 'var(--text)',
        ink2: 'var(--text-secondary)',
        ink3: 'var(--text-tertiary)',
        danger: 'var(--danger)',
        dangerSoft: 'var(--danger-soft)',
        success: 'var(--success)',
      },
      fontFamily: {
        sans: [
          'Inter',
          'system-ui',
          '-apple-system',
          'Segoe UI',
          'Roboto',
          'PingFang SC',
          'Microsoft YaHei',
          'sans-serif',
        ],
        mono: ['JetBrains Mono', 'ui-monospace', 'SFMono-Regular', 'Menlo', 'Consolas', 'monospace'],
      },
      borderRadius: {
        // 圆角克制：小元素 4-6px，按钮/卡片 8px，大容器 12px 封顶
        sm: '4px',
        DEFAULT: '6px',
        md: '8px',
        lg: '10px',
        xl: '12px',
      },
      boxShadow: {
        // 几乎不用阴影；弹层仅极淡一层
        subtle: '0 1px 2px rgba(0,0,0,0.04)',
        overlay: '0 4px 16px rgba(0,0,0,0.08)',
      },
    },
  },
  plugins: [],
}
