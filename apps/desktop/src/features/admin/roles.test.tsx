import '@testing-library/jest-dom/vitest'
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { RoleManager } from './RoleManager'
import { has, toggle, setOverride, overrideValue, permissions } from './roles'
import { invokeBridge } from '../../lib/tauri'
import { receiveChatEvent } from '../chat/api'
import type { ServerView } from '../../lib/generated/ServerView'
import type { RoleSnapshot } from '../../lib/generated/RoleSnapshot'
vi.mock('../../lib/tauri', () => ({ invokeBridge: vi.fn() }))
vi.mock('../voice/store', () => ({ refreshVoice: vi.fn(async () => {}) }))
const data: RoleSnapshot = {
  administrator: false,
  manageRoles: true,
  manageChannels: true,
  supportsV2: true,
  createPosition: 9,
  roles: [
    {
      id: 'manager',
      name: 'Manager',
      color: null,
      permissions: '12',
      position: 10,
      isDefault: false,
      editable: false,
    },
    {
      id: 'helper',
      name: 'Helper',
      color: 0x123456,
      permissions: '1024',
      position: 2,
      isDefault: false,
      editable: true,
    },
    {
      id: 'everyone',
      name: '@everyone',
      color: null,
      permissions: '512',
      position: 0,
      isDefault: true,
      editable: true,
    },
  ],
  members: [
    { id: 'own', name: 'Owner', roleIds: ['manager'], editable: false },
    { id: 'guest', name: 'Guest', roleIds: [], editable: true },
  ],
}
const view: ServerView = {
  ownUserId: 'own',
  canManageServer: false,
  canManageRoles: true,
  canManageChannels: true,
  canManageWebhooks: false,
  canViewAuditLog: false,
  canCreateInvites: false,
  canManageInvites: false,
  channels: [
    {
      id: 'chat',
      name: 'chat',
      kind: 2,
      parentId: null,
      profile: 'Voice',
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
let current: RoleSnapshot
beforeEach(() => {
  current = structuredClone(data)
  HTMLDialogElement.prototype.showModal = function () {
    this.open = true
  }
  HTMLDialogElement.prototype.close = function () {
    this.open = false
  }
  vi.mocked(invokeBridge)
    .mockReset()
    .mockImplementation(async (command) =>
      command === 'role_admin_snapshot'
        ? structuredClone(current)
        : command === 'role_overrides'
          ? [{ roleId: 'helper', allow: '1099511627776', deny: '1024' }]
          : undefined,
    )
})
afterEach(cleanup)
const mount = () => render(<RoleManager server="server" view={view} connected close={() => {}} />)
const ready = async () => {
  await screen.findByText('Helper')
  await waitFor(() => expect(screen.getByRole('button', { name: 'Create role' })).toBeEnabled())
}
it('keeps full-width masks and normalizes tri-state bits without losing other overrides', () => {
  const high = (1n << 60n).toString()
  expect(has(toggle(high, 14, true), 60)).toBe(true)
  expect(has(toggle(high, 14, true), 14)).toBe(true)
  const both = { roleId: 'r', allow: '1024', deny: (1024n | (1n << 60n)).toString() }
  expect(overrideValue(both, 10)).toBe('allow')
  const inherited = setOverride(both, 10, 'inherit')
  expect(inherited.allow).toBe('0')
  expect(inherited.deny).toBe(high)
  expect(permissions[14][2]).toContain('Connect and View channel')
})
it('protects hierarchy, default role deletion and administrator grants', async () => {
  mount()
  await ready()
  expect(screen.getByRole('button', { name: 'Edit Manager' })).toBeDisabled()
  expect(screen.getByRole('button', { name: 'Delete @everyone' })).toBeDisabled()
  expect(screen.getByRole('button', { name: 'Move Helper up' })).toBeDisabled()
  fireEvent.click(screen.getByRole('button', { name: 'Edit Helper' }))
  expect(screen.getByRole('checkbox', { name: /^Administrator/ })).toBeDisabled()
  fireEvent.click(screen.getByRole('checkbox', { name: 'Use default color' }))
  fireEvent.click(screen.getByRole('button', { name: 'Save role' }))
  await screen.findByText('Permissions saved.')
  expect(invokeBridge).toHaveBeenCalledWith('role_admin_action', {
    serverId: 'server',
    action: {
      kind: 'edit',
      role: 'helper',
      fields: { name: 'Helper', color: null, permissions: '1024' },
    },
  })
})
it('preserves drafts on server denial and blocks observed concurrent role edits', async () => {
  mount()
  await ready()
  fireEvent.click(screen.getByRole('button', { name: 'Edit Helper' }))
  fireEvent.change(screen.getByLabelText('Name'), { target: { value: 'Draft' } })
  vi.mocked(invokeBridge).mockImplementation(async (command) => {
    if (command === 'role_admin_action') throw new Error('Permission denied')
    return structuredClone(current)
  })
  fireEvent.click(screen.getByRole('button', { name: 'Save role' }))
  await screen.findByText('Permission denied')
  expect(screen.getByLabelText('Name')).toHaveValue('Draft')
  current.roles[1]!.name = 'Changed remotely'
  act(() =>
    receiveChatEvent({ serverId: 'server', occurredAtMs: 1, event: { kind: 'membersChanged' } }),
  )
  await screen.findByText(/This role changed on the server/)
  expect(screen.getByRole('button', { name: 'Save role' })).toBeDisabled()
})
it('assigns editable roles and keeps everyone implicit', async () => {
  mount()
  await ready()
  fireEvent.click(screen.getByRole('button', { name: 'Member roles' }))
  expect(screen.getByRole('button', { name: 'Assign roles to Owner' })).toBeDisabled()
  fireEvent.click(screen.getByRole('button', { name: 'Assign roles to Guest' }))
  expect(screen.getByRole('checkbox', { name: /everyone, always/ })).toBeChecked()
  expect(screen.getByRole('checkbox', { name: 'Manager (protected)' })).toBeDisabled()
  fireEvent.click(screen.getByRole('checkbox', { name: 'Helper' }))
  fireEvent.click(screen.getByRole('button', { name: 'Save assignments' }))
  await screen.findByText('Permissions saved.')
  expect(invokeBridge).toHaveBeenCalledWith('role_admin_action', {
    serverId: 'server',
    action: { kind: 'assign', member: 'guest', roles: ['helper'] },
  })
})
it('loads saved overrides and preserves unshown bits when saving', async () => {
  mount()
  await ready()
  fireEvent.click(screen.getByRole('button', { name: 'Channel overrides' }))
  fireEvent.change(screen.getByLabelText('Channel'), { target: { value: 'chat' } })
  fireEvent.change(screen.getByLabelText('Role'), { target: { value: 'helper' } })
  await waitFor(() => expect(screen.getByLabelText('Send messages')).toHaveValue('deny'))
  fireEvent.change(screen.getByLabelText('Send messages'), { target: { value: 'allow' } })
  fireEvent.click(screen.getByRole('button', { name: 'Save overrides' }))
  await screen.findByText('Permissions saved.')
  expect(invokeBridge).toHaveBeenCalledWith('role_admin_action', {
    serverId: 'server',
    action: {
      kind: 'override',
      channel: 'chat',
      role: 'helper',
      allow: '1099511628800',
      deny: '0',
    },
  })
})
it('never edits inherited defaults after a failed override read', async () => {
  mount()
  await ready()
  vi.mocked(invokeBridge).mockImplementation(async (command) => {
    if (command === 'role_overrides') throw new Error('Read denied')
    return structuredClone(current)
  })
  fireEvent.click(screen.getByRole('button', { name: 'Channel overrides' }))
  fireEvent.change(screen.getByLabelText('Channel'), { target: { value: 'chat' } })
  fireEvent.change(screen.getByLabelText('Role'), { target: { value: 'helper' } })
  await screen.findByText('Read denied')
  expect(screen.getByRole('button', { name: 'Save overrides' })).toBeDisabled()
})
it('requires typed deletion confirmation and sends atomic moves', async () => {
  mount()
  await ready()
  fireEvent.click(screen.getByRole('button', { name: 'Move Helper down' }))
  await screen.findByText('Permissions saved.')
  expect(invokeBridge).toHaveBeenCalledWith('role_admin_action', {
    serverId: 'server',
    action: { kind: 'move', role: 'helper', before: null },
  })
  await waitFor(() => expect(screen.getByRole('button', { name: 'Delete Helper' })).toBeEnabled())
  fireEvent.click(screen.getByRole('button', { name: 'Delete Helper' }))
  expect(screen.getByRole('button', { name: 'Delete role permanently' })).toBeDisabled()
  fireEvent.change(screen.getByLabelText('Type the role name to confirm'), {
    target: { value: 'Helper' },
  })
  fireEvent.click(screen.getByRole('button', { name: 'Delete role permanently' }))
  await screen.findByText('Permissions saved.')
  expect(invokeBridge).toHaveBeenCalledWith('role_admin_action', {
    serverId: 'server',
    action: { kind: 'delete', role: 'helper' },
  })
})
it('discards editors on disconnect and disables mutation entry points', async () => {
  const rendered = mount()
  await ready()
  fireEvent.click(screen.getByRole('button', { name: 'Edit Helper' }))
  rendered.rerender(<RoleManager server="server" view={view} connected={false} close={() => {}} />)
  expect(screen.queryByRole('button', { name: 'Save role' })).not.toBeInTheDocument()
  expect(screen.getByRole('button', { name: 'Refresh' })).toBeDisabled()
})
