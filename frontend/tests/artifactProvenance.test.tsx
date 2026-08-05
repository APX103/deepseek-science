import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import ArtifactPanel, { artifactOriginLabel } from '../src/components/workbench/ArtifactPanel'
import { workspaceFileToArtifact } from '../src/api/client'
import type { Artifact } from '../src/types'

function artifact(path: string, origin: Artifact['origin']): Artifact {
  return {
    path,
    size: 128,
    frame_id: origin === 'unknown' ? null : 'frame',
    kind: path.endsWith('.csv') ? 'data' : 'markdown',
    origin,
    created_at: origin === 'unknown' ? null : '2026-08-05T00:00:00Z',
  }
}

function renderArtifacts(artifacts: Artifact[]): string {
  return renderToStaticMarkup(
    <ArtifactPanel
      artifacts={artifacts}
      files={[]}
      tabs={[]}
      activeTab={null}
      taskLabel="Agent outputs"
      onSelectTab={() => {}}
      onCloseTab={() => {}}
      onOpenArtifact={() => {}}
      onPreviewFile={() => {}}
    />,
  )
}

describe('artifact provenance truthfulness', () => {
  test('workspace-scanned input is never guessed to be created or uploaded', () => {
    const html = renderArtifacts([artifact('input.csv', 'unknown')])
    expect(html).toContain('Workspace files (origin not recorded)')
    expect(html).toContain('Workspace file')
    expect(html).not.toContain('Created')
    expect(html).not.toContain('Uploaded')
    expect(html).toContain('1 file')
    expect(html).not.toContain('1 artifact')
  })

  test('the production workspace-file mapper emits only unknown nullable provenance', () => {
    expect(workspaceFileToArtifact({ path: 'inputs/source.csv', name: 'source.csv', size: 512 })).toEqual({
      path: 'inputs/source.csv',
      size: 512,
      frame_id: null,
      kind: 'data',
      origin: 'unknown',
      created_at: null,
    })
  })

  test('agent, upload, and unknown files use separate groups and labels', () => {
    const html = renderArtifacts([
      artifact('report.md', 'agent'),
      artifact('source.csv', 'upload'),
      artifact('legacy.csv', 'unknown'),
    ])
    expect(html).toContain('Agent outputs')
    expect(html).toContain('Created by agent')
    expect(html).toContain('Your uploads')
    expect(html).toContain('Uploaded')
    expect(html).toContain('Workspace files (origin not recorded)')
    expect(html).toContain('Workspace file')
  })

  test('origin label helper covers every current origin explicitly', () => {
    expect(artifactOriginLabel('agent')).toBe('Created by agent')
    expect(artifactOriginLabel('upload')).toBe('Uploaded')
    expect(artifactOriginLabel('unknown')).toBe('Workspace file')
  })
})
