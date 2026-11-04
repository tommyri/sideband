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
import { useUploads } from './useUploads'
import { Composer } from './Composer'
import { MessageFiles } from './MessageFiles'
import { invokeBridge } from '../../lib/tauri'
import { sendMessage } from '../../lib/api'
import { fileSize, mediaKind, MAX_FILE_BYTES } from './attachments'
vi.mock('../../lib/tauri', () => ({ invokeBridge: vi.fn(async () => 'uploaded-id') }))
vi.mock('../../lib/api', () => ({ sendMessage: vi.fn(async () => 'sent') }))
function file(name = 'notes.txt') {
  const value = new File(['hello'], name, { type: 'text/plain' })
  Object.defineProperty(value, 'arrayBuffer', {
    value: async () => new TextEncoder().encode('hello').buffer,
  })
  return value
}
beforeEach(() => {
  vi.mocked(invokeBridge).mockReset().mockResolvedValue('uploaded-id')
  vi.mocked(sendMessage).mockClear()
})
afterEach(cleanup)
it('uploads file-only messages and keeps uploads when sending fails', async () => {
  vi.mocked(sendMessage).mockRejectedValueOnce(new Error('Disconnected')).mockResolvedValue('sent')
  render(
    <Composer
      server="server"
      channel="channel"
      disabled={false}
      canAttach
      draftIdentity="file-send"
      target={null}
      cancel={() => {}}
      sent={() => {}}
    />,
  )
  fireEvent.change(screen.getByLabelText('Attach files'), { target: { files: [file()] } })
  await screen.findByText(/notes.txt.*Ready/)
  fireEvent.click(screen.getByRole('button', { name: 'Send' }))
  await screen.findByText(/Disconnected/)
  expect(screen.getByText(/notes.txt.*Ready/)).toBeInTheDocument()
  fireEvent.click(screen.getByRole('button', { name: 'Send' }))
  await waitFor(() => expect(screen.queryByText(/notes.txt.*Ready/)).not.toBeInTheDocument())
  expect(sendMessage).toHaveBeenLastCalledWith(
    expect.objectContaining({ content: '', attachmentIds: ['uploaded-id'] }),
  )
  expect(vi.mocked(sendMessage).mock.calls[0]![0].nonce).toBe(
    vi.mocked(sendMessage).mock.calls[1]![0].nonce,
  )
})
it('rejects invalid files and enforces upload permission for paste/drop', () => {
  const { result } = renderHook(() => useUploads('limits', 's', 'c'))
  expect(() => result.current.add([new File([], 'empty')])).toThrow('1 byte')
  const large = file()
  Object.defineProperty(large, 'size', { value: MAX_FILE_BYTES + 1 })
  expect(() => result.current.add([large])).toThrow('25 MiB')
  expect(() => result.current.add(Array.from({ length: 11 }, () => file()))).toThrow('10 files')
  render(
    <Composer
      server="s"
      channel="c"
      disabled={false}
      target={null}
      cancel={() => {}}
      sent={() => {}}
    />,
  )
  expect(screen.getByRole('button', { name: 'Attach' })).toBeDisabled()
  fireEvent.paste(screen.getByRole('textbox'), { clipboardData: { files: [file()] } })
  expect(invokeBridge).not.toHaveBeenCalled()
})
it('cancels uploads, ignores late completion and allows failed transfers to retry', async () => {
  let finish!: (id: string) => void
  vi.mocked(invokeBridge).mockImplementation((command) =>
    command === 'attachment_transfer'
      ? new Promise((resolve) => {
          finish = resolve as (id: string) => void
        })
      : Promise.resolve(undefined),
  )
  const { result } = renderHook(() => useUploads('cancel', 's', 'c'))
  act(() => result.current.add([file()]))
  await waitFor(() => expect(result.current.items[0]?.status).toBe('uploading'))
  const key = result.current.items[0]!.key
  act(() => result.current.remove(key))
  await act(async () => finish('late-id'))
  expect(result.current.items).toHaveLength(0)
  expect(invokeBridge).toHaveBeenCalledWith('cancel_attachment', { transfer: key })
  vi.mocked(invokeBridge).mockRejectedValueOnce(new Error('Permission denied'))
  act(() => result.current.add([file()]))
  await waitFor(() => expect(result.current.items[0]?.error).toBe('Permission denied'))
  vi.mocked(invokeBridge).mockResolvedValue('retry-id')
  act(() => result.current.retry(result.current.items[0]!.key))
  await waitFor(() => expect(result.current.items[0]?.id).toBe('retry-id'))
})
it('retains ready attachments by draft identity across navigation', async () => {
  const first = renderHook(() => useUploads('draft-files', 's', 'c'))
  act(() => first.result.current.add([file()]))
  await waitFor(() => expect(first.result.current.items[0]?.status).toBe('ready'))
  first.unmount()
  const second = renderHook(() => useUploads('draft-files', 's', 'c'))
  expect(second.result.current.items[0]?.id).toBe('uploaded-id')
  const other = renderHook(() => useUploads('another-account', 's', 'c'))
  expect(other.result.current.items).toHaveLength(0)
})
it('loads media explicitly, revokes blob URLs, downloads natively and hides previews', async () => {
  const revoke = vi.fn()
  vi.stubGlobal(
    'URL',
    Object.assign(URL, { createObjectURL: vi.fn(() => 'blob:test'), revokeObjectURL: revoke }),
  )
  vi.mocked(invokeBridge).mockResolvedValue(btoa('image'))
  const props = {
    server: 's',
    message: 'm',
    attachments: [{ id: 'a', filename: 'photo.png', mime: 'image/png', size: 1024 }],
    previews: [
      {
        url: 'https://example.com',
        title: 'Example card',
        description: 'Description',
        siteName: 'Example',
        imageUrl: null,
      },
    ],
  }
  const view = render(<MessageFiles {...props} />)
  expect(invokeBridge).not.toHaveBeenCalled()
  fireEvent.click(screen.getByRole('button', { name: 'Load image' }))
  await screen.findByAltText('photo.png')
  vi.mocked(invokeBridge).mockResolvedValue('/Downloads/Sideband downloads/photo.png')
  fireEvent.click(screen.getByRole('button', { name: 'Download' }))
  await screen.findByText(/Saved to/)
  fireEvent.click(screen.getByRole('button', { name: 'Hide link preview' }))
  expect(screen.queryByText('Example card')).not.toBeInTheDocument()
  view.unmount()
  expect(revoke).toHaveBeenCalledWith('blob:test')
  render(<MessageFiles {...props} />)
  expect(screen.getByRole('button', { name: 'Show link preview' })).toBeInTheDocument()
  expect(mediaKind('image/svg+xml')).toBeNull()
  expect(mediaKind('text/html')).toBeNull()
  expect(fileSize(1024)).toBe('1.0 KiB')
})
