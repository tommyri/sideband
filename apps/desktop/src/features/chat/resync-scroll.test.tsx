import '@testing-library/jest-dom/vitest'
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import type { BridgeMessage } from '../../lib/generated/BridgeMessage'

// jsdom has no layout, so the virtualizer is replaced by one that reports the
// row being read and records where Chat asks it to scroll.
const virtual = vi.hoisted(() => {
  const state = {
    count: 0,
    key: (index: number): string => String(index),
    scrollOffset: 0,
    reading: '02',
    scrollToEnd: vi.fn(),
    scrollToIndex: vi.fn(),
    getTotalSize: () => 0,
    getVirtualItems: () =>
      Array.from({ length: state.count }, (_, index) => ({
        index,
        key: state.key(index),
        start: 0,
        end: 0,
        size: 0,
        lane: 0,
      })),
    measureElement: () => {},
    getVirtualItemForOffset: () => ({ key: state.reading }),
  }
  return state
})
vi.mock('@tanstack/react-virtual', () => ({
  useVirtualizer: (options: { count: number; getItemKey: (index: number) => string }) => {
    virtual.count = options.count
    virtual.key = options.getItemKey
    return virtual
  },
}))
vi.mock('../../lib/tauri', () => ({ invokeBridge: vi.fn() }))
vi.mock('../../lib/api', () => ({ sendMessage: vi.fn() }))
import { invokeBridge } from '../../lib/tauri'
import { Chat } from './Chat'
import { receiveChatEvent } from './api'

const run = (...ids: string[]): BridgeMessage[] =>
  ids.map((id) => ({
    deleted: false,
    id,
    channelId: 'c',
    authorId: 'owner',
    authorName: 'Owner',
    username: 'owner',
    content: `message ${id}`,
    replyTo: null,
    createdAt: 0,
    editedAt: null,
    attachmentCount: 0,
    attachments: [],
    previews: [],
  }))
let messages = run('01', '02', '03')
beforeEach(() => {
  globalThis.ResizeObserver ??= class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver
  messages = run('01', '02', '03')
  virtual.reading = '02'
  vi.mocked(invokeBridge).mockImplementation(async (command) =>
    command === 'chat_snapshot'
      ? {
          messages,
          ownUserId: 'owner',
          lastRead: null,
          canSend: true,
          canManage: false,
          canAttach: true,
          start: false,
          latest: true,
        }
      : [],
  )
})
afterEach(() => {
  cleanup()
  vi.clearAllMocks()
})

async function resync(next: BridgeMessage[]) {
  messages = next
  act(() => receiveChatEvent({ serverId: 's', occurredAtMs: 0, event: { kind: 'resynced' } }))
  await waitFor(() =>
    expect(invokeBridge).toHaveBeenLastCalledWith('chat_snapshot', expect.anything()),
  )
}

it('shows the oldest kept message when a resync dropped the row being read', async () => {
  render(<Chat server="s" channel="c" name="general" connected />)
  const list = screen.getByLabelText('Message history')
  await screen.findByText('message 02')
  // Scrolled up, reading message 02.
  Object.defineProperties(list, {
    scrollHeight: { value: 1000, configurable: true },
    clientHeight: { value: 200, configurable: true },
  })
  list.scrollTop = 100
  fireEvent.scroll(list)
  virtual.scrollToEnd.mockClear()
  await resync(run('07', '08', '09'))
  await screen.findByText('message 07')
  expect(virtual.scrollToIndex).toHaveBeenCalledWith(0, { align: 'start' })
  expect(virtual.scrollToEnd).not.toHaveBeenCalled()
})

it('stays at the newest message when a resync replaced the whole run', async () => {
  render(<Chat server="s" channel="c" name="general" connected />)
  await screen.findByText('message 03')
  virtual.reading = '03'
  virtual.scrollToEnd.mockClear()
  await resync(run('07', '08'))
  await screen.findByText('message 07')
  expect(virtual.scrollToEnd).toHaveBeenCalled()
  expect(virtual.scrollToIndex).not.toHaveBeenCalled()
})

it('leaves the position to the virtualizer when the row being read is kept', async () => {
  render(<Chat server="s" channel="c" name="general" connected />)
  await screen.findByText('message 02')
  virtual.scrollToEnd.mockClear()
  await resync(run('02', '03', '04'))
  await screen.findByText('message 04')
  expect(virtual.scrollToEnd).not.toHaveBeenCalled()
  expect(virtual.scrollToIndex).not.toHaveBeenCalled()
})
