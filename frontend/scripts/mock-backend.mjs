// 本地 mock 后端：按 docs/api-contract.md 提供 /api/health、/api/config、POST /api/sessions、POST /api/sessions/{sid}/stream-sse。
// 仅用于前端联调（真实后端未就绪时）；stream-sse 按契约逐帧发 start/thinking/text×N/complete。
// 用法：node scripts/mock-backend.mjs（监听 127.0.0.1:17896）
import http from 'node:http'

const json = (res, obj) => {
  res.writeHead(200, { 'Content-Type': 'application/json' })
  res.end(JSON.stringify(obj))
}

http
  .createServer((req, res) => {
    const url = new URL(req.url, 'http://x')
    if (url.pathname === '/api/health') return json(res, { status: 'ok', version: '0.1.0' })
    if (url.pathname === '/api/config')
      return json(res, { llm_configured: true, model: 'deepseek-chat', base_url: 'https://api.deepseek.com' })
    if (url.pathname === '/api/sessions' && req.method === 'POST')
      return json(res, { id: `srv_${Math.random().toString(36).slice(2, 10)}`, frame_id: 'frame_1', model: 'deepseek-chat', workspace: '/tmp/mock' })
    if (url.pathname.endsWith('/stream-sse') && req.method === 'POST') {
      res.writeHead(200, { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-cache', Connection: 'keep-alive' })
      const send = (o) => res.write(`data: ${JSON.stringify(o)}\n\n`)
      send({ type: 'start', frame_id: 'frame_1', task_summary: 'mock 对话' })
      const frames = [
        { type: 'thinking', text: '用户打了个招呼，' },
        { type: 'thinking', text: '我应该友好地回应。' },
        ...'你好！我是 DeepSeek，很高兴见到你。有什么科研任务可以帮你？'.split('').map((c) => ({ type: 'text', text: c })),
      ]
      let i = 0
      const timer = setInterval(() => {
        if (i < frames.length) return send(frames[i++])
        clearInterval(timer)
        send({ type: 'complete', kind: 'natural', final_text: '', awaiting: null, usage: { input_tokens: 12, output_tokens: 34 }, iterations: 1, frame_status: 'COMPLETED', artifacts: {} })
        res.end()
      }, 80)
      req.on('close', () => clearInterval(timer))
      return
    }
    res.writeHead(404)
    res.end()
  })
  .listen(17896, '127.0.0.1', () => console.log('mock backend on 127.0.0.1:17896'))
