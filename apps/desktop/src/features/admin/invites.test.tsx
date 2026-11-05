import '@testing-library/jest-dom/vitest'
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { InviteManager } from './InviteManager'
import { canRevoke, inviteStatus, parseUses } from './invites'
import { invokeBridge } from '../../lib/tauri'
import type { InviteView } from '../../lib/generated/InviteView'
import type { ServerView } from '../../lib/generated/ServerView'
vi.mock('../../lib/tauri', () => ({ invokeBridge: vi.fn() }))
const row: InviteView = {
  id: '00000000000000000000000001',
  createdBy: 'own',
  uses: 0,
  maxUses: 1,
  createdAt: 1000,
  expiresAt: null,
  revokedAt: null,
}
const view: ServerView = {
  ownUserId: 'own',
  canModerateMembers: false,
  canManageServer: false,
  canManageRoles: false,
  canManageChannels: false,
  canManageWebhooks: false,
  canViewAuditLog: false,
  canCreateInvites: true,
  canManageInvites: false,
  channels: [],
  directMessages: [],
  members: [],
  participants: [],
  activeVoice: null,
}
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
      command === 'invite_page'
        ? [row]
        : command === 'create_server_invite'
          ? { id: row.id, code: 'new-secret-code' }
          : undefined,
    )
})
afterEach(cleanup)
it('resolves status and revoke permissions exactly like the server', () => {
  expect(inviteStatus(row, 2000)).toBe('Active')
  expect(inviteStatus({ ...row, uses: 1 }, 2000)).toBe('Used up')
  expect(inviteStatus({ ...row, expiresAt: 2000 }, 2000)).toBe('Expired')
  expect(inviteStatus({ ...row, revokedAt: 1 }, 2000)).toBe('Revoked')
  expect(canRevoke(row, 'own', true, false)).toBe(true)
  expect(canRevoke(row, 'own', false, true)).toBe(false)
  expect(canRevoke(row, 'other', true, false)).toBe(false)
  expect(canRevoke(row, 'other', false, true)).toBe(true)
  expect(parseUses('')).toBeNull()
  expect(parseUses('25')).toBe(25)
  for (const value of ['0', '-1', '1.5', '1e3', '4294967296'])
    expect(() => parseUses(value)).toThrow()
})
it('creates with limits, copies the one-time code and discards it when the panel closes', async () => {
  const first = render(<InviteManager server="server" view={view} connected close={() => {}} />)
  await screen.findByText('00000001')
  fireEvent.change(screen.getByLabelText('Maximum uses'), { target: { value: '5' } })
  fireEvent.change(screen.getByLabelText('Expires after'), { target: { value: '168' } })
  fireEvent.click(screen.getByRole('button', { name: 'Create invite' }))
  await screen.findByDisplayValue('new-secret-code')
  expect(invokeBridge).toHaveBeenCalledWith('create_server_invite', {
    input: { serverId: 'server', maxUses: 5, expiresInHours: 168 },
  })
  expect(screen.getByRole('button', { name: 'Create invite' })).toBeDisabled()
  fireEvent.click(screen.getByRole('button', { name: 'Copy code' }))
  await screen.findByText('Invite code copied.')
  expect(copy).toHaveBeenCalledWith('new-secret-code')
  first.unmount()
  render(<InviteManager server="server" view={view} connected close={() => {}} />)
  expect(screen.queryByDisplayValue('new-secret-code')).not.toBeInTheDocument()
})
it('requires confirmation and reports server denial without removing the row', async () => {
  vi.mocked(invokeBridge).mockImplementation(async (command) => {
    if (command === 'revoke_server_invite') throw { message: 'Permission denied' }
    return [row]
  })
  render(<InviteManager server="server" view={view} connected close={() => {}} />)
  await screen.findByText('00000001')
  fireEvent.click(screen.getByRole('button', { name: 'Revoke' }))
  expect(invokeBridge).not.toHaveBeenCalledWith('revoke_server_invite', expect.anything())
  fireEvent.click(
    within(screen.getByRole('alertdialog')).getByRole('button', { name: 'Confirm revoke' }),
  )
  await screen.findByText('Permission denied')
  expect(screen.getByText('00000001')).toBeInTheDocument()
  expect(screen.getByRole('button', { name: 'Confirm revoke' })).toBeEnabled()
})
it('disables actions while disconnected and hides creation for managers without create permission', async () => {
  const first = render(
    <InviteManager server="server" view={view} connected={false} close={() => {}} />,
  )
  expect(screen.getByRole('button', { name: 'Create invite' })).toBeDisabled()
  expect(screen.getByRole('button', { name: 'Refresh' })).toBeDisabled()
  expect(invokeBridge).not.toHaveBeenCalled()
  first.unmount()
  render(
    <InviteManager
      server="server"
      view={{ ...view, ownUserId: 'manager', canCreateInvites: false, canManageInvites: true }}
      connected
      close={() => {}}
    />,
  )
  await screen.findByText('00000001')
  expect(screen.queryByRole('button', { name: 'Create invite' })).not.toBeInTheDocument()
  expect(screen.getByRole('button', { name: 'Revoke' })).toBeEnabled()
})
it('paginates from the oldest id and clears metadata when access is lost', async () => {
  const page = Array.from({ length: 50 }, (_, i) => ({
    ...row,
    id: String(100 - i).padStart(26, '0'),
  }))
  vi.mocked(invokeBridge)
    .mockResolvedValueOnce(page)
    .mockResolvedValueOnce([row])
    .mockRejectedValueOnce({ message: 'Access removed' })
  render(<InviteManager server="server" view={view} connected close={() => {}} />)
  fireEvent.click(await screen.findByRole('button', { name: 'Load older invites' }))
  await screen.findByText('00000001')
  expect(invokeBridge).toHaveBeenCalledWith('invite_page', {
    serverId: 'server',
    before: page[49]!.id,
  })
  expect(screen.getAllByRole('row')).toHaveLength(52)
  fireEvent.click(screen.getByRole('button', { name: 'Refresh' }))
  await screen.findByText('Access removed')
  expect(screen.getAllByRole('row')).toHaveLength(1)
})
it('keeps the code available when clipboard access fails', async () => {
  copy.mockRejectedValue(new Error('denied'))
  render(<InviteManager server="server" view={view} connected close={() => {}} />)
  await screen.findByText('00000001')
  fireEvent.click(screen.getByRole('button', { name: 'Create invite' }))
  fireEvent.click(await screen.findByRole('button', { name: 'Copy code' }))
  await screen.findByText(/Clipboard access failed/)
  expect(screen.getByDisplayValue('new-secret-code')).toBeInTheDocument()
  await waitFor(() =>
    expect(screen.getByRole('button', { name: 'I saved the code' })).toBeEnabled(),
  )
})
