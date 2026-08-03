import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// https://vite.dev/config/
export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      // 浏览器开发时 /api 代理到本地后端（见 docs/api-contract.md）
      '/api': {
        target: 'http://127.0.0.1:17896',
        changeOrigin: true,
      },
    },
  },
})
