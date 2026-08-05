// New Project 弹窗：Name / Description / Agent Context，提交到真实后端。
import { useState } from 'react'
import type { Project } from '../types'
import { createProject } from '../api/client'
import Modal from './Modal'

interface Props {
  onClose: () => void
  onCreated: (p: Project) => void
}

export default function NewProjectModal({ onClose, onCreated }: Props) {
  const [name, setName] = useState('')
  const [description, setDescription] = useState('')
  const [agentContext, setAgentContext] = useState('')

  const submit = async () => {
    if (!name.trim()) return
    const p = await createProject({
      name: name.trim(),
      description: description.trim(),
      agent_context: agentContext.trim(),
    })
    onCreated(p)
    onClose()
  }

  return (
    <Modal title="New Project" onClose={onClose}>
      <form
        className="space-y-4 p-4"
        onSubmit={(e) => {
          e.preventDefault()
          void submit()
        }}
      >
        <div>
          <label className="mb-1 block text-[13px] font-medium">Name</label>
          <input
            autoFocus
            className="input"
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="例如：钙钛矿太阳电池"
          />
        </div>
        <div>
          <label className="mb-1 block text-[13px] font-medium">Description</label>
          <textarea
            className="input min-h-[64px] resize-y"
            value={description}
            onChange={(e) => setDescription(e.target.value)}
          />
          <p className="mt-1 text-[12px] text-ink3">
            Shown in the project list for your reference — not included in the agent's prompt.
          </p>
        </div>
        <div>
          <label className="mb-1 block text-[13px] font-medium">Agent Context</label>
          <textarea
            className="input min-h-[80px] resize-y"
            value={agentContext}
            onChange={(e) => setAgentContext(e.target.value)}
          />
          <p className="mt-1 text-[12px] text-ink3">
            This context will be included in every agent's system prompt for this project — use it for
            conventions, data locations, and standing instructions.
          </p>
        </div>
        <div className="flex justify-end gap-2 pt-1">
          <button type="button" className="btn-outline" onClick={onClose}>
            Cancel
          </button>
          <button type="submit" className="btn-primary" disabled={!name.trim()}>
            Create
          </button>
        </div>
      </form>
    </Modal>
  )
}
