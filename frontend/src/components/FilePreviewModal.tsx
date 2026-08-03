// 文本文件预览弹层：等宽字体显示 mock 内容（Files 视图点击触发）。
import type { WorkspaceFile } from '../types'
import { mockFileContents } from '../mock/data'
import Modal from './Modal'

interface Props {
  file: WorkspaceFile
  onClose: () => void
}

export default function FilePreviewModal({ file, onClose }: Props) {
  const content = mockFileContents[file.path]
  return (
    <Modal title={file.path} onClose={onClose} width="max-w-2xl">
      <div className="max-h-[60vh] overflow-auto p-4">
        {content ? (
          <pre className="font-mono text-[12px] leading-[1.7] text-ink2">{content}</pre>
        ) : (
          <p className="text-[13px] text-ink3">暂无预览内容（mock 未覆盖该文件）</p>
        )}
      </div>
    </Modal>
  )
}
