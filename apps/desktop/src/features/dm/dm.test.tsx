import '@testing-library/jest-dom/vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { DmDialog } from './DmDialog'
import { DmList } from './DmList'
import { DmHeader } from './DmHeader'
import { dmName, sortedDms, canAddMembers, dmOnline } from './model'
import { dmAction, openDm } from './api'
import { refreshVoice, useVoiceStore } from '../voice/store'
import type { ServerView } from '../../lib/generated/ServerView'
import type { MemberView } from '../../lib/generated/MemberView'
import type { DmView } from '../../lib/generated/DmView'
vi.mock('./api', async (original) => ({
  ...(await original<typeof import('./api')>()),
  dmAction: vi.fn(),
}))
vi.mock('../../lib/tauri', () => ({ invokeBridge: vi.fn() }))
vi.mock('../voice/store', async (original) => ({
  ...(await original<typeof import('../voice/store')>()),
  refreshVoice: vi.fn(),
}))
import { invokeBridge } from '../../lib/tauri'

const member = (id: string): MemberView => ({
  id,
  displayName: 'Same display name',
  username: id,
  roleName: 'Member',
  roleColor: null,
  rolePosition: 0,
  presence: 'online',
  activity: '',
  canKick: false,
  canBan: false,
  canMute: false,
  canMove: false,
  volume: 100,
  localMuted: false,
})
const conversation: DmView = {
  id: 'group',
  name: 'Raid',
  kind: 5,
  memberIds: ['own', 'alice', 'bob'],
  creatorId: 'own',
  lastActivity: 100,
  unread: true,
  mentions: 2,
}
const view: ServerView = {
  ownUserId: 'own',
  canCreateInvites: false,
  canManageInvites: false,
  channels: [],
  directMessages: [conversation],
  members: ['own', 'alice', 'bob', 'carol'].map(member),
  participants: [],
  activeVoice: null,
}
beforeEach(() => {
  vi.clearAllMocks()
  HTMLDialogElement.prototype.showModal = function () {
    this.setAttribute('open', '')
  }
  HTMLDialogElement.prototype.close = function () {
    this.removeAttribute('open')
  }
})
afterEach(cleanup)
it('sorts by last activity, resolves participant labels and keeps presence/unread accessible', () => {
  const pair = {
    ...conversation,
    id: 'pair',
    kind: 4,
    memberIds: ['own', 'alice'],
    lastActivity: 200,
    name: '',
  }
  expect(sortedDms([conversation, pair]).map((dm) => dm.id)).toEqual(['pair', 'group'])
  const members = new Map(view.members.map((m) => [m.id, m]))
  expect(dmName(pair, members, 'own')).toBe('Same display name')
  expect(dmOnline(pair, members, 'own')).toBe(true)
  expect(dmName(conversation, members, 'own')).toBe('Raid')
  const select = vi.fn()
  render(
    <DmList
      view={{ ...view, directMessages: [conversation, pair] }}
      selected="pair"
      connected
      select={select}
      create={vi.fn()}
    />,
  )
  expect(screen.getAllByRole('button', { name: /^Open/ })[0]).toHaveAttribute(
    'aria-current',
    'page',
  )
  fireEvent.click(screen.getByRole('button', { name: 'Open group message Raid' }))
  expect(select).toHaveBeenCalledWith('group')
  expect(screen.getAllByLabelText('2 mentions')).toHaveLength(2)
  expect(screen.getAllByLabelText('Participant online')).toHaveLength(2)
})
it('requires two other people for groups and sends immutable IDs plus the name', async () => {
  vi.mocked(dmAction).mockResolvedValue({ channelId: 'created', warning: null })
  const complete = vi.fn()
  render(
    <DmDialog
      server="server-a"
      view={view}
      mode="create"
      connected
      close={vi.fn()}
      complete={complete}
    />,
  )
  fireEvent.click(screen.getByRole('checkbox', { name: 'Group conversation' }))
  fireEvent.change(screen.getByLabelText('Group name (optional)'), {
    target: { value: 'Game night' },
  })
  fireEvent.click(screen.getByRole('checkbox', { name: /@alice/ }))
  expect(screen.getByRole('button', { name: 'Open conversation' })).toBeDisabled()
  fireEvent.click(screen.getByRole('checkbox', { name: /@bob/ }))
  fireEvent.click(screen.getByRole('button', { name: 'Open conversation' }))
  await waitFor(() =>
    expect(complete).toHaveBeenCalledWith({ channelId: 'created', warning: null }),
  )
  expect(dmAction).toHaveBeenCalledWith('server-a', {
    kind: 'create',
    userIds: ['alice', 'bob'],
    name: 'Game night',
  })
})
it('retains selection on a server denial and does not claim a conversation opened', async () => {
  vi.mocked(dmAction).mockRejectedValue({ message: 'That user is unavailable.' })
  const complete = vi.fn()
  render(
    <DmDialog server="s" view={view} mode="create" connected close={vi.fn()} complete={complete} />,
  )
  fireEvent.click(screen.getByRole('checkbox', { name: /@alice/ }))
  fireEvent.click(screen.getByRole('button', { name: 'Open conversation' }))
  expect(await screen.findByRole('alert')).toHaveTextContent('unavailable')
  expect(screen.getByRole('checkbox', { name: /@alice/ })).toBeChecked()
  expect(complete).not.toHaveBeenCalled()
})
it('limits adds to nonmembers and gates creator controls and disconnected changes', () => {
  expect(canAddMembers(conversation, 'alice')).toBe(false)
  expect(canAddMembers({ ...conversation, memberIds: Array(10).fill('own') }, 'own')).toBe(false)
  const { unmount } = render(
    <DmHeader
      conversation={conversation}
      view={{ ...view, ownUserId: 'alice' }}
      connected
      manage={vi.fn()}
    />,
  )
  expect(screen.getByRole('button', { name: 'Add members' })).toBeDisabled()
  expect(screen.getByRole('button', { name: 'Leave group' })).toBeEnabled()
  unmount()
  render(
    <DmDialog
      server="s"
      view={view}
      conversation={conversation}
      mode="add"
      connected={false}
      close={vi.fn()}
      complete={vi.fn()}
    />,
  )
  expect(screen.queryByRole('checkbox', { name: /@alice/ })).not.toBeInTheDocument()
  expect(screen.getByRole('checkbox', { name: /@carol/ })).toBeInTheDocument()
  expect(screen.getByRole('button', { name: 'Add members' })).toBeDisabled()
})
it('passes a naming warning to the workspace without retrying creation', async () => {
  const result = { channelId: 'created', warning: 'Created, but naming failed' }
  vi.mocked(dmAction).mockResolvedValue(result)
  const complete = vi.fn()
  render(
    <DmDialog server="s" view={view} mode="create" connected close={vi.fn()} complete={complete} />,
  )
  fireEvent.click(screen.getByRole('checkbox', { name: /@alice/ }))
  fireEvent.click(screen.getByRole('button', { name: 'Open conversation' }))
  await waitFor(() => expect(complete).toHaveBeenCalledWith(result))
  expect(dmAction).toHaveBeenCalledOnce()
})
it('opening a partner selects the returned existing DM only on its own server', async () => {
  vi.mocked(invokeBridge).mockResolvedValue({ channelId: 'existing', warning: null })
  useVoiceStore.setState({ selected: { other: 'keep-this' } })
  await openDm('server-a', 'alice')
  expect(invokeBridge).toHaveBeenCalledWith('dm_action', {
    serverId: 'server-a',
    action: { kind: 'create', userIds: ['alice'], name: '' },
  })
  expect(refreshVoice).toHaveBeenCalledWith('server-a')
  expect(useVoiceStore.getState().selected).toEqual({ other: 'keep-this', 'server-a': 'existing' })
})
