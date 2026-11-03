import '@testing-library/jest-dom/vitest'
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { ShortcutSettings } from './ShortcutSettings'
import { ShortcutGuard } from './ShortcutGuard'
import { conflicts, empty, useShortcuts, type Snapshot } from './store'
import { invokeBridge } from '../../../lib/tauri'
vi.mock('../../../lib/tauri', () => ({ invokeBridge: vi.fn() }))
const snapshot: Snapshot = {
  preferences: empty,
  labels: ['Not bound', 'Not bound', 'Not bound'],
  running: false,
  capturing: false,
  suspended: false,
  mouse: true,
  captureSupported: true,
  systemBinding: false,
  permission: 'Input Monitoring',
  limitations: [],
  linux: false,
  error: '',
  events: [],
}
beforeEach(() => {
  useShortcuts.setState({ snapshot: null })
  HTMLDialogElement.prototype.showModal = function () {
    this.open = true
  }
  vi.mocked(invokeBridge).mockImplementation(async (command) =>
    command === 'input_snapshot' ? snapshot : undefined,
  )
})
afterEach(() => {
  cleanup()
  vi.clearAllMocks()
})
it('opening settings only reads capabilities and never starts or captures input', async () => {
  render(<ShortcutSettings close={() => {}} />)
  await screen.findByRole('button', { name: 'Capture push-to-talk' })
  expect(invokeBridge).toHaveBeenCalledWith('input_snapshot')
  expect(invokeBridge).not.toHaveBeenCalledWith('capture_shortcut')
  expect(invokeBridge).not.toHaveBeenCalledWith('save_shortcuts', expect.anything())
})
it('captures a native binding into a draft and saves only on explicit action', async () => {
  render(<ShortcutSettings close={() => {}} />)
  const capture = await screen.findByRole('button', { name: 'Capture toggle mute' })
  const binding = { trigger: { key: 'KeyM' } }
  vi.mocked(invokeBridge).mockImplementation(async (command) =>
    command === 'capture_shortcut'
      ? { binding, label: 'M' }
      : command === 'save_shortcuts'
        ? { ...snapshot, preferences: { ...empty, mute: binding }, running: true }
        : snapshot,
  )
  fireEvent.click(capture)
  await screen.findByText('M')
  expect(invokeBridge).not.toHaveBeenCalledWith('save_shortcuts', expect.anything())
  fireEvent.click(screen.getByRole('button', { name: 'Save shortcuts' }))
  await waitFor(() =>
    expect(invokeBridge).toHaveBeenCalledWith('save_shortcuts', {
      preferences: { ...empty, mute: binding },
    }),
  )
})
it('blocks overlapping triggers even when modifiers differ', () => {
  expect(
    conflicts({
      ...empty,
      ptt: { trigger: { key: 'KeyV' } },
      mute: { trigger: { key: 'KeyV' }, modifiers: { ctrl: true } },
    }),
  ).toBe(true)
  expect(conflicts(empty)).toBe(false)
})
it('shows system-managed options rather than raw capture for a portal', async () => {
  vi.mocked(invokeBridge).mockResolvedValue({
    ...snapshot,
    systemBinding: true,
    captureSupported: false,
    mouse: false,
    linux: true,
  })
  render(<ShortcutSettings close={() => {}} />)
  await screen.findByLabelText('Enable system-managed PTT, mute and deafen')
  expect(screen.queryByRole('button', { name: 'Capture push-to-talk' })).not.toBeInTheDocument()
})
it('disables capture on an unavailable backend', async () => {
  vi.mocked(invokeBridge).mockResolvedValue({ ...snapshot, captureSupported: false, mouse: false })
  render(<ShortcutSettings close={() => {}} />)
  expect(await screen.findByRole('button', { name: 'Capture push-to-talk' })).toBeDisabled()
})
it('suspends for password focus and in-app binding capture, then resumes', async () => {
  render(
    <>
      <ShortcutGuard />
      <input type="password" aria-label="Password" />
      <button>Elsewhere</button>
      <div data-testid="capture" />
    </>,
  )
  await act(async () => screen.getByLabelText('Password').focus())
  await waitFor(() =>
    expect(invokeBridge).toHaveBeenCalledWith(
      'suspend_shortcuts',
      expect.objectContaining({ suspended: true }),
    ),
  )
  vi.mocked(invokeBridge).mockClear()
  await act(async () => screen.getByRole('button', { name: 'Elsewhere' }).focus())
  await waitFor(() =>
    expect(invokeBridge).toHaveBeenCalledWith(
      'suspend_shortcuts',
      expect.objectContaining({ suspended: false }),
    ),
  )
  vi.mocked(invokeBridge).mockClear()
  await act(async () => screen.getByTestId('capture').setAttribute('data-binding-capture', 'true'))
  await waitFor(() =>
    expect(invokeBridge).toHaveBeenCalledWith(
      'suspend_shortcuts',
      expect.objectContaining({ suspended: true }),
    ),
  )
})
