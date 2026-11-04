import '@testing-library/jest-dom/vitest'
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { ChannelManager } from './ChannelManager'
import { validFields } from './channels'
import { invokeBridge } from '../../lib/tauri'
import { receiveChatEvent } from '../chat/api'
import type { ServerView } from '../../lib/generated/ServerView'
import type { ChannelAdminSnapshot } from '../../lib/generated/ChannelAdminSnapshot'
vi.mock('../../lib/tauri', () => ({ invokeBridge: vi.fn() }))
vi.mock('../voice/store', () => ({ refreshVoice: vi.fn(async () => {}) }))
const category = {
  id: 'category',
  name: 'General',
  kind: 1,
  topic: '',
  parentId: null,
  position: 0,
  profile: 1,
  bitrate: null,
  userLimit: null,
}
const text = {
  ...category,
  id: 'text',
  name: 'chat',
  kind: 2,
  parentId: 'category',
  position: 0,
  topic: 'Old topic',
}
const voice = {
  ...category,
  id: 'voice',
  name: 'Lounge',
  kind: 3,
  parentId: 'category',
  position: 1,
}
const snapshot: ChannelAdminSnapshot = {
  channels: [category, text, voice],
  profiles: [
    { id: 1, name: 'Voice', minBitrate: 48000, maxBitrate: 96000, defaultBitrate: 80000 },
    { id: 2, name: 'Music', minBitrate: 96000, maxBitrate: 192000, defaultBitrate: 128000 },
    { id: 3, name: 'Low latency', minBitrate: 64000, maxBitrate: 64000, defaultBitrate: 64000 },
  ],
}
const view: ServerView = {
  ownUserId: 'own',
  canManageServer: false,
  canManageRoles: false,
  canManageChannels: true,
  canManageWebhooks: false,
  canViewAuditLog: false,
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
  vi.mocked(invokeBridge)
    .mockReset()
    .mockImplementation(async (command) =>
      command === 'channel_admin_snapshot' ? structuredClone(snapshot) : undefined,
    )
})
afterEach(cleanup)
const mount = () =>
  render(<ChannelManager server="server" view={view} connected close={() => {}} />)
const row = (name: string) =>
  screen.getAllByRole('row').find((r) => within(r).queryByText(name, { exact: true }))!
it('validates names, Unicode counts, topics and voice limits', () => {
  const fields = { name: ' Music ', topic: '', profile: 2, bitrate: 128000, userLimit: null }
  expect(validFields(fields, 3, snapshot.profiles).name).toBe('Music')
  expect(validFields({ ...fields, name: '😀'.repeat(64) }, 3, snapshot.profiles).name).toHaveLength(
    128,
  )
  for (const change of [
    { name: '' },
    { name: 'a'.repeat(65) },
    { topic: 'a'.repeat(1025) },
    { bitrate: 95000 },
    { userLimit: 100 },
    { userLimit: 1.5 },
  ])
    expect(() => validFields({ ...fields, ...change }, 3, snapshot.profiles)).toThrow()
})
it('creates voice channels using the selected category and profile constraints', async () => {
  mount()
  await screen.findByText('Lounge')
  fireEvent.click(screen.getByRole('button', { name: 'New channel' }))
  fireEvent.change(screen.getByLabelText('Name'), { target: { value: 'Music room' } })
  fireEvent.change(screen.getByLabelText('Type'), { target: { value: '3' } })
  fireEvent.change(screen.getByLabelText('Category'), { target: { value: 'category' } })
  fireEvent.change(screen.getByLabelText('Voice profile'), { target: { value: '2' } })
  expect(screen.getByLabelText('Bitrate')).toHaveAttribute('min', '96000')
  expect(screen.getByLabelText('Bitrate')).toHaveAttribute('max', '192000')
  fireEvent.click(screen.getByRole('checkbox', { name: /Use profile default/ }))
  fireEvent.change(screen.getByLabelText('Bitrate'), { target: { value: '144000' } })
  fireEvent.change(screen.getByLabelText(/User limit/), { target: { value: '8' } })
  fireEvent.click(screen.getByRole('button', { name: 'Create channel' }))
  await screen.findByText('Channel created.')
  expect(invokeBridge).toHaveBeenCalledWith('channel_admin_action', {
    serverId: 'server',
    action: {
      kind: 'create',
      channelKind: 3,
      parentId: 'category',
      fields: { name: 'Music room', topic: '', profile: 2, bitrate: 144000, userLimit: 8 },
    },
  })
})
it('prevents nested categories and keeps form contents after a save denial', async () => {
  mount()
  await screen.findByText('Lounge')
  fireEvent.click(screen.getByRole('button', { name: 'New channel' }))
  fireEvent.change(screen.getByLabelText('Type'), { target: { value: '1' } })
  expect(screen.queryByLabelText('Category')).not.toBeInTheDocument()
  fireEvent.click(screen.getByRole('button', { name: 'Cancel' }))
  fireEvent.click(within(row('chat')).getByRole('button', { name: 'Edit' }))
  fireEvent.change(screen.getByLabelText('Name'), { target: { value: 'renamed' } })
  fireEvent.change(screen.getByLabelText('Topic'), { target: { value: '' } })
  vi.mocked(invokeBridge).mockRejectedValueOnce({ message: 'Permission denied' })
  fireEvent.click(screen.getByRole('button', { name: 'Save changes' }))
  await screen.findByText(/Permission denied/)
  expect(screen.getByLabelText('Name')).toHaveValue('renamed')
  expect(invokeBridge).toHaveBeenLastCalledWith('channel_admin_action', {
    serverId: 'server',
    action: expect.objectContaining({
      kind: 'edit',
      channel: 'text',
      fields: expect.objectContaining({ topic: '' }),
    }),
  })
})
it('moves to the root and reorders siblings using their server positions', async () => {
  mount()
  await screen.findByText('Lounge')
  fireEvent.click(screen.getByRole('button', { name: 'Move Lounge up' }))
  await screen.findByText('Channel moved.')
  expect(invokeBridge).toHaveBeenCalledWith('channel_admin_action', {
    serverId: 'server',
    action: { kind: 'move', channel: 'voice', parentId: 'category', position: 0 },
  })
  fireEvent.click(within(row('Lounge')).getByRole('button', { name: 'Move' }))
  fireEvent.change(screen.getByLabelText('Category'), { target: { value: '' } })
  fireEvent.click(screen.getByRole('button', { name: 'Move channel' }))
  await waitFor(() =>
    expect(invokeBridge).toHaveBeenCalledWith('channel_admin_action', {
      serverId: 'server',
      action: { kind: 'move', channel: 'voice', parentId: null, position: 0 },
    }),
  )
})
it('requires typed confirmation for category cascade deletion and retains it on denial', async () => {
  mount()
  await screen.findByText('Lounge')
  fireEvent.click(within(row('Category')).getByRole('button', { name: 'Delete' }))
  const confirm = within(screen.getByRole('alertdialog'))
  expect(confirm.getByText(/including channels hidden from you/)).toBeInTheDocument()
  expect(confirm.getByRole('button', { name: 'Confirm delete' })).toBeDisabled()
  fireEvent.change(confirm.getByLabelText('Type General to confirm'), {
    target: { value: 'General' },
  })
  vi.mocked(invokeBridge).mockRejectedValueOnce({ message: 'Deletion denied' })
  fireEvent.click(confirm.getByRole('button', { name: 'Confirm delete' }))
  await screen.findByText(/Deletion denied/)
  expect(confirm.getByRole('button', { name: 'Confirm delete' })).toBeEnabled()
  expect(screen.getByText('Lounge')).toBeInTheDocument()
})
it('does not overwrite an edit when another admin changes the same settings', async () => {
  mount()
  await screen.findByText('Lounge')
  fireEvent.click(within(row('chat')).getByRole('button', { name: 'Edit' }))
  fireEvent.change(screen.getByLabelText('Name'), { target: { value: 'my draft' } })
  vi.mocked(invokeBridge).mockResolvedValue({
    ...snapshot,
    channels: [category, { ...text, topic: 'Changed remotely' }, voice],
  })
  act(() =>
    receiveChatEvent({ serverId: 'server', occurredAtMs: 0, event: { kind: 'channelsChanged' } }),
  )
  await screen.findByText(/This channel changed or was removed/)
  expect(screen.getByLabelText('Name')).toHaveValue('my draft')
  expect(screen.getByRole('button', { name: 'Save changes' })).toBeDisabled()
})
it('does not load without permission and discards drafts when disconnected', async () => {
  const result = render(
    <ChannelManager
      server="server"
      view={{ ...view, canManageChannels: false }}
      connected
      close={() => {}}
    />,
  )
  expect(invokeBridge).not.toHaveBeenCalled()
  expect(screen.getByRole('button', { name: 'New channel' })).toBeDisabled()
  result.rerender(<ChannelManager server="server" view={view} connected close={() => {}} />)
  await screen.findByText('Lounge')
  fireEvent.click(screen.getByRole('button', { name: 'New channel' }))
  fireEvent.change(screen.getByLabelText('Name'), { target: { value: 'draft' } })
  result.rerender(<ChannelManager server="server" view={view} connected={false} close={() => {}} />)
  expect(screen.queryByDisplayValue('draft')).not.toBeInTheDocument()
  expect(screen.getByRole('button', { name: 'New channel' })).toBeDisabled()
})
