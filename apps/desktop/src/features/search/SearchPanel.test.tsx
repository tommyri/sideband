import '@testing-library/jest-dom/vitest'
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { SearchPanel } from './SearchPanel'
import { invokeBridge } from '../../lib/tauri'
import { navigateMessage } from '../chat/navigation'
import { receiveChatEvent } from '../chat/api'
import type { ServerView } from '../../lib/generated/ServerView'
import type { SearchResult } from '../../lib/generated/SearchResult'
vi.mock('../../lib/tauri', () => ({ invokeBridge: vi.fn() }))
vi.mock('../chat/navigation', () => ({ navigateMessage: vi.fn() }))
const view = {
  canModerateMembers: false,
  ownUserId: 'me',
  members: [],
  directMessages: [],
  channels: [{ id: 'channel', name: 'general', kind: 2 }],
} as unknown as ServerView
const row: SearchResult = {
  id: 'message',
  channelId: 'channel',
  authorId: 'me',
  authorName: 'Alex',
  createdAt: '1000',
  excerpt: '<script> \u0001launch\u0002 details',
}
beforeEach(() => {
  vi.clearAllMocks()
  HTMLDialogElement.prototype.showModal = function () {
    this.setAttribute('open', '')
  }
  HTMLDialogElement.prototype.close = vi.fn()
})
afterEach(cleanup)
function search() {
  fireEvent.change(screen.getByLabelText('Search words'), { target: { value: 'launch' } })
  fireEvent.click(screen.getByRole('button', { name: 'Search' }))
}
it('renders safe highlights and navigates to the selected message', async () => {
  vi.mocked(invokeBridge).mockResolvedValue([row])
  const close = vi.fn()
  const { container } = render(<SearchPanel server="server" view={view} close={close} />)
  search()
  await screen.findByText('1 match')
  expect(container.querySelector('mark')).toHaveTextContent('launch')
  expect(container.querySelector('script')).toBeNull()
  expect(invokeBridge).toHaveBeenCalledWith('search_messages', {
    serverId: 'server',
    query: { q: 'launch', channel: null, from: null, before: null, after: null, has: null },
  })
  fireEvent.click(screen.getByRole('button', { name: 'Jump to message by Alex' }))
  expect(navigateMessage).toHaveBeenCalledWith('sideband://message/server/channel/message')
  expect(close).toHaveBeenCalled()
})
it('discards delayed results after permissions change and removes existing matches on deletion', async () => {
  let resolve!: (rows: SearchResult[]) => void
  vi.mocked(invokeBridge).mockReturnValueOnce(
    new Promise((r) => {
      resolve = r
    }),
  )
  render(<SearchPanel server="server" view={view} close={vi.fn()} />)
  search()
  act(() =>
    receiveChatEvent({ serverId: 'server', occurredAtMs: 1, event: { kind: 'channelsChanged' } }),
  )
  await act(async () => {
    resolve([row])
  })
  expect(screen.queryByText('Alex')).not.toBeInTheDocument()
  vi.mocked(invokeBridge).mockResolvedValue([row])
  search()
  await screen.findByText('Alex')
  act(() =>
    receiveChatEvent({
      serverId: 'server',
      occurredAtMs: 1,
      event: { kind: 'messagesChanged', channel: 'channel' },
    }),
  )
  expect(screen.queryByText('Alex')).not.toBeInTheDocument()
})
it('hides revoked conversations on rerender and reports invalid date filters', async () => {
  vi.mocked(invokeBridge).mockResolvedValue([row])
  const props = { server: 'server', view, close: vi.fn() }
  const { rerender } = render(<SearchPanel {...props} />)
  search()
  await screen.findByText('Alex')
  rerender(<SearchPanel {...props} view={{ ...view, channels: [] }} />)
  expect(screen.queryByText('Alex')).not.toBeInTheDocument()
  fireEvent.change(screen.getByLabelText('After (UTC)'), { target: { value: '2026-11-10' } })
  fireEvent.change(screen.getByLabelText('Before (UTC)'), { target: { value: '2026-11-01' } })
  search()
  await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('After must be earlier'))
  expect(invokeBridge).toHaveBeenCalledTimes(1)
})
