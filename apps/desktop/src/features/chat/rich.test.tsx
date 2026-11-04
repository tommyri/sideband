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
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { RichMessage } from './RichMessage'
import { Composer } from './Composer'
import {
  canonicalId,
  completionAt,
  completeOptions,
  messageLink,
  parseMessageLink,
  safeLink,
} from './rich-model'
import { draftKey, getDraft, saveDraft, draftNonce, clearDraftNonce } from './navigation'
import { useTyping } from './useTyping'
import { receiveChatEvent } from './api'
import { invokeBridge } from '../../lib/tauri'
import { sendMessage } from '../../lib/api'
vi.mock('../../lib/tauri', () => ({ invokeBridge: vi.fn(async () => undefined) }))
vi.mock('../../lib/api', () => ({ sendMessage: vi.fn(async () => 'sent') }))
const id = '01ARZ3NDEKTSV4RRFFQ69G5FAV'
const member = { id, username: 'immutable', displayName: 'Friendly name' }
const members = new Map([[id, member]])
beforeEach(() => {
  vi.clearAllMocks()
})
afterEach(() => {
  cleanup()
  vi.useRealTimers()
})
it('renders formatting and highlighted code but never raw HTML, images or unsafe links', () => {
  const content =
    '**bold** *italic* ~~strike~~\n\n> quote\n\n```js\nconst x = 1\n```\n\n<script>alert(1)</script><img src="https://evil.test/track" onerror="alert(1)">\n\n[x](javascript:alert%281%29) [file](file:///etc/passwd) [safe](https://example.com) ![alt](https://evil.test/image)'
  const { container } = render(<RichMessage content={content} members={members} own={id} />)
  expect(container.querySelector('strong')).toHaveTextContent('bold')
  expect(container.querySelector('em')).toHaveTextContent('italic')
  expect(container.querySelector('del')).toHaveTextContent('strike')
  expect(container.querySelector('blockquote')).toHaveTextContent('quote')
  expect(container.querySelector('.hljs-keyword')).toHaveTextContent('const')
  expect(container.querySelector('script,img,iframe')).toBeNull()
  expect(screen.getAllByRole('link')).toHaveLength(1)
  fireEvent.click(screen.getByRole('link', { name: 'safe' }))
  expect(invokeBridge).toHaveBeenCalledWith('open_chat_link', { url: 'https://example.com/' })
})
it('resolves canonical mention aliases, keeps code literal, and reveals nested spoiler formatting on demand', () => {
  const alias = id.toLowerCase().replaceAll('0', 'o').replaceAll('1', 'l')
  const { container } = render(
    <RichMessage
      content={`<@${alias}> @everyone :smile: \`<@${id}> :smile:\` ||secret **bold**||`}
      members={members}
      own={id}
    />,
  )
  expect(screen.getByText('@Friendly name')).toHaveAttribute('title', '@immutable')
  expect(container.querySelectorAll('mark.own')).toHaveLength(2)
  expect(container.querySelector('code')).toHaveTextContent(`\u003c@${id}> :smile:`)
  expect(screen.queryByText('secret')).not.toBeInTheDocument()
  fireEvent.click(screen.getByRole('button', { name: 'Reveal spoiler' }))
  expect(screen.getByText('bold')).toBeInTheDocument()
  expect(container).toHaveTextContent('😄')
})
it('rejects overflow/invalid ids and dangerous links, retaining server scope in message links', () => {
  expect(canonicalId(id.toLowerCase())).toBe(id)
  expect(canonicalId('8'.repeat(26))).toBeNull()
  expect(canonicalId('U'.repeat(26))).toBeNull()
  for (const url of [
    'javascript:alert(1)',
    'data:text/html,hi',
    'file:///tmp/file',
    '//evil.test',
    'https://user:password@example.com',
  ])
    expect(safeLink(url)).toBe('')
  const link = messageLink('[::1]:5781', id, id)
  expect(parseMessageLink(link)).toEqual({ server: '[::1]:5781', channel: id, message: id })
  expect(parseMessageLink(link + '?redirect=https://evil.test')).toBeNull()
  expect(completionAt('Hi @fri', 7)).toEqual({ kind: '@', query: 'fri', start: 3, end: 7 })
  expect(completeOptions('@', 'immutable', [member])[0]?.value).toBe(`<@${id}> `)
})
it('completes mentions with the keyboard without sending and supports edit-last and formatting', async () => {
  const editLast = vi.fn()
  render(
    <Composer
      server="s"
      channel="c"
      disabled={false}
      members={[member]}
      target={null}
      cancel={vi.fn()}
      sent={vi.fn()}
      editLast={editLast}
    />,
  )
  const input = screen.getByRole('textbox') as HTMLTextAreaElement
  fireEvent.keyDown(input, { key: 'ArrowUp' })
  expect(editLast).toHaveBeenCalledOnce()
  fireEvent.change(input, { target: { value: '@imm', selectionStart: 4 } })
  expect(screen.getByRole('option')).toHaveTextContent('@immutable')
  fireEvent.keyDown(input, { key: 'Enter' })
  expect(sendMessage).not.toHaveBeenCalled()
  expect(input.value).toBe(`<@${id}> `)
  fireEvent.change(input, { target: { value: 'hello' } })
  input.setSelectionRange(0, 5)
  fireEvent.keyDown(input, { key: 'b', ctrlKey: true })
  expect(input.value).toBe('**hello**')
  await act(async () => {})
})
it('keeps drafts separate by account, server and channel', () => {
  const key = draftKey('server', 'me', 'channel')
  saveDraft(key, 'unfinished')
  expect(getDraft(key)).toBe('unfinished')
  expect(getDraft(draftKey('other', 'me', 'channel'))).toBe('')
  expect(getDraft(draftKey('server', 'other', 'channel'))).toBe('')
  saveDraft(key, '')
  expect(getDraft(key)).toBe('')
})
it('expires typing indicators and ignores the local user or other conversations', async () => {
  vi.useFakeTimers()
  const { result } = renderHook(() => useTyping('server', 'channel', 'own'))
  const event = (user: string, channel = 'channel') =>
    receiveChatEvent({
      serverId: 'server',
      occurredAtMs: 0,
      event: { kind: 'typing', channel, user },
    })
  act(() => {
    event('own')
    event('other', 'elsewhere')
    event('alice')
  })
  expect(result.current).toEqual(['alice'])
  act(() => vi.advanceTimersByTime(6001))
  expect(result.current).toEqual([])
})
it('throttles typing and retains draft text after a failed send', async () => {
  vi.mocked(sendMessage).mockRejectedValueOnce(new Error('offline'))
  render(
    <Composer
      server="typing-test"
      channel="c"
      disabled={false}
      target={null}
      cancel={vi.fn()}
      sent={vi.fn()}
    />,
  )
  const input = screen.getByRole('textbox')
  fireEvent.change(input, { target: { value: 'hello' } })
  fireEvent.change(input, { target: { value: 'hello again' } })
  expect(
    vi.mocked(invokeBridge).mock.calls.filter(([command]) => command === 'chat_action'),
  ).toHaveLength(1)
  fireEvent.click(screen.getByRole('button', { name: 'Send' }))
  await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('offline'))
  expect(input).toHaveValue('hello again')
})

it('keeps retry nonces across composer remounts and replaces them for changed or sent drafts', () => {
  const key = draftKey('nonce-server', 'me', 'channel')
  const first = draftNonce(key, 'hello')
  expect(draftNonce(key, 'hello')).toBe(first)
  expect(draftNonce(key, 'changed')).not.toBe(first)
  const changed = draftNonce(key, 'changed')
  clearDraftNonce(key)
  expect(draftNonce(key, 'changed')).not.toBe(changed)
})
