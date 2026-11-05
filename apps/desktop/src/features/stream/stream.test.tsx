import '@testing-library/jest-dom/vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { SourcePicker } from './SourcePicker'
import { StreamControls, WatchStream } from './StreamControls'
import { useStreams } from './store'
import { useVoiceStore } from '../voice/store'
import * as api from './api'
vi.mock('./api', async (importOriginal) => ({
  ...(await importOriginal<typeof api>()),
  sources: vi.fn(),
  preview: vi.fn(),
  start: vi.fn(),
  stop: vi.fn(),
  watch: vi.fn(),
}))
beforeEach(() => {
  vi.clearAllMocks()
  HTMLDialogElement.prototype.showModal = function () {
    this.setAttribute('open', '')
  }
  HTMLDialogElement.prototype.close = function () {
    this.removeAttribute('open')
  }
  vi.mocked(api.sources).mockResolvedValue([
    { id: 'portal', label: 'Choose with system picker', kind: 'portal' },
  ])
  vi.mocked(api.start).mockResolvedValue()
  useStreams.setState({
    views: {
      server: {
        canShare: true,
        tracks: [{ track: 7, user: 'friend', channel: 'room' }],
        share: null,
        watching: [],
      },
    },
  })
  useVoiceStore.setState({
    active: { serverId: 'server', channelId: 'room', muted: false, deafened: false },
    views: {},
  })
})
afterEach(cleanup)
it('requires an explicit source and sends quality/cursor choices', async () => {
  const close = vi.fn()
  render(<SourcePicker server="server" channel="room" allowed close={close} />)
  const start = screen.getByRole('button', { name: 'Share selected source' })
  expect(start).toBeDisabled()
  fireEvent.click(await screen.findByLabelText(/Choose with system picker/))
  fireEvent.change(screen.getByLabelText('Quality'), { target: { value: '720p30' } })
  fireEvent.click(screen.getByLabelText('Show cursor'))
  fireEvent.click(start)
  await waitFor(() =>
    expect(api.start).toHaveBeenCalledWith('server', 'room', 'portal', '720p30', false),
  )
  expect(close).toHaveBeenCalledOnce()
  expect(api.preview).not.toHaveBeenCalled()
})
it('blocks sharing after permission loss while the picker is open', async () => {
  const props = { server: 'server', channel: 'room', close: vi.fn() }
  const { rerender } = render(<SourcePicker {...props} allowed />)
  fireEvent.click(await screen.findByLabelText(/Choose with system picker/))
  rerender(<SourcePicker {...props} allowed={false} />)
  expect(screen.getByRole('button', { name: 'Share selected source' })).toBeDisabled()
  expect(screen.getByRole('alert')).toHaveTextContent('STREAM permission')
  expect(api.start).not.toHaveBeenCalled()
})
it('surfaces capture permission errors and permits retry', async () => {
  vi.mocked(api.sources).mockRejectedValueOnce({ message: 'Screen Recording access denied' })
  render(<SourcePicker server="server" channel="room" allowed close={vi.fn()} />)
  expect(await screen.findByRole('alert')).toHaveTextContent('Screen Recording access denied')
  fireEvent.click(screen.getByRole('button', { name: 'Refresh sources' }))
  expect(await screen.findByLabelText(/Choose with system picker/)).toBeVisible()
})
it('gates Share screen and exposes aggregate viewer count and Stop', () => {
  useStreams.setState({
    views: { server: { canShare: false, tracks: [], share: null, watching: [] } },
  })
  const { unmount } = render(<StreamControls server="server" channel="room" connected />)
  expect(screen.getByRole('button', { name: 'Share screen' })).toBeDisabled()
  unmount()
  useStreams.setState({
    views: {
      server: {
        canShare: true,
        tracks: [],
        watching: [],
        share: {
          server: 'server',
          channel: 'room',
          source: 'Display',
          state: 'sharing',
          viewers: 2,
          error: null,
        },
      },
    },
  })
  render(<StreamControls server="server" channel="room" connected />)
  expect(screen.getByRole('status')).toHaveTextContent('2 watching')
  vi.mocked(api.stop).mockResolvedValue()
  fireEvent.click(screen.getByRole('button', { name: 'Stop sharing' }))
  expect(api.stop).toHaveBeenCalledOnce()
})
it('opens a native viewer for the selected track and reports failure', async () => {
  vi.mocked(api.watch).mockRejectedValue({ message: 'Decoder unavailable' })
  render(<WatchStream server="server" user="friend" channel="room" />)
  fireEvent.click(screen.getByRole('button', { name: 'Watch' }))
  expect(api.watch).toHaveBeenCalledWith('server', 7)
  expect(await screen.findByRole('alert')).toHaveTextContent('Decoder unavailable')
})
it('requires joining the same channel before watching', () => {
  useVoiceStore.setState({ active: null })
  render(<WatchStream server="server" user="friend" channel="room" />)
  expect(screen.getByRole('button', { name: 'Watch' })).toBeDisabled()
})
