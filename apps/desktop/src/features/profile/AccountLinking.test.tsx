import '@testing-library/jest-dom/vitest'
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { useAppStore } from '../../store/app-store'
import { AccountLinking } from './AccountLinking'
import { linkServerAccount, reviewServerAccount } from './accountLinkApi'

vi.mock('./accountLinkApi', () => ({ linkServerAccount: vi.fn(), reviewServerAccount: vi.fn() }))

const servers = [
  'Friday Games',
  ...Array.from({ length: 8 }, (_, i) => `Studio ${i + 1}`),
  'Night Shift',
].map((label, i) => ({
  label,
  id: String(i),
  host: label.toLowerCase().replaceAll(' ', '') + '.example',
  port: 443,
  httpsPort: 443,
  unread: 0,
  status: { kind: 'connected' as const },
}))

beforeEach(() => {
  vi.resetAllMocks()
  useAppStore.setState({ servers })
  vi.mocked(reviewServerAccount).mockImplementation(async (id) =>
    id === '9'
      ? { status: 'serverSignInOnly', role: null, username: null, userId: null }
      : {
          status: 'eligible',
          role: id === '0' ? 'Admin' : 'Member',
          username: 'tommy',
          userId: `member-${id}`,
        },
  )
  vi.mocked(linkServerAccount).mockResolvedValue()
})
afterEach(cleanup)

it('reviews Friday Games, preserves the Admin account, and leaves the tenth server on per-device login', async () => {
  render(<AccountLinking />)
  expect(await screen.findByText('Server sign-in only')).toBeVisible()
  fireEvent.click(await screen.findByRole('button', { name: 'Review Friday Games' }))
  expect(screen.getByRole('heading', { name: 'Link your Friday Games account?' })).toHaveFocus()
  expect(screen.getByText('fridaygames.example · Signed in as tommy')).toBeVisible()
  expect(screen.getByText('Role: Admin')).toBeVisible()
  fireEvent.click(screen.getByRole('button', { name: 'Cancel' }))
  expect(linkServerAccount).not.toHaveBeenCalled()
  fireEvent.click(screen.getByRole('button', { name: 'Review Friday Games' }))
  fireEvent.click(screen.getByRole('button', { name: 'Link this account' }))
  await screen.findByText('1 account linked.')
  expect(linkServerAccount).toHaveBeenCalledWith('0', 'member-0', 'Admin')
  fireEvent.click(screen.getByRole('button', { name: 'Review 8 other accounts' }))
  expect(screen.getAllByRole('checkbox')).toHaveLength(8)
  fireEvent.click(screen.getByRole('button', { name: 'Link 8 accounts' }))
  await screen.findByText('8 accounts linked.')
  expect(screen.getAllByText('Linked to your profile')).toHaveLength(9)
  expect(linkServerAccount).toHaveBeenCalledTimes(9)
  expect(linkServerAccount).not.toHaveBeenCalledWith('9', expect.anything(), expect.anything())
  expect(screen.getByText('Server sign-in only')).toBeVisible()
})

it('keeps successes, skips deselected accounts and lets a failed member be reviewed again', async () => {
  useAppStore.setState({ servers: servers.slice(0, 3) })
  vi.mocked(linkServerAccount).mockRejectedValueOnce(new Error('Server went offline'))
  render(<AccountLinking />)
  fireEvent.click(await screen.findByRole('button', { name: 'Review 3 accounts together' }))
  fireEvent.click(screen.getByRole('checkbox', { name: 'Link Studio 2' }))
  fireEvent.click(screen.getByRole('button', { name: 'Link 2 accounts' }))
  await screen.findByText('Server went offline')
  expect(vi.mocked(linkServerAccount).mock.calls).toEqual([
    ['0', 'member-0', 'Admin'],
    ['1', 'member-1', 'Member'],
  ])
  expect(screen.getAllByText('Linked to your profile')).toHaveLength(1)
  fireEvent.click(screen.getByRole('button', { name: 'Check Friday Games again' }))
  await screen.findByRole('button', { name: 'Review Friday Games' })
  expect(linkServerAccount).toHaveBeenCalledTimes(2)
})

it('stops a reviewed batch after its in-flight request when the settings surface closes', async () => {
  let finish!: () => void
  vi.mocked(linkServerAccount).mockImplementationOnce(
    () =>
      new Promise<void>((resolve) => {
        finish = resolve
      }),
  )
  const view = render(<AccountLinking />)
  fireEvent.click(await screen.findByRole('button', { name: 'Review 9 accounts together' }))
  fireEvent.click(screen.getByRole('button', { name: 'Link 9 accounts' }))
  await waitFor(() => expect(linkServerAccount).toHaveBeenCalledTimes(1))
  view.unmount()
  await act(async () => finish())
  expect(linkServerAccount).toHaveBeenCalledTimes(1)
})
