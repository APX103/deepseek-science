import { describe, expect, test } from 'bun:test'
import { readFileSync } from 'node:fs'

function readProjectFile(relativePath: string): string {
  return readFileSync(new URL(relativePath, import.meta.url), 'utf8')
}

const capability = JSON.parse(
  readProjectFile('../../src-tauri/capabilities/default.json'),
) as unknown
const sidebarSource = readProjectFile('../src/components/workbench/Sidebar.tsx')
const chatAreaSource = readProjectFile('../src/components/workbench/ChatArea.tsx')
const promptQueueSource = readProjectFile('../src/components/workbench/PromptQueue.tsx')
const homePageSource = readProjectFile('../src/pages/HomePage.tsx')
const logsPageSource = readProjectFile('../src/pages/LogsPage.tsx')
const windowChromeSource = readProjectFile('../src/windowChrome.ts')
const previewTitleBarSource = readProjectFile('../src/components/PreviewTitleBar.tsx')
const pdfPreviewSource = readProjectFile('../src/components/PdfPreviewModal.tsx')
const imagePreviewSource = readProjectFile('../src/components/ImagePreviewModal.tsx')
const filePreviewSource = readProjectFile('../src/components/FilePreviewModal.tsx')

function sourceSection(source: string, startMarker: string, endMarker: string): string {
  const start = source.indexOf(startMarker)
  const end = source.indexOf(endMarker, start)

  expect(start).toBeGreaterThanOrEqual(0)
  expect(end).toBeGreaterThan(start)
  return source.slice(start, end)
}

describe('native window drag capability', () => {
  test('authorizes only start-dragging for the scoped main window', () => {
    expect(capability).toEqual({
      $schema: '../gen/schemas/desktop-schema.json',
      identifier: 'default',
      description: 'Capability for the main window',
      windows: ['main'],
      permissions: [
        'core:default',
        'core:window:allow-start-dragging',
        {
          identifier: 'opener:allow-open-url',
          allow: [{ url: 'https://*' }, { url: 'http://*' }],
        },
      ],
    })
  })
})

describe('native window drag regions', () => {
  test('keeps the existing page and loaded-sidebar blank strips draggable', () => {
    const fixedStrip =
      /<div data-tauri-drag-region className="fixed inset-x-0 top-0 z-30 h-7" \/>/

    expect(homePageSource).toMatch(fixedStrip)
    expect(logsPageSource).toMatch(fixedStrip)
    expect(sidebarSource).toContain('<div data-tauri-drag-region className="h-10" />')

    const brandLink = sidebarSource.match(/<Link\s+to="\/"[\s\S]*?>/)?.[0]
    expect(brandLink).toBeDefined()
    expect(brandLink).not.toContain('data-tauri-drag-region')
  })

  test('marks only blank toolbar surfaces and leaves both controls clickable', () => {
    const toolbarSource = sourceSection(
      chatAreaSource,
      'function WorkbenchToolbar(',
      '/** Agent Failed',
    )

    expect(toolbarSource).toMatch(
      /<div\s+data-tauri-drag-region\s+className="flex h-9 shrink-0 select-none items-center gap-1\.5 px-1\.5"/,
    )
    expect(toolbarSource).toContain(
      '<div className="h-full flex-1" data-tauri-drag-region />',
    )
    expect(toolbarSource.match(/data-tauri-drag-region/g)).toHaveLength(2)
    expect(toolbarSource).not.toContain('data-tauri-drag-region="deep"')

    const buttons = toolbarSource.match(/<button\b[\s\S]*?>/g) ?? []
    expect(buttons).toHaveLength(2)
    for (const button of buttons) {
      expect(button).not.toContain('data-tauri-drag-region')
    }
  })

  test('retains the collapsed-left traffic-light inset', () => {
    expect(windowChromeSource).toContain('export const MAC_TRAFFIC_LIGHT_INSET = 76')
    expect(chatAreaSource).toContain(
      "import { MAC_TRAFFIC_LIGHT_INSET } from '../../windowChrome'",
    )
    expect(chatAreaSource).toContain(
      'style={{ paddingLeft: leftCollapsed ? MAC_TRAFFIC_LIGHT_INSET : undefined }}',
    )
  })

  test('keeps full-window previews traffic-light-safe with bare blank drag surfaces', () => {
    expect(previewTitleBarSource).toContain(
      "import { MAC_TRAFFIC_LIGHT_INSET } from '../windowChrome'",
    )
    expect(previewTitleBarSource).toContain(
      'style={{ paddingLeft: MAC_TRAFFIC_LIGHT_INSET }}',
    )
    expect(previewTitleBarSource).toContain(
      'className="min-w-0 truncate font-mono text-[13px] text-ink2"',
    )
    expect(previewTitleBarSource).toContain(
      'className="shrink-0 text-[12px] text-ink3"',
    )
    expect(previewTitleBarSource).toContain(
      'className="ml-auto flex shrink-0 items-center gap-1"',
    )
    expect(previewTitleBarSource).toContain(
      '<div className="h-full flex-1" data-tauri-drag-region />',
    )
    expect(previewTitleBarSource.match(/data-tauri-drag-region/g)).toHaveLength(2)
    expect(previewTitleBarSource).not.toContain('data-tauri-drag-region="deep"')
    expect(previewTitleBarSource).not.toContain('stopPropagation')

    for (const source of [pdfPreviewSource, imagePreviewSource]) {
      expect(source).toContain("import PreviewTitleBar from './PreviewTitleBar'")
      expect(source).toContain('event.target === event.currentTarget')
      expect(source).not.toContain('stopPropagation')

      const controls = source.match(/<(?:a|button)\b[\s\S]*?>/g) ?? []
      expect(controls.length).toBeGreaterThan(0)
      for (const control of controls) {
        expect(control).not.toContain('data-tauri-drag-region')
      }
    }

    expect(pdfPreviewSource).toContain(
      '<PreviewTitleBar path={artifact.path} size={artifact.size}>',
    )
    expect(imagePreviewSource).toContain('<PreviewTitleBar path={file.path} size={file.size}>')
    expect(filePreviewSource).not.toContain('PreviewTitleBar')
    expect(filePreviewSource).not.toContain('MAC_TRAFFIC_LIGHT_INSET')
  })

  test('never turns queue editing, drag, or action controls into window drag regions', () => {
    expect(promptQueueSource).toContain('data-prompt-queue-interaction')
    expect(promptQueueSource).toContain('draggable={false}')
    expect(promptQueueSource).toContain('setPointerCapture(event.pointerId)')
    expect(promptQueueSource).not.toContain('data-tauri-drag-region')
  })
})
