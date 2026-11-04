import '@testing-library/jest-dom/vitest'
import {
  act,
  cleanup,
  fireEvent,
  render,
  renderHook,
  screen,
  waitFor,
} from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { Composer } from './Composer'
import { useChat } from './useChat'
import { grouped, canEdit, atBottom } from './model'
import { receiveChatEvent } from './api'
import { invokeBridge } from '../../lib/tauri'
import { sendMessage } from '../../lib/api'
import type { BridgeMessage } from '../../lib/generated/BridgeMessage'

vi.mock('../../lib/tauri', () => ({ invokeBridge: vi.fn() }))
vi.mock('../../lib/api', () => ({ sendMessage: vi.fn() }))
const message: BridgeMessage = {
  deleted: false,
  id: '01',
  channelId: 'channel',
  authorId: 'owner',
  authorName: 'A name',
  username: 'account',
  content: '<img src=x onerror=alert(1)>',
  replyTo: null,
  createdAt: Date.now(),
  editedAt: null,
  attachmentCount: 0,
  attachments: [],
  previews: [],
}
const snapshot = {
  messages: [message],
  ownUserId: 'owner',
  lastRead: null,
  canSend: true,
  canManage: false,
  canAttach: true,
}
afterEach(() => {
  cleanup()
  vi.restoreAllMocks()
})
beforeEach(() => {
  vi.mocked(invokeBridge).mockReset().mockResolvedValue(undefined)
  vi.mocked(sendMessage).mockReset()
})

describe('chat composer', () => {
  it('retains a failed draft and retries with the same nonce, clearing only after success', async () => {
    vi.mocked(sendMessage)
      .mockRejectedValueOnce({ message: 'Disconnected' })
      .mockResolvedValueOnce('id')
    const cancel = vi.fn()
    render(
      <Composer
        server="s"
        channel="c"
        disabled={false}
        target={null}
        cancel={cancel}
        sent={vi.fn()}
      />,
    )
    fireEvent.change(screen.getByRole('textbox'), { target: { value: message.content } })
    fireEvent.click(screen.getByRole('button', { name: 'Send' }))
    expect(await screen.findByRole('alert')).toHaveTextContent('Disconnected')
    expect(screen.getByRole('textbox')).toHaveValue(message.content)
    expect(document.querySelector('img')).toBeNull()
    fireEvent.click(screen.getByRole('button', { name: 'Send' }))
    await waitFor(() => expect(screen.getByRole('textbox')).toHaveValue(''))
    expect(sendMessage).toHaveBeenCalledTimes(2)
    expect(vi.mocked(sendMessage).mock.calls[0]![0].nonce).toBe(
      vi.mocked(sendMessage).mock.calls[1]![0].nonce,
    )
    expect(cancel).toHaveBeenCalledOnce()
  })
  it('preserves Shift+Enter and IME composition; replies use the selected message id', async () => {
    vi.mocked(sendMessage).mockResolvedValue('id')
    render(
      <Composer
        server="s"
        channel="c"
        disabled={false}
        target={{ kind: 'reply', message }}
        cancel={vi.fn()}
        sent={vi.fn()}
      />,
    )
    const input = screen.getByRole('textbox')
    fireEvent.change(input, { target: { value: 'reply' } })
    fireEvent.keyDown(input, { key: 'Enter', shiftKey: true })
    fireEvent.keyDown(input, { key: 'Enter', isComposing: true })
    expect(sendMessage).not.toHaveBeenCalled()
    fireEvent.keyDown(input, { key: 'Enter' })
    await waitFor(() =>
      expect(sendMessage).toHaveBeenCalledWith(
        expect.objectContaining({ replyTo: '01', content: 'reply' }),
      ),
    )
  })
  it('edits through the native action and disables disconnected sends', async () => {
    vi.mocked(invokeBridge).mockResolvedValue(undefined)
    const { rerender } = render(
      <Composer
        server="s"
        channel="c"
        disabled
        target={{ kind: 'edit', message }}
        cancel={vi.fn()}
        sent={vi.fn()}
      />,
    )
    expect(screen.getByRole('button', { name: 'Save edit' })).toBeDisabled()
    rerender(
      <Composer
        server="s"
        channel="c"
        disabled={false}
        target={{ kind: 'edit', message }}
        cancel={vi.fn()}
        sent={vi.fn()}
      />,
    )
    fireEvent.change(screen.getByRole('textbox'), { target: { value: 'edited' } })
    fireEvent.click(screen.getByRole('button', { name: 'Save edit' }))
    await waitFor(() =>
      expect(invokeBridge).toHaveBeenCalledWith('chat_action', {
        serverId: 's',
        action: { kind: 'edit', message: '01', content: 'edited' },
      }),
    )
    expect(sendMessage).not.toHaveBeenCalled()
  })
})

describe('native cache synchronization', () => {
  it('isolates servers/channels and replaces snapshots so deletes do not reappear', async () => {
    let current = snapshot
    vi.mocked(invokeBridge).mockImplementation(async (command) =>
      command === 'load_history' ? [message] : current,
    )
    const { result } = renderHook(() => useChat('server', 'channel'))
    await waitFor(() => expect(result.current.view?.messages).toHaveLength(1))
    const calls = vi.mocked(invokeBridge).mock.calls.length
    act(() =>
      receiveChatEvent({
        serverId: 'other',
        occurredAtMs: 0,
        event: { kind: 'messagesChanged', channel: 'channel' },
      }),
    )
    act(() =>
      receiveChatEvent({
        serverId: 'server',
        occurredAtMs: 0,
        event: { kind: 'messagesChanged', channel: 'other' },
      }),
    )
    expect(invokeBridge).toHaveBeenCalledTimes(calls)
    current = { ...snapshot, messages: [] }
    act(() =>
      receiveChatEvent({
        serverId: 'server',
        occurredAtMs: 0,
        event: { kind: 'messagesChanged', channel: 'channel' },
      }),
    )
    await waitFor(() => expect(result.current.view?.messages).toEqual([]))
    expect(result.current.hasOlder).toBe(false)
  })
  it('never installs a late response from a previous channel', async () => {
    let resolve!: (value: unknown) => void
    vi.mocked(invokeBridge).mockImplementation(async (command, args) => {
      if (command === 'load_history') return []
      if (args?.channelId === 'old')
        return new Promise((done) => {
          resolve = done
        })
      return { ...snapshot, messages: [{ ...message, channelId: 'new', content: 'new channel' }] }
    })
    const { result, rerender } = renderHook(({ channel }) => useChat('server', channel), {
      initialProps: { channel: 'old' },
    })
    await waitFor(() => expect(resolve).toBeDefined())
    rerender({ channel: 'new' })
    await waitFor(() => expect(result.current.view?.messages[0]?.content).toBe('new channel'))
    await act(async () => resolve(snapshot))
    expect(result.current.view?.messages[0]?.content).toBe('new channel')
  })
})

it('groups only the same immutable author within seven minutes and the same day', () => {
  const previous = { ...message, createdAt: new Date(2026, 10, 4, 12).getTime() }
  expect(grouped(previous, { ...previous, createdAt: previous.createdAt + 1000 })).toBe(true)
  expect(grouped(previous, { ...previous, authorId: 'impostor' })).toBe(false)
  expect(grouped(previous, { ...previous, createdAt: previous.createdAt + 420000 })).toBe(false)
  expect(grouped(previous, { ...previous, replyTo: 'parent' })).toBe(false)
  expect(grouped({ ...previous, authorId: null }, { ...previous, authorId: null })).toBe(false)
  expect(canEdit(message, 'someone-else')).toBe(false)
  expect(canEdit(message, 'owner', message.createdAt + 7 * 86400000)).toBe(false)
})
it('requires the actual bottom of the scroll viewport', () => {
  const element = document.createElement('div')
  Object.defineProperties(element, { scrollHeight: { value: 1000 }, clientHeight: { value: 200 } })
  element.scrollTop = 0
  expect(atBottom(element)).toBe(false)
  element.scrollTop = 800
  expect(atBottom(element)).toBe(true)
})
