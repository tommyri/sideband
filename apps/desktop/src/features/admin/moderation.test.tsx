import '@testing-library/jest-dom/vitest'
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { MemberManager } from './MemberManager'
import { invokeBridge } from '../../lib/tauri'
import type { ModerationPage } from '../../lib/generated/ModerationPage'
vi.mock('../../lib/tauri', () => ({ invokeBridge: vi.fn() }))
const member = {
  id: 'member',
  username: 'guest',
  name: 'Guest',
  roles: ['Everyone'],
  joinedAt: '1700000000000',
  lastSessionAt: null,
  disabled: false,
  banned: false,
  canKick: true,
  canBan: true,
}
const original: ModerationPage = {
  members: [
    member,
    { ...member, id: 'self', username: 'owner', name: 'Owner', canKick: false, canBan: false },
  ],
  bans: [],
  nextCursor: 'next',
  canBan: true,
}
let denied: boolean
let calls: unknown[]
beforeEach(() => {
  denied = false
  calls = []
  HTMLDialogElement.prototype.showModal = function () {
    this.open = true
  }
  HTMLDialogElement.prototype.close = function () {
    this.open = false
  }
  vi.mocked(invokeBridge)
    .mockReset()
    .mockImplementation(async (command, args) => {
      if (command === 'moderate_member') {
        calls.push(args)
        if (denied) throw new Error('Hierarchy denied')
        return
      }
      const options = args as { bans: boolean; before: string | null }
      return options.bans
        ? {
            ...original,
            members: [],
            bans: [
              {
                id: 'ban',
                userId: 'member',
                username: 'guest',
                name: 'Guest',
                roles: ['Everyone'],
                reason: '<script>unsafe</script>',
                createdAt: '1700000000000',
                expiresAt: null,
                bannedBy: 'owner',
                canUnban: true,
              },
            ],
            nextCursor: null,
          }
        : options.before
          ? { ...original, members: [], nextCursor: null }
          : structuredClone(original)
    })
})
afterEach(cleanup)
const mount = () => render(<MemberManager server="server" connected permitted close={vi.fn()} />)
it('guards self/hierarchy and requires confirmation before a timed ban', async () => {
  mount()
  await screen.findByText('Guest')
  const owner = screen.getByText('Owner').closest('tr')!
  expect(within(owner).getByRole('button', { name: 'Ban' })).toBeDisabled()
  fireEvent.click(screen.getAllByRole('button', { name: 'Ban' })[0]!)
  expect(screen.getByRole('button', { name: 'Confirm action' })).toBeDisabled()
  fireEvent.change(screen.getByLabelText(/^Reason/), { target: { value: 'Repeated harassment' } })
  fireEvent.change(screen.getByLabelText('Ban duration'), { target: { value: '604800' } })
  fireEvent.change(screen.getByLabelText('Type guest to confirm'), { target: { value: 'guest' } })
  fireEvent.click(screen.getByRole('button', { name: 'Confirm action' }))
  await screen.findByText('guest was banned.')
  expect(calls).toEqual([
    {
      serverId: 'server',
      action: {
        kind: 'ban',
        userId: 'member',
        reason: 'Repeated harassment',
        durationSeconds: 604800,
      },
    },
  ])
})
it('preserves denied drafts and bounds Unicode reasons', async () => {
  mount()
  await screen.findByText('Guest')
  denied = true
  fireEvent.click(screen.getAllByRole('button', { name: 'Kick' })[0]!)
  fireEvent.change(screen.getByLabelText(/^Reason/), { target: { value: '😀'.repeat(501) } })
  fireEvent.change(screen.getByLabelText('Type guest to confirm'), { target: { value: 'guest' } })
  expect(screen.getByRole('button', { name: 'Confirm action' })).toBeDisabled()
  fireEvent.change(screen.getByLabelText(/^Reason/), { target: { value: 'Reason retained' } })
  fireEvent.click(screen.getByRole('button', { name: 'Confirm action' }))
  await screen.findByText(/Hierarchy denied/)
  expect(screen.getByLabelText(/^Reason/)).toHaveValue('Reason retained')
  expect(screen.queryByText('guest was disconnected.')).not.toBeInTheDocument()
})
it('paginates search and resets the cursor when searching', async () => {
  mount()
  await screen.findByText('Guest')
  fireEvent.click(screen.getByRole('button', { name: 'Next page' }))
  await screen.findByText('No members match this search.')
  expect(invokeBridge).toHaveBeenCalledWith('get_moderation_page', {
    serverId: 'server',
    bans: false,
    before: 'next',
    query: '',
  })
  fireEvent.change(screen.getByLabelText('Search members'), { target: { value: 'Guest' } })
  fireEvent.click(screen.getByRole('button', { name: 'Search' }))
  await screen.findByText('Guest')
  expect(invokeBridge).toHaveBeenLastCalledWith('get_moderation_page', {
    serverId: 'server',
    bans: false,
    before: null,
    query: 'Guest',
  })
})
it('renders ban reasons as text and confirms unban by user identifier', async () => {
  mount()
  await screen.findByText('Guest')
  fireEvent.click(screen.getByRole('button', { name: 'Active bans' }))
  await screen.findByText('<script>unsafe</script>')
  expect(screen.getByText('Permanent')).toBeInTheDocument()
  fireEvent.click(screen.getByRole('button', { name: 'Unban' }))
  fireEvent.change(screen.getByLabelText('Type guest to confirm'), { target: { value: 'guest' } })
  fireEvent.click(screen.getByRole('button', { name: 'Confirm action' }))
  await screen.findByText('Ban lifted for guest.')
  expect(calls).toEqual([
    {
      serverId: 'server',
      action: { kind: 'unban', userId: 'member', reason: '', durationSeconds: null },
    },
  ])
})
it('discards pending responses when the server changes and disables disconnected controls', async () => {
  let resolve!: (value: ModerationPage) => void
  vi.mocked(invokeBridge).mockImplementation(
    () =>
      new Promise((r) => {
        resolve = r
      }),
  )
  const view = mount()
  view.rerender(<MemberManager server="other" connected={false} permitted close={vi.fn()} />)
  resolve(original)
  await waitFor(() =>
    expect(screen.getByRole('alert')).toHaveTextContent('Connect with moderator permissions'),
  )
  expect(screen.queryByText('Guest')).not.toBeInTheDocument()
  expect(screen.queryByRole('button', { name: 'Ban' })).not.toBeInTheDocument()
})
