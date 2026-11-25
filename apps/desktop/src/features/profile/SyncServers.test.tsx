import '@testing-library/jest-dom/vitest'
import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { invokeBridge } from '../../lib/tauri'
import { SyncServers } from './SyncServers'
import type { SyncOptions } from '../../lib/generated/SyncOptions'

vi.mock('../../lib/tauri', () => ({ invokeBridge: vi.fn() }))
const friday = {
  id: '[games.example]:443',
  name: 'Friday Games',
  target: { host: 'games.example', port: 443, httpsPort: 443 },
  hasPublicIcon: false,
}
let options: SyncOptions
beforeEach(() => {
  vi.resetAllMocks()
  options = { available: [friday], chosen: null, unchecked: 0 }
  vi.mocked(invokeBridge).mockImplementation(async (command) =>
    command === 'sync_server_list' ? options : undefined,
  )
})
afterEach(cleanup)

it('previews the community, allows cancelling and acknowledges only a confirmed upload', async () => {
  render(<SyncServers />)
  await screen.findByText('games.example:443')
  expect(screen.getByText('FG')).toBeVisible()
  fireEvent.click(screen.getByRole('button', { name: 'Choose Friday Games' }))
  expect(screen.getByRole('heading', { name: 'Use Friday Games for sync?' })).toBeVisible()
  fireEvent.click(screen.getByRole('button', { name: 'Cancel' }))
  expect(invokeBridge).not.toHaveBeenCalledWith('choose_sync_server', expect.anything())
  fireEvent.click(screen.getByRole('button', { name: 'Choose Friday Games' }))
  fireEvent.click(screen.getByRole('button', { name: 'Use this server' }))
  expect(await screen.findByText('Encrypted profile copy saved.')).toBeVisible()
  expect(invokeBridge).toHaveBeenCalledWith('choose_sync_server', { serverId: friday.id })
  fireEvent.click(screen.getByRole('button', { name: 'Verify saved copy' }))
  expect(await screen.findByText('Sideband opened and verified your encrypted copy.')).toBeVisible()
})

it('shows a distinct recovery action and can recheck an empty eligibility list', async () => {
  options = { available: [], chosen: null, unchecked: 1 }
  const manage = vi.fn()
  render(<SyncServers onManageBackup={manage} />)
  expect(await screen.findByRole('heading', { name: 'No sync servers available' })).toBeVisible()
  fireEvent.click(screen.getByRole('button', { name: 'Manage recovery backup' }))
  expect(manage).toHaveBeenCalledOnce()
  options.available = [friday]
  fireEvent.click(screen.getByRole('button', { name: 'Check again' }))
  expect(await screen.findByRole('button', { name: 'Choose Friday Games' })).toBeVisible()
})

it('preserves the confirmation when the transfer fails and lets the user retry', async () => {
  render(<SyncServers />)
  fireEvent.click(await screen.findByRole('button', { name: 'Choose Friday Games' }))
  vi.mocked(invokeBridge).mockRejectedValueOnce({ message: 'Server unavailable' })
  fireEvent.click(screen.getByRole('button', { name: 'Use this server' }))
  expect(await screen.findByRole('alert')).toHaveTextContent('Server unavailable')
  expect(screen.queryByText('Your sync server')).toBeNull()
  fireEvent.click(screen.getByRole('button', { name: 'Use this server' }))
  expect(await screen.findByText('Encrypted profile copy saved.')).toBeVisible()
})

it('keeps a saved server visible after restart when it is offline', async () => {
  options = { available: [], chosen: friday, unchecked: 1 }
  render(<SyncServers />)
  expect(await screen.findByRole('heading', { name: 'Your sync server' })).toBeVisible()
  expect(screen.getByText('Friday Games')).toBeVisible()
  expect(screen.getByText(/server is unavailable/)).toBeVisible()
  expect(screen.queryByRole('button', { name: 'Verify saved copy' })).toBeNull()
})
