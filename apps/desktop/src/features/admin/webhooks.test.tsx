import '@testing-library/jest-dom/vitest'
import { cleanup, fireEvent, render, screen, waitFor, within, act } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { WebhookManager } from './WebhookManager'
import { webhookName } from './webhooks'
import { invokeBridge } from '../../lib/tauri'
import type { WebhookView } from '../../lib/generated/WebhookView'
import type { ServerView } from '../../lib/generated/ServerView'
vi.mock('../../lib/tauri', () => ({ invokeBridge: vi.fn() }))
const row: WebhookView = {
  id: 'hook',
  channelId: 'text',
  name: 'Builds',
  createdBy: 'own',
  createdAt: 1000,
  revokedAt: null,
}
const view: ServerView = {
  ownUserId: 'own',
  canManageRoles: false,
  canManageChannels: false,
  canManageWebhooks: true,
  canViewAuditLog: false,
  canCreateInvites: false,
  canManageInvites: false,
  channels: [
    {
      id: 'text',
      parentId: null,
      name: 'general',
      kind: 2,
      profile: '',
      userLimit: null,
      canJoin: false,
      canSpeak: false,
      unread: false,
      mentions: 0,
    },
  ],
  directMessages: [],
  members: [],
  participants: [],
  activeVoice: null,
}
const secret = 'https://example.test:5781/api/v1/webhooks/hook/SECRET'
const copy = vi.fn()
beforeEach(() => {
  HTMLDialogElement.prototype.showModal = function () {
    this.open = true
  }
  HTMLDialogElement.prototype.close = function () {
    this.open = false
  }
  Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: copy } })
  copy.mockReset().mockResolvedValue(undefined)
  vi.mocked(invokeBridge)
    .mockReset()
    .mockImplementation(async (command) =>
      command === 'webhook_page'
        ? [row]
        : command === 'create_server_webhook'
          ? { id: row.id, url: secret }
          : undefined,
    )
})
afterEach(cleanup)
it('validates names and creates with a text channel, revealing/copying the URL once', async () => {
  expect(webhookName(' Builds ')).toBe('Builds')
  expect(webhookName('😀'.repeat(80))).toHaveLength(160)
  for (const input of ['', ' ', 'a'.repeat(81)]) expect(() => webhookName(input)).toThrow()
  const result = render(<WebhookManager server="server" view={view} connected close={() => {}} />)
  await screen.findByText('Builds')
  fireEvent.change(screen.getByLabelText('Name'), { target: { value: ' Deploys ' } })
  fireEvent.click(screen.getByRole('button', { name: 'Create webhook' }))
  await screen.findByDisplayValue(secret)
  expect(invokeBridge).toHaveBeenCalledWith('create_server_webhook', {
    serverId: 'server',
    channelId: 'text',
    name: 'Deploys',
  })
  fireEvent.click(screen.getByRole('button', { name: 'Copy URL' }))
  await screen.findByText('Webhook URL copied.')
  expect(copy).toHaveBeenCalledWith(secret)
  expect(screen.getByRole('button', { name: 'Create webhook' })).toBeDisabled()
  result.unmount()
  render(<WebhookManager server="server" view={view} connected close={() => {}} />)
  expect(screen.queryByDisplayValue(secret)).not.toBeInTheDocument()
})
it('offers manual copy and discards a revealed URL on disconnection', async () => {
  copy.mockRejectedValue(new Error('denied'))
  const result = render(<WebhookManager server="server" view={view} connected close={() => {}} />)
  fireEvent.change(screen.getByLabelText('Name'), { target: { value: 'Builds' } })
  fireEvent.click(screen.getByRole('button', { name: 'Create webhook' }))
  fireEvent.click(await screen.findByRole('button', { name: 'Copy URL' }))
  await screen.findByText(/Clipboard access failed/)
  expect(screen.getByDisplayValue(secret)).toBeInTheDocument()
  result.rerender(<WebhookManager server="server" view={view} connected={false} close={() => {}} />)
  expect(screen.queryByDisplayValue(secret)).not.toBeInTheDocument()
  expect(screen.getByRole('button', { name: 'Create webhook' })).toBeDisabled()
  expect(screen.queryByRole('table')).not.toBeInTheDocument()
})
it('requires confirmation and retains a webhook on revoke denial', async () => {
  vi.mocked(invokeBridge).mockImplementation(async (command) => {
    if (command === 'revoke_server_webhook') throw { message: 'Permission denied' }
    return [row]
  })
  render(<WebhookManager server="server" view={view} connected close={() => {}} />)
  fireEvent.click(await screen.findByRole('button', { name: 'Revoke' }))
  expect(invokeBridge).not.toHaveBeenCalledWith('revoke_server_webhook', expect.anything())
  fireEvent.click(
    within(screen.getByRole('alertdialog')).getByRole('button', { name: 'Confirm revoke' }),
  )
  await screen.findByText('Permission denied')
  expect(screen.getByText('Active')).toBeInTheDocument()
  expect(screen.getByRole('button', { name: 'Confirm revoke' })).toBeEnabled()
  vi.mocked(invokeBridge).mockImplementation(async (command) =>
    command === 'webhook_page' ? [{ ...row, revokedAt: 2000 }] : undefined,
  )
  fireEvent.click(screen.getByRole('button', { name: 'Confirm revoke' }))
  await screen.findByText('Revoked')
  expect(screen.getByRole('button', { name: 'Revoke' })).toBeDisabled()
})
it('paginates metadata and clears it after access loss', async () => {
  const page = Array.from({ length: 50 }, (_, i) => ({
    ...row,
    id: String(100 - i),
    name: `Build ${i}`,
  }))
  vi.mocked(invokeBridge)
    .mockResolvedValueOnce(page)
    .mockResolvedValueOnce([row])
    .mockRejectedValueOnce({ message: 'Access removed' })
  render(<WebhookManager server="server" view={view} connected close={() => {}} />)
  fireEvent.click(await screen.findByRole('button', { name: 'Load older webhooks' }))
  await screen.findByText('Builds')
  expect(invokeBridge).toHaveBeenLastCalledWith('webhook_page', {
    serverId: 'server',
    before: '51',
  })
  expect(screen.getAllByRole('row')).toHaveLength(52)
  fireEvent.click(screen.getByRole('button', { name: 'Refresh' }))
  await screen.findByText('Access removed')
  expect(screen.getAllByRole('row')).toHaveLength(1)
})
it('never requests metadata without permission and rejects unavailable channels', async () => {
  const result = render(
    <WebhookManager
      server="server"
      view={{ ...view, canManageWebhooks: false }}
      connected
      close={() => {}}
    />,
  )
  expect(invokeBridge).not.toHaveBeenCalled()
  expect(screen.getByRole('button', { name: 'Create webhook' })).toBeDisabled()
  result.rerender(
    <WebhookManager server="server" view={{ ...view, channels: [] }} connected close={() => {}} />,
  )
  expect(screen.getByRole('button', { name: 'Create webhook' })).toBeDisabled()
})
it('does not reveal a late mutation response after connection loss', async () => {
  let resolve!: (value: unknown) => void
  vi.mocked(invokeBridge).mockImplementation(async (command) =>
    command === 'create_server_webhook'
      ? new Promise((done) => {
          resolve = done
        })
      : [row],
  )
  const result = render(<WebhookManager server="server" view={view} connected close={() => {}} />)
  fireEvent.change(screen.getByLabelText('Name'), { target: { value: 'Builds' } })
  fireEvent.click(screen.getByRole('button', { name: 'Create webhook' }))
  await waitFor(() => expect(screen.getByRole('button', { name: 'Close' })).toBeDisabled())
  result.rerender(<WebhookManager server="server" view={view} connected={false} close={() => {}} />)
  await act(async () => resolve({ id: 'hook', url: secret }))
  expect(screen.queryByDisplayValue(secret)).not.toBeInTheDocument()
})
