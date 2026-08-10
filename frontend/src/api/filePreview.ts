export type FilePreviewKind = 'pdf' | 'image' | 'text'

/** Route common renderable binary formats before falling back to text/source preview. */
export function filePreviewKind(path: string): FilePreviewKind {
  const ext = path.split('.').pop()?.toLowerCase()
  if (ext === 'pdf') return 'pdf'
  if (['png', 'jpg', 'jpeg', 'gif', 'webp', 'svg'].includes(ext ?? '')) return 'image'
  return 'text'
}
