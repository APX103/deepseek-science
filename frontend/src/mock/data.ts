// 集中假数据：第一版不接后端，全部页面从这里取数。
import type {
  AppSettings,
  Artifact,
  LogEntry,
  LogLevel,
  LogSource,
  McpServer,
  Message,
  Project,
  SessionState,
  SessionSummary,
  Skill,
  TemplateInfo,
  WorkspaceFile,
} from '../types'

export const mockProjects: Project[] = [
  {
    id: 'proj_a1b2c3d4',
    name: '钙钛矿太阳电池',
    description: '无铅钙钛矿材料调研与综述写作',
    agent_context: '聚焦 Sn/Sn-Ge 基无铅钙钛矿体系，输出中文学术综述。',
    last_session_id: null,
    session_count: 1,
    pinned: false,
    archived: false,
    created_at: '2026-07-30T09:00:00Z',
    updated_at: '2026-07-31T03:00:00Z',
  },
  {
    id: 'proj_e5f6a7b8',
    name: 'Example project',
    description: '示例项目',
    agent_context: '',
    last_session_id: null,
    session_count: 4,
    pinned: false,
    archived: false,
    created_at: '2026-07-28T09:00:00Z',
    updated_at: '2026-07-30T03:00:00Z',
  },
]

export const mockSessions: SessionSummary[] = [
  {
    id: 's_9f8e7d6c5b4a',
    project_id: 'proj_a1b2c3d4',
    title: '研究一下 新型绿色无铅钙钛矿材料在太阳电池领域的应用，写一篇综述',
    status: 'failed',
    live: false,
    created_at: '2026-07-31T02:30:00Z',
    updated_at: '2026-07-31T03:00:00Z',
  },
]

export const mockArtifacts: Artifact[] = [
  {
    path: '2502.22967v1.pdf',
    size: 3_142_000,
    frame_id: 'upload',
    kind: 'pdf',
    origin: 'upload',
    created_at: '2026-07-31T02:31:00Z',
  },
  {
    path: 'review_leadfree_perovskite.md',
    size: 48_200,
    frame_id: 'frame_3',
    kind: 'markdown',
    origin: 'agent',
    created_at: '2026-07-31T02:48:00Z',
  },
  {
    path: 'review_leadfree_perovskite.tex',
    size: 61_500,
    frame_id: 'frame_4',
    kind: 'tex',
    origin: 'agent',
    created_at: '2026-07-31T02:52:00Z',
  },
  {
    path: 'review_leadfree_perovskite.pdf',
    size: 892_000,
    frame_id: 'frame_4',
    kind: 'pdf',
    origin: 'agent',
    created_at: '2026-07-31T02:55:00Z',
  },
]

const PYTHON_CODE = `vid = "34f858b1-a5e9-48b7-8381-88c14e5944ec"
path = f"artifacts/{vid}.pdf"
print("Path:", path)`

const ERROR_JSON = `{
  "error": "File not found or not accessible: {artifact:34f858b1-a5e9-48b7-8381-88c14e5944ec}"
}`

export const mockMessages: Message[] = [
  {
    role: 'user',
    content: '研究一下 新型绿色无铅钙钛矿材料在太阳电池领域的应用，写一篇综述',
  },
  {
    role: 'assistant',
    content: [
      {
        type: 'text',
        text: '我看这篇论文呢，这是我们建的一个小白鼠，就是做一些材料模拟，然后通过 LLM 去找文献，找到这篇主要的文献。在这个过程中也可以利用 DeepSeek Science 把科研的流程跑通，写综述、方便阅读之类的。',
      },
      {
        type: 'tool_use',
        id: 'toolu_01',
        name: 'Read',
        input: {
          file_path: '{artifact:34f858b1-a5e9-48b7-8381-88c14e5944ec}',
          pages: '1-3',
        },
      },
      {
        type: 'tool_result',
        tool_use_id: 'toolu_01',
        content: ERROR_JSON,
        is_error: true,
      },
      {
        type: 'tool_use',
        id: 'toolu_02',
        name: 'Read',
        input: {
          file_path: '34f858b1-a5e9-48b7-8381-88c14e5944ec',
          pages: '1-3',
        },
      },
      {
        type: 'tool_result',
        tool_use_id: 'toolu_02',
        content: ERROR_JSON,
        is_error: true,
      },
      {
        type: 'tool_use',
        id: 'toolu_03',
        name: 'Bash',
        input: { command: 'python get_path.py', description: '获取 PDF 文件路径' },
      },
      {
        type: 'tool_result',
        tool_use_id: 'toolu_03',
        content: PYTHON_CODE,
        is_error: false,
      },
      {
        type: 'tool_use',
        id: 'toolu_04',
        name: 'Read',
        input: {
          file_path: '/Users/apx103/.deepseek-science/proj_a1b2c3d4/artifacts/2502.22967v1.pdf',
          pages: '1-3',
        },
      },
      {
        type: 'tool_result',
        tool_use_id: 'toolu_04',
        content: '[Read 3 pages: 摘要 / 引言 / Sn 基钙钛矿…]',
        is_error: false,
      },
    ],
  },
  {
    role: 'assistant',
    content: [
      {
        type: 'text',
        text: '{"error":{"message":"The input you provided is invalid","type":"input_invalid"}}',
      },
    ],
  },
]

export const mockSessionState: SessionState = {
  id: 's_9f8e7d6c5b4a',
  frame_id: 'frame_4',
  status: 'failed',
  task_summary: '研究一下 新型绿色无铅钙钛矿材料在太阳电池领域的应用，写一篇综述',
  plan_mode: false,
  plan: null,
  artifacts: Object.fromEntries(mockArtifacts.map((a) => [a.path, a])),
  messages: mockMessages,
  runs: [],
}

export const mockFiles: WorkspaceFile[] = [
  { path: 'main.tex', size: 2_048, name: 'main.tex' },
  { path: 'references.bib', size: 4_392, name: 'references.bib' },
  { path: 'get_path.py', size: 312, name: 'get_path.py' },
  { path: '2502.22967v1.pdf', size: 3_142_000, name: '2502.22967v1.pdf' },
  { path: 'data/jv_curve.csv', size: 1_864, name: 'jv_curve.csv' },
  { path: 'review_leadfree_perovskite.md', size: 48_200, name: 'review_leadfree_perovskite.md' },
  { path: 'review_leadfree_perovskite.tex', size: 61_500, name: 'review_leadfree_perovskite.tex' },
  { path: 'review_leadfree_perovskite.pdf', size: 892_000, name: 'review_leadfree_perovskite.pdf' },
]

/** 文本类文件的 mock 内容（Files 预览弹层用）。 */
export const mockFileContents: Record<string, string> = {
  'main.tex': `\\documentclass[11pt]{ctexart}
\\usepackage{amsmath,graphicx}
\\title{新型绿色无铅钙钛矿材料在太阳电池领域的应用研究进展}
\\author{DeepSeek Science}
\\date{2026年7月}

\\begin{document}
\\maketitle

\\section{引言}
钙钛矿太阳电池在过去十年间取得了从 3.8\\% 到 26.1\\% 的认证效率突破……

\\section{Sn 基钙钛矿}
\\bibliography{references}
\\end{document}
`,
  'references.bib': `@article{hao2014lead,
  title  = {Lead-free solid-state organic--inorganic halide perovskite solar cells},
  author = {Hao, Feng and Stoumpos, Constantinos C. and Cao, Duyen H.},
  journal= {Nature Photonics},
  year   = {2014},
  volume = {8},
  pages  = {489--494}
}

@article{noel2014leadfree,
  title  = {Lead-free organic--inorganic tin halide perovskites for photovoltaic applications},
  author = {Noel, Nakita K. and Stranks, Samuel D.},
  journal= {Energy \\& Environmental Science},
  year   = {2014},
  volume = {7},
  pages  = {3061--3068}
}
`,
  'get_path.py': `vid = "34f858b1-a5e9-48b7-8381-88c14e5944ec"
path = f"artifacts/{vid}.pdf"
print("Path:", path)
`,
  'data/jv_curve.csv': `voltage_v,current_density_ma_cm2
0.00,22.41
0.10,21.98
0.20,21.32
0.30,20.41
0.40,19.18
0.50,17.55
0.60,15.42
0.70,12.68
0.80,9.21
0.90,4.87
1.00,0.00
`,
  'review_leadfree_perovskite.md': `# 新型绿色无铅钙钛矿材料在太阳电池领域的应用研究进展

## 摘要

铅卤化物钙钛矿太阳电池在过去十年间取得了从 3.8% 到 26.1% 的认证效率突破，
成为光伏领域最具前景的技术之一。然而，铅的毒性限制了其商业化进程……

## 1 引言

## 2 Sn 基钙钛矿

### 2.1 结构与光电性质
`,
  'review_leadfree_perovskite.tex': `\\section{Sn 基钙钛矿}

\\subsection{结构与光电性质}
Sn$^{2+}$ 与 Pb$^{2+}$ 具有相似的离子半径（分别为 1.18 \\AA{} 和 1.19 \\AA{}），
使得 Sn 成为替代 Pb 最直接的候选元素……

\\begin{equation}
  E_g^{\\text{MASnI}_3} \\approx 1.3 \\ \\text{eV}
\\end{equation}
`,
}

export const mockSkills: Skill[] = [
  { name: 'AlphaFold2', description: '蛋白质结构预测流程', source: 'featured', enabled: true },
  { name: 'Boltz', description: '生物分子相互作用建模', source: 'featured', enabled: true },
  { name: 'Borzoi', description: '基因组调控序列建模', source: 'featured', enabled: true },
  { name: 'Chai-1', description: '分子结构预测', source: 'featured', enabled: true },
  { name: 'DiffDock', description: '分子对接', source: 'featured', enabled: true },
  { name: 'ESM-2', description: '蛋白语言模型嵌入', source: 'featured', enabled: true },
  { name: 'ESMFold2', description: '快速蛋白折叠', source: 'featured', enabled: true },
  { name: 'Evo 2', description: '基因组序列建模', source: 'featured', enabled: true },
  { name: 'Indication Dossier', description: '药物适应症调研报告', source: 'featured', enabled: true },
  { name: 'LigandMPNN', description: '配体环境蛋白设计', source: 'featured', enabled: true },
  { name: 'Literature Review', description: '文献检索与综述写作', source: 'local', enabled: true },
  { name: 'OpenFold3', description: '开源结构预测', source: 'featured', enabled: true },
  { name: 'ProteinMPNN', description: '蛋白序列设计', source: 'featured', enabled: true },
  { name: 'scGPT', description: '单细胞转录组建模', source: 'featured', enabled: true },
  { name: 'Paper Writing', description: '学术论文结构化写作与润色', source: 'local', enabled: true },
  { name: 'Data Analysis', description: '实验数据清洗、统计与绘图', source: 'local', enabled: true },
]

export const mockTemplates: TemplateInfo[] = [
  {
    id: 'review-cn',
    name: '中文综述',
    description: '中文学术综述（ctexart）',
    documentclass: 'ctexart',
    columns: 1,
  },
  {
    id: 'paper-2col',
    name: '英文双栏论文',
    description: '双栏 journal 风格',
    documentclass: 'article',
    columns: 2,
  },
]

// ---------- 日志（system/agent 混合，沿用钙钛矿会话 sid）----------
const LOG_SID = 's_9f8e7d6c5b4a'

function log(
  id: number,
  ts: string,
  level: LogLevel,
  source: LogSource,
  kind: string,
  message: string,
  extra?: Partial<LogEntry>,
): LogEntry {
  return { id, ts, level, source, kind, message, ...extra }
}

export const mockLogs: LogEntry[] = [
  log(1, '2026-07-30T09:12:03Z', 'info', 'system', 'startup', '后端启动 v0.1.0 port 17896', {
    detail: { version: '0.1.0', port: 17896, data_dir: '~/.deepseek-science' },
  }),
  log(2, '2026-07-30T09:12:04Z', 'info', 'system', 'db_migrate', '迁移 mem_layer_a 完成', {
    detail: { step: 'mem_layer_a', rows: 12 },
  }),
  log(3, '2026-07-30T18:40:11Z', 'info', 'system', 'shutdown', '后端关闭', {
    detail: { reason: 'user quit' },
  }),
  log(4, '2026-07-31T02:29:55Z', 'info', 'system', 'startup', '后端启动 v0.1.0 port 17896', {
    detail: { version: '0.1.0', port: 17896, data_dir: '~/.deepseek-science' },
  }),
  log(5, '2026-07-31T02:29:56Z', 'debug', 'system', 'config_load', '配置加载完成', {
    detail: { config_path: '~/.deepseek-science/config.toml', providers: 2 },
  }),
  log(6, '2026-07-31T02:29:57Z', 'info', 'system', 'mcp_connect', 'MCP server「文献搜索」已连接', {
    detail: { server: 'literature-search', tools_count: 3 },
  }),
  log(7, '2026-07-31T02:29:58Z', 'warn', 'system', 'mcp_error', 'MCP 连接失败', {
    detail: { server: 'compute-cluster', error: 'timeout after 10s' },
  }),
  log(8, '2026-07-31T02:30:01Z', 'info', 'agent', 'run_start', '会话开始运行', {
    session_id: LOG_SID,
    frame_id: 'frame_1',
    detail: { prompt_summary: '研究一下 新型绿色无铅钙钛矿材料在太阳电池领域的应用，写一篇综述' },
  }),
  log(9, '2026-07-31T02:30:02Z', 'info', 'agent', 'frame_status', 'frame → RUNNING', {
    session_id: LOG_SID,
    frame_id: 'frame_1',
    detail: { from: 'CREATED', to: 'RUNNING' },
  }),
  log(10, '2026-07-31T02:30:09Z', 'info', 'agent', 'llm_call', '调用 deepseek-chat', {
    session_id: LOG_SID,
    frame_id: 'frame_1',
    iteration: 1,
    detail: { model: 'deepseek-chat', input_tokens: 4210, output_tokens: 812, ms: 6230, stop_reason: 'tool_use' },
  }),
  log(11, '2026-07-31T02:30:10Z', 'info', 'agent', 'tool_call', '调用 Read', {
    session_id: LOG_SID,
    frame_id: 'frame_1',
    iteration: 1,
    detail: { tool: 'Read', input_summary: 'file_path={artifact:34f858b1…}, pages=1-3', ms: 8 },
  }),
  log(12, '2026-07-31T02:30:10Z', 'warn', 'agent', 'tool_error', 'Read 执行失败', {
    session_id: LOG_SID,
    frame_id: 'frame_1',
    iteration: 1,
    detail: { tool: 'Read', error: 'File not found or not accessible: {artifact:34f858b1-a5e9-48b7-8381-88c14e5944ec}' },
  }),
  log(13, '2026-07-31T02:30:18Z', 'info', 'agent', 'llm_call', '调用 deepseek-chat', {
    session_id: LOG_SID,
    frame_id: 'frame_1',
    iteration: 2,
    detail: { model: 'deepseek-chat', input_tokens: 5124, output_tokens: 476, ms: 7102, stop_reason: 'tool_use' },
  }),
  log(14, '2026-07-31T02:30:19Z', 'info', 'agent', 'tool_call', '调用 Bash', {
    session_id: LOG_SID,
    frame_id: 'frame_1',
    iteration: 2,
    detail: { tool: 'Bash', input_summary: 'python get_path.py', ms: 4 },
  }),
  log(15, '2026-07-31T02:30:20Z', 'info', 'agent', 'tool_result', 'Bash 完成', {
    session_id: LOG_SID,
    frame_id: 'frame_1',
    iteration: 2,
    detail: { tool: 'Bash', ok: true, result_summary: 'Path: artifacts/34f858b1-a5e9-48b7-8381-88c14e5944ec.pdf' },
  }),
  log(16, '2026-07-31T02:30:35Z', 'info', 'agent', 'llm_call', '调用 deepseek-chat', {
    session_id: LOG_SID,
    frame_id: 'frame_1',
    iteration: 3,
    detail: { model: 'deepseek-chat', input_tokens: 6890, output_tokens: 1204, ms: 9817, stop_reason: 'tool_use' },
  }),
  log(17, '2026-07-31T02:40:12Z', 'info', 'agent', 'frame_status', 'frame → AWAITING_PLAN_APPROVAL', {
    session_id: LOG_SID,
    frame_id: 'frame_2',
    detail: { from: 'RUNNING', to: 'AWAITING_PLAN_APPROVAL' },
  }),
  log(18, '2026-07-31T02:41:03Z', 'info', 'agent', 'plan', '计划已批准（3 步）', {
    session_id: LOG_SID,
    frame_id: 'frame_2',
    detail: { steps_count: 3, approved: true },
  }),
  log(19, '2026-07-31T02:44:30Z', 'info', 'agent', 'compact', 'Rolling Compact 折叠 1 段', {
    session_id: LOG_SID,
    frame_id: 'frame_3',
    detail: { level: 1, tokens_freed: 8200 },
  }),
  log(20, '2026-07-31T02:48:02Z', 'info', 'agent', 'tool_call', '调用 write_file', {
    session_id: LOG_SID,
    frame_id: 'frame_3',
    iteration: 5,
    detail: { tool: 'write_file', input_summary: 'review_leadfree_perovskite.md (48.2 KB)', ms: 6 },
  }),
  log(21, '2026-07-31T02:48:02Z', 'info', 'agent', 'tool_result', 'write_file 完成', {
    session_id: LOG_SID,
    frame_id: 'frame_3',
    iteration: 5,
    detail: { tool: 'write_file', ok: true, result_summary: 'review_leadfree_perovskite.md 已写入' },
  }),
  log(22, '2026-07-31T02:52:12Z', 'info', 'agent', 'tool_result', 'write_file 完成', {
    session_id: LOG_SID,
    frame_id: 'frame_4',
    iteration: 6,
    detail: { tool: 'write_file', ok: true, result_summary: 'review_leadfree_perovskite.tex 已写入 (61.5 KB)' },
  }),
  log(23, '2026-07-31T02:53:40Z', 'warn', 'agent', 'verify', 'reviewer 裁决：warn', {
    session_id: LOG_SID,
    frame_id: 'frame_4',
    detail: { verdict: 'warn', findings_count: 2 },
  }),
  log(24, '2026-07-31T02:54:05Z', 'info', 'agent', 'memory', '抽取 3 条记忆', {
    session_id: LOG_SID,
    frame_id: 'frame_4',
    detail: { appended: 3, replaced: 0, removed: 0 },
  }),
  log(25, '2026-07-31T02:55:20Z', 'error', 'system', 'llm_error', 'LLM 调用失败重试', {
    detail: { model: 'deepseek-chat', attempt: 2, error: '429 rate limit exceeded' },
  }),
  log(26, '2026-07-31T02:55:31Z', 'error', 'agent', 'run_end', '会话运行结束', {
    session_id: LOG_SID,
    frame_id: 'frame_4',
    detail: { kind: 'error', iterations: 7, usage: { input_tokens: 38210, output_tokens: 9402 } },
  }),
  log(27, '2026-07-31T10:41:02Z', 'info', 'system', 'startup', '后端启动 v0.1.0 port 17896', {
    detail: { version: '0.1.0', port: 17896, data_dir: '~/.deepseek-science' },
  }),
  log(28, '2026-07-31T10:41:03Z', 'debug', 'system', 'db_migrate', '检查迁移：无待执行步骤', {
    detail: { step: 'check', rows: 0 },
  }),
]

// ---------- Settings / MCP ----------
export const mockSettings: AppSettings = {
  providers: [
    {
      name: 'deepseek',
      base_url: 'https://api.deepseek.com',
      api_key_masked: 'sk-…****',
      enabled: true,
      model: 'deepseek-chat',
    },
    {
      name: 'openai-compatible',
      base_url: 'https://api.openai.com/v1',
      api_key_masked: '',
      enabled: false,
      model: 'gpt-4o',
    },
  ],
  model: 'deepseek-chat',
  default_workspace: '~/.deepseek-science',
  revision: 0,
  overridden_fields: [],
}

export const mockMcpServers: McpServer[] = [
  { name: 'literature-search', url: 'http://127.0.0.1:8901/sse', enabled: true, connected: true },
  { name: 'compute-cluster', url: 'http://127.0.0.1:8902/sse', enabled: true, connected: false },
]
