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
import { Chat } from './Chat'
import { useChat } from './useChat'
import { grouped, canEdit, atBottom, afterTrim } from './model'
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
  start: true,
  latest: true,
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
  it('shows the cached messages when the first page fails, and keeps the reason', async () => {
    vi.mocked(invokeBridge).mockImplementation(async (command) => {
      if (command === 'load_history')
        throw { code: 'client', message: 'state changed while loading history; retry the page' }
      return snapshot
    })
    const { result } = renderHook(() => useChat('server', 'channel'))
    await waitFor(() => expect(result.current.error).toMatch('state changed'))
    await waitFor(() => expect(result.current.view?.messages).toHaveLength(1))
  })
  it('clears a snapshot error once a later snapshot succeeds', async () => {
    let fail = true
    vi.mocked(invokeBridge).mockImplementation(async (command) => {
      if (command === 'load_history') return []
      if (fail) throw { code: 'disconnected', message: 'That server is not connected.' }
      return snapshot
    })
    const { result } = renderHook(() => useChat('server', 'channel'))
    await waitFor(() => expect(result.current.error).toMatch('not connected'))
    fail = false
    act(() =>
      receiveChatEvent({
        serverId: 'server',
        occurredAtMs: 0,
        event: { kind: 'connection', status: { kind: 'connected' } },
      }),
    )
    await waitFor(() => expect(result.current.view?.messages).toHaveLength(1))
    expect(result.current.error).toBe('')
  })
  it('shows the run around a jumped-to message and loads newer pages below it', async () => {
    let latest = false
    vi.mocked(invokeBridge).mockImplementation(async (command) =>
      command === 'chat_snapshot' ? { ...snapshot, start: false, latest } : [],
    )
    const { result } = renderHook(() => useChat('server', 'channel'))
    await waitFor(() => expect(result.current.loading).toBe(false))
    await act(() => result.current.show('old'))
    expect(invokeBridge).toHaveBeenLastCalledWith('chat_snapshot', {
      serverId: 'server',
      channelId: 'channel',
      around: 'old',
    })
    latest = true
    await act(() => result.current.loadNewer('01'))
    expect(invokeBridge).toHaveBeenCalledWith('load_newer', {
      serverId: 'server',
      channelId: 'channel',
      messageId: '01',
    })
    // The run reached the newest: later reads show the newest run again.
    await act(() => result.current.refresh())
    expect(invokeBridge).toHaveBeenLastCalledWith('chat_snapshot', {
      serverId: 'server',
      channelId: 'channel',
      around: null,
    })
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

it('offers newer pages below a jumped-to run and does not mark it read', async () => {
  globalThis.ResizeObserver ??= class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver
  vi.mocked(invokeBridge).mockImplementation(async (command) =>
    command === 'chat_snapshot' ? { ...snapshot, start: false, latest: false } : [],
  )
  render(<Chat server="s" channel="c" name="general" connected />)
  fireEvent.click(await screen.findByRole('button', { name: 'Load newer messages' }))
  await waitFor(() =>
    expect(invokeBridge).toHaveBeenCalledWith('load_newer', {
      serverId: 's',
      channelId: 'c',
      messageId: '01',
    }),
  )
  expect(screen.getByRole('button', { name: 'Jump to latest' })).toBeInTheDocument()
  expect(
    vi.mocked(invokeBridge).mock.calls.some(([, args]) => JSON.stringify(args).includes('"read"')),
  ).toBe(false)
})

it('moves the reader only when a resync dropped the row being read', () => {
  const run = (...ids: string[]) => ids.map((id) => ({ ...message, id }))
  const before = run('01', '02', '03', '04')
  // Older pages loaded above, or newer messages below: the virtualizer keeps the row.
  expect(afterTrim(before, run('00', ...before.map((m) => m.id)), '02', false)).toBeNull()
  expect(afterTrim(before, run('02', '03', '04', '05'), '02', false)).toBeNull()
  // The row being read was dropped with the older messages.
  expect(afterTrim(before, run('03', '04', '05'), '02', false)).toBe('start')
  expect(afterTrim(before, run('07', '08', '09'), '04', true)).toBe('end')
  expect(afterTrim(before, run('07', '08', '09'), '02', false)).toBe('start')
  // A live delete of the row being read, with the oldest message kept, is not a trim.
  expect(afterTrim(before, run('01', '03', '04'), '02', false)).toBeNull()
  expect(afterTrim(before, [], '02', false)).toBeNull()
  expect(afterTrim([], run('01'), null, true)).toBeNull()
})
