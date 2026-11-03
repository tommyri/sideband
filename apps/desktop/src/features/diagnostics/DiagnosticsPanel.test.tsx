import '@testing-library/jest-dom/vitest'
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { DiagnosticsPanel } from './DiagnosticsPanel'
import { invokeBridge } from '../../lib/tauri'
import { fixture } from './fixture'
vi.mock('../../lib/tauri', () => ({ invokeBridge: vi.fn() }))
beforeEach(() => {
  HTMLDialogElement.prototype.showModal = function () {
    this.open = true
  }
  vi.mocked(invokeBridge).mockImplementation(async (command) => {
    if (command === 'diagnostics_snapshot') return fixture
    if (command === 'test_build_identity') return { commit: 'test' }
    if (command === 'export_test_report') return '/Downloads/report.zip'
    throw new Error('No environment')
  })
})
afterEach(() => {
  cleanup()
  vi.clearAllMocks()
})
it('opens on the diagnostic shortcut, shows core values, exports redacted JSON and stops polling on close', async () => {
  render(<DiagnosticsPanel />)
  fireEvent.keyDown(window, { code: 'KeyD', ctrlKey: true, shiftKey: true, altKey: true })
  expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
  fireEvent.keyDown(window, { code: 'KeyD', ctrlKey: true, shiftKey: true })
  await screen.findByText('24 ms')
  expect(screen.getByText('1350 bytes')).toBeInTheDocument()
  fireEvent.change(screen.getByLabelText(/Notes/), { target: { value: 'token=seed-secret' } })
  fireEvent.click(screen.getByRole('button', { name: 'Preview redacted diagnostics' }))
  expect(screen.getByLabelText('Diagnostics export preview')).not.toHaveTextContent('seed-secret')
  fireEvent.click(screen.getByRole('button', { name: 'Save log bundle' }))
  await screen.findByRole('status')
  const call = vi.mocked(invokeBridge).mock.calls.find(([c]) => c === 'export_test_report')
  const files = call?.[1]?.files as { name: string; bytes: number[] }[]
  expect(files.map((f) => f.name)).toEqual([
    'report.json',
    'schema.json',
    'diagnostics.log',
    'notes.txt',
  ])
  for (const file of files)
    expect(new TextDecoder().decode(new Uint8Array(file.bytes))).not.toContain('seed-secret')
  fireEvent.click(screen.getByRole('button', { name: 'Close diagnostics' }))
  const before = vi.mocked(invokeBridge).mock.calls.length
  await act(async () => {
    await new Promise((r) => setTimeout(r, 600))
  })
  expect(vi.mocked(invokeBridge).mock.calls).toHaveLength(before)
})
