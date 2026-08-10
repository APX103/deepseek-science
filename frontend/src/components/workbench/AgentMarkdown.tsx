import { memo } from 'react'
import { sanitizeAssistantDisplayText } from '../../api/assistantProtocol'
import MarkdownContent, { safeMarkdownUrl } from '../MarkdownContent'

interface Props {
  content: string
}

function AgentMarkdown({ content }: Props) {
  const displayContent = sanitizeAssistantDisplayText(content)
  return <MarkdownContent content={displayContent} />
}

export { safeMarkdownUrl }
export default memo(AgentMarkdown)
