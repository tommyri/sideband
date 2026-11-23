import '@testing-library/jest-dom/vitest'
import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { TestingPanel } from './TestingPanel'
import { invokeBridge } from '../../lib/tauri'
vi.mock('../../lib/tauri', () => ({ invokeBridge: vi.fn() }))
beforeEach(() => {
  HTMLDialogElement.prototype.showModal = function () {
    this.open = true
  }
  vi.mocked(invokeBridge).mockImplementation(async (command) => {
    if (command === 'test_build_identity')
      return { commit: 'full-sha', target: 'test-target', profile: 'debug' }
    if (command === 'export_test_report') return '/Downloads/Sideband reports/test.zip'
    throw new Error('Unavailable test environment')
  })
})
afterEach(() => {
  cleanup()
  vi.clearAllMocks()
})
it('exports a human-judged sanitized run without recording', async () => {
  render(<TestingPanel />)
  fireEvent.click(screen.getByRole('button', { name: 'Help' }))
  fireEvent.click(screen.getByRole('menuitem', { name: 'Guided tests' }))
  fireEvent.click(screen.getByRole('button', { name: 'Start run' }))
  await screen.findByText(/events retained/)
  expect(screen.getByLabelText('Outcome for step 1')).toHaveValue('not-run')
  fireEvent.change(screen.getByLabelText('Outcome for step 1'), { target: { value: 'fail' } })
  fireEvent.change(screen.getByLabelText(/Run notes/), {
    target: { value: 'token=seed-secret Recovery failed' },
  })
  fireEvent.click(screen.getByRole('button', { name: 'Finish run' }))
  await screen.findByRole('button', { name: 'Preview sanitized bundle' })
  expect(screen.queryByLabelText(/Attach WAV/)).not.toBeInTheDocument()
  fireEvent.click(screen.getByRole('button', { name: 'Preview sanitized bundle' }))
  expect(screen.getByLabelText('Sanitized report preview')).not.toHaveTextContent('seed-secret')
  fireEvent.click(screen.getByRole('button', { name: 'Export ZIP to Downloads' }))
  await screen.findByRole('status')
  const call = vi.mocked(invokeBridge).mock.calls.find(([c]) => c === 'export_test_report')
  expect(call?.[1]).toMatchObject({ consent: false, includeRecording: false })
  const files = call?.[1]?.files as { name: string; bytes: number[] }[]
  expect(files.map((f) => f.name)).toEqual([
    'report.json',
    'schema.json',
    'diagnostics.log',
    'notes.txt',
  ])
  const report = JSON.parse(new TextDecoder().decode(new Uint8Array(files[0]!.bytes)))
  expect(report.steps[0].outcome).toBe('fail')
  expect(report.steps[1].outcome).toBe('not-run')
  expect(report.build.commit).toBe('full-sha')
})
it('reports the native reason when an export is refused', async () => {
  vi.mocked(invokeBridge).mockImplementation(async (command) => {
    if (command === 'test_build_identity') return { commit: 'full-sha' }
    // Native commands reject with an ApiError object, not an Error.
    if (command === 'export_test_report')
      throw { code: 'too_large', message: 'The report bundle is larger than 16 MiB.' }
    throw new Error('Unavailable test environment')
  })
  render(<TestingPanel />)
  fireEvent.click(screen.getByRole('button', { name: 'Help' }))
  fireEvent.click(screen.getByRole('menuitem', { name: 'Guided tests' }))
  fireEvent.click(screen.getByRole('button', { name: 'Start run' }))
  await screen.findByText(/events retained/)
  fireEvent.click(screen.getByRole('button', { name: 'Finish run' }))
  fireEvent.click(await screen.findByRole('button', { name: 'Preview sanitized bundle' }))
  fireEvent.click(screen.getByRole('button', { name: 'Export ZIP to Downloads' }))
  expect(await screen.findByText(/larger than 16 MiB/)).toBeInTheDocument()
  expect(screen.queryByText(/Downloads is writable/)).not.toBeInTheDocument()
})
