import '@testing-library/jest-dom/vitest'
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { CHECK_INTERVAL, UpdateControl } from './UpdateControl'
import { invokeBridge } from '../../lib/tauri'
vi.mock('../../lib/tauri', () => ({ invokeBridge: vi.fn() }))
beforeEach(() => {
  HTMLDialogElement.prototype.showModal = function () {
    this.open = true
  }
  HTMLDialogElement.prototype.close = function () {
    this.open = false
  }
})
afterEach(() => {
  cleanup()
  vi.clearAllMocks()
  vi.useRealTimers()
})
it('checks on startup and every six hours without installing automatically; cleans up timer', async () => {
  vi.useFakeTimers()
  vi.mocked(invokeBridge).mockResolvedValue({ enabled: true, version: '0.0.2', notes: 'Changes' })
  const view = render(<UpdateControl />)
  await act(async () => {
    await vi.advanceTimersByTimeAsync(0)
  })
  expect(invokeBridge).toHaveBeenCalledTimes(1)
  await act(async () => {
    await vi.advanceTimersByTimeAsync(CHECK_INTERVAL)
  })
  expect(invokeBridge).toHaveBeenCalledTimes(2)
  expect(invokeBridge).not.toHaveBeenCalledWith('install_update')
  view.unmount()
  await vi.advanceTimersByTimeAsync(CHECK_INTERVAL)
  expect(invokeBridge).toHaveBeenCalledTimes(2)
})
it('renders release notes as text and requires explicit install; failures allow retry', async () => {
  vi.mocked(invokeBridge).mockImplementation(async (command) => {
    if (command === 'install_update') throw new Error('Signature verification failed')
    return { enabled: true, version: '0.0.2', notes: '<script>untrusted()</script>' }
  })
  render(<UpdateControl />)
  fireEvent.click(await screen.findByRole('button', { name: 'Update available: 0.0.2' }))
  expect(screen.getByText('<script>untrusted()</script>')).toBeInTheDocument()
  expect(document.querySelector('script')).toBeNull()
  fireEvent.click(screen.getByRole('button', { name: 'Install and restart' }))
  expect(await screen.findByRole('alert')).toHaveTextContent('Signature verification failed')
  await waitFor(() =>
    expect(screen.getByRole('button', { name: 'Install and restart' })).toBeEnabled(),
  )
  expect(invokeBridge).toHaveBeenCalledWith('install_update')
})
it('explains manual updates when signing configuration or package support is absent', async () => {
  vi.mocked(invokeBridge).mockResolvedValue({ enabled: false, version: null, notes: null })
  render(<UpdateControl />)
  fireEvent.click(screen.getByRole('button', { name: 'App updates' }))
  expect(await screen.findByText(/This build uses manual updates/)).toBeInTheDocument()
  expect(screen.queryByRole('button', { name: 'Install and restart' })).toBeNull()
})
it('reports offline failures and recovers on manual retry', async () => {
  vi.mocked(invokeBridge)
    .mockRejectedValueOnce(new Error('Offline'))
    .mockResolvedValue({ enabled: true, version: null, notes: null })
  render(<UpdateControl />)
  fireEvent.click(screen.getByRole('button', { name: 'App updates' }))
  expect(await screen.findByRole('alert')).toHaveTextContent('Offline')
  fireEvent.click(screen.getByRole('button', { name: 'Check for updates' }))
  expect(await screen.findByText('You’re up to date.')).toBeInTheDocument()
  expect(screen.queryByRole('alert')).toBeNull()
})
