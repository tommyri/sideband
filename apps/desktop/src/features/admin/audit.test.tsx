import '@testing-library/jest-dom/vitest'
import { cleanup, fireEvent, render, screen, waitFor, act } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { AuditLog } from './AuditLog'
import { auditFilters } from './audit'
import { invokeBridge } from '../../lib/tauri'
import type { AuditEntryView } from '../../lib/generated/AuditEntryView'
import type { ServerView } from '../../lib/generated/ServerView'
vi.mock('../../lib/tauri', () => ({ invokeBridge: vi.fn() }))
const row: AuditEntryView = {
  id: '00000000000000000000000001',
  actorId: null,
  action: 'server.update',
  targetKind: 'server',
  targetId: null,
  details: '{"name":"<script>alert(1)</script>"}',
  createdAt: 1000,
}
const view: ServerView = {
  ownUserId: 'own',
  canModerateMembers: false,
  canManageServer: false,
  canManageRoles: false,
  canManageChannels: false,
  canManageWebhooks: false,
  canViewAuditLog: true,
  canCreateInvites: false,
  canManageInvites: false,
  channels: [],
  directMessages: [],
  members: [],
  participants: [],
  activeVoice: null,
}
beforeEach(() => {
  HTMLDialogElement.prototype.showModal = function () {
    this.open = true
  }
  HTMLDialogElement.prototype.close = function () {
    this.open = false
  }
  vi.mocked(invokeBridge).mockReset().mockResolvedValue([row])
})
afterEach(cleanup)
it('validates exact actor and time filters', () => {
  expect(auditFilters('', ' invite.create ', '', '')).toEqual({
    actor: null,
    action: 'invite.create',
    since: null,
    until: null,
  })
  expect(auditFilters(row.id, '', '2026-01-01T12:00', '2026-01-01T13:00').until).toBeGreaterThan(0)
  expect(() => auditFilters('not-id', '', '', '')).toThrow('user ID')
  expect(() => auditFilters('', '', '2026-01-02T12:00', '2026-01-01T12:00')).toThrow('earlier')
  expect(() => auditFilters('', '', 'bad date', '')).toThrow('valid date')
  expect(() => auditFilters('', 'ø'.repeat(65), '', '')).toThrow('128 bytes')
})
it('shows system actors and treats event details as text', async () => {
  render(<AuditLog server="server" view={view} connected close={() => {}} />)
  expect(await screen.findByText('System')).toBeInTheDocument()
  fireEvent.click(screen.getByText('Inspect event'))
  expect(screen.getByText(row.details)).toBeInTheDocument()
  expect(document.querySelector('script')).toBeNull()
})
it('keeps filters with older pages and starts over when filters are applied or reset', async () => {
  const page = Array.from({ length: 50 }, (_, i) => ({
    ...row,
    id: String(100 - i).padStart(26, '0'),
  }))
  vi.mocked(invokeBridge).mockResolvedValue(page)
  render(<AuditLog server="server" view={view} connected close={() => {}} />)
  await screen.findByRole('button', { name: 'Load older events' })
  fireEvent.change(screen.getByLabelText('Action'), { target: { value: 'invite.create' } })
  fireEvent.click(screen.getByRole('button', { name: 'Apply filters' }))
  await waitFor(() =>
    expect(invokeBridge).toHaveBeenLastCalledWith('audit_page', {
      query: {
        serverId: 'server',
        before: null,
        actor: null,
        action: 'invite.create',
        since: null,
        until: null,
      },
    }),
  )
  await waitFor(() =>
    expect(screen.getByRole('button', { name: 'Load older events' })).toBeEnabled(),
  )
  vi.mocked(invokeBridge).mockResolvedValueOnce([row])
  fireEvent.click(screen.getByRole('button', { name: 'Load older events' }))
  await waitFor(() => expect(screen.getAllByRole('row')).toHaveLength(52))
  expect(invokeBridge).toHaveBeenLastCalledWith('audit_page', {
    query: expect.objectContaining({ before: page[49]!.id, action: 'invite.create' }),
  })
  fireEvent.click(screen.getByRole('button', { name: 'Reset filters' }))
  await waitFor(() =>
    expect(invokeBridge).toHaveBeenLastCalledWith('audit_page', {
      query: expect.objectContaining({ before: null, action: null }),
    }),
  )
})
it('clears rows after server denial and can refresh to recover', async () => {
  render(<AuditLog server="server" view={view} connected close={() => {}} />)
  await screen.findByText('server.update')
  vi.mocked(invokeBridge).mockRejectedValueOnce({ message: 'Permission denied' })
  fireEvent.click(screen.getByRole('button', { name: 'Refresh' }))
  await screen.findByText('Permission denied')
  expect(screen.queryByText('server.update')).not.toBeInTheDocument()
  fireEvent.click(screen.getByRole('button', { name: 'Refresh' }))
  await screen.findByText('server.update')
  expect(screen.queryByRole('alert')).not.toBeInTheDocument()
})
it('ignores stale responses after filter changes and disconnection', async () => {
  let resolve!: (rows: AuditEntryView[]) => void
  vi.mocked(invokeBridge).mockReturnValueOnce(
    new Promise((done) => {
      resolve = done
    }),
  )
  const result = render(<AuditLog server="server" view={view} connected close={() => {}} />)
  vi.mocked(invokeBridge).mockResolvedValueOnce([{ ...row, action: 'invite.create' }])
  fireEvent.change(screen.getByLabelText('Action'), { target: { value: 'invite.create' } })
  fireEvent.click(screen.getByRole('button', { name: 'Apply filters' }))
  await screen.findByText('invite.create')
  await act(async () => resolve([row]))
  expect(screen.queryByText('server.update')).not.toBeInTheDocument()
  result.rerender(<AuditLog server="server" view={view} connected={false} close={() => {}} />)
  expect(screen.queryByRole('table')).not.toBeInTheDocument()
  expect(screen.getByRole('button', { name: 'Refresh' })).toBeDisabled()
  result.rerender(
    <AuditLog
      server="server"
      view={{ ...view, canViewAuditLog: false }}
      connected
      close={() => {}}
    />,
  )
  expect(screen.queryByRole('table')).not.toBeInTheDocument()
  expect(screen.getByRole('button', { name: 'Apply filters' })).toBeDisabled()
})
