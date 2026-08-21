// 项目空态页：左侧栏显示项目会话列表，右侧主区域为空，等待用户选择或新建会话。
import { useState } from 'react'
import { useParams } from 'react-router-dom'
import { useProjects } from '../store'
import Sidebar from '../components/workbench/Sidebar'
import ResizeHandle from '../components/workbench/ResizeHandle'
import SkillsModal from '../components/SkillsModal'

const LEFT_KEY = 'dss_left_w'

function readWidth(key: string, fallback: number): number {
  const v = Number(localStorage.getItem(key))
  return Number.isFinite(v) && v > 0 ? v : fallback
}

export default function ProjectPage() {
  const { pid = '' } = useParams()
  const projects = useProjects()
  const project = projects.find((p) => p.id === pid)
  const [leftW, setLeftW] = useState(() => readWidth(LEFT_KEY, 224))
  const [showSkills, setShowSkills] = useState(false)

  return (
    <div className="flex h-full">
      <Sidebar
        pid={pid}
        sid=""
        width={leftW}
        onOpenSkills={() => setShowSkills(true)}
        onOpenFiles={() => {}}
      />
      <ResizeHandle side="left" value={leftW} min={200} max={360} onChange={setLeftW} />

      <div className="flex min-w-0 flex-1 flex-col items-center justify-center bg-bg p-8">
        <div className="text-center">
          <h1 className="text-[18px] font-semibold text-ink">{project?.name ?? 'Project'}</h1>
          <p className="mt-2 text-[13px] text-ink2">
            从左侧选择一个会话，或点击 New 创建新会话。
          </p>
        </div>
      </div>

      {showSkills && <SkillsModal onClose={() => setShowSkills(false)} />}
    </div>
  )
}
