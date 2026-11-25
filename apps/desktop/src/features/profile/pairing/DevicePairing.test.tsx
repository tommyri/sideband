import '@testing-library/jest-dom/vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, expect, it, vi } from 'vitest'
import { invokeBridge } from '../../../lib/tauri'
import { pairingLink, readInvitation, relayAddress } from './api'
import { DevicePairing } from './DevicePairing'
vi.mock('../../../lib/tauri', () => ({ invokeBridge: vi.fn() }))
afterEach(() => {
  cleanup()
  vi.resetAllMocks()
})
const target = { host: '2001:db8::1', port: 443, httpsPort: 8443 }
const code = 'ABCDE-FGHIJ-KLMNO'
const handlers = { onChooseRelay: vi.fn(), onClose: vi.fn(), onLinked: vi.fn() }
it('carries the same IPv6 relay and split HTTPS port through links and manual entry', () => {
  expect(readInvitation(pairingLink(target, code), '')).toEqual({ target, code })
  expect(readInvitation(relayAddress(target), code)).toEqual({ target, code })
  expect(() => readInvitation('sideband://other?relay=host#code', '')).toThrow()
  expect(() => readInvitation('host:443?https=0', code)).toThrow()
})
it('shows access and the phrase and requires an explicit match before allowing', async () => {
  vi.mocked(invokeBridge).mockImplementation(async (command, args) => {
    if (command === 'pair_start') return { kind: 'invite', code, target }
    if (args?.action === 'check')
      return { kind: 'review', device: 'Windows PC', phrase: 'lamp · desk · chair', memberships: 2 }
    return { kind: 'allowed' }
  })
  render(<DevicePairing source {...handlers} />)
  fireEvent.click(screen.getByRole('button', { name: 'Create one-time code' }))
  expect(await screen.findByLabelText('Sync server address')).toHaveValue(relayAddress(target))
  const approve = await screen.findByRole('button', { name: 'Allow Windows PC' }, { timeout: 3500 })
  expect(approve).toBeDisabled()
  expect(screen.getByText(/access to your 2 server memberships/)).toBeVisible()
  expect(screen.getByText('lamp · desk · chair')).toBeVisible()
  fireEvent.click(screen.getByRole('checkbox'))
  expect(invokeBridge).not.toHaveBeenCalledWith(
    'pair_update',
    expect.objectContaining({ action: 'approve' }),
  )
  fireEvent.click(approve)
  expect(await screen.findByText('Access approved')).toBeVisible()
})
it('accepts an opened invitation only after Request access and cancels a mismatched phrase', async () => {
  vi.mocked(invokeBridge).mockImplementation(async (command, args) =>
    command === 'pair_join'
      ? { kind: 'compare', device: 'Gaming PC', phrase: 'red · blue · green' }
      : args?.action === 'cancel'
        ? { kind: 'closed' }
        : { kind: 'waiting' },
  )
  render(<DevicePairing source={false} invitation={pairingLink(target, code)} {...handlers} />)
  expect(invokeBridge).not.toHaveBeenCalled()
  fireEvent.change(screen.getByLabelText('Name of this computer'), {
    target: { value: 'Gaming PC' },
  })
  fireEvent.click(screen.getByRole('button', { name: 'Request access' }))
  await screen.findByText('red · blue · green')
  expect(invokeBridge).toHaveBeenCalledWith(
    'pair_join',
    expect.objectContaining({ target, code, name: 'Gaming PC' }),
  )
  fireEvent.click(screen.getByRole('button', { name: 'Cancel linking' }))
  await screen.findByText('Linking canceled')
  expect(invokeBridge).not.toHaveBeenCalledWith(
    'pair_update',
    expect.objectContaining({ action: 'install' }),
  )
})
it('recovers polling after a relay outage and installs only from Finish linking', async () => {
  let polls = 0
  vi.mocked(invokeBridge).mockImplementation(async (command, args) => {
    if (command === 'pair_join')
      return { kind: 'compare', device: 'Mac', phrase: 'one · two · three' }
    if (args?.action === 'check' && ++polls === 1)
      throw { code: 'pair_network', message: 'Sync server offline' }
    return args?.action === 'install' ? { kind: 'linked', name: 'Sam' } : { kind: 'ready' }
  })
  render(<DevicePairing source={false} {...handlers} />)
  fireEvent.change(screen.getByLabelText('Pairing link or sync server address'), {
    target: { value: relayAddress(target) },
  })
  fireEvent.change(screen.getByLabelText('One-time code'), { target: { value: code } })
  fireEvent.click(screen.getByRole('button', { name: 'Request access' }))
  await screen.findByText('Sync server offline', {}, { timeout: 3500 })
  const finish = await screen.findByRole('button', { name: 'Finish linking' }, { timeout: 3500 })
  expect(invokeBridge).not.toHaveBeenCalledWith(
    'pair_update',
    expect.objectContaining({ action: 'install' }),
  )
  fireEvent.click(finish)
  fireEvent.click(await screen.findByRole('button', { name: 'Continue' }))
  await waitFor(() => expect(handlers.onLinked).toHaveBeenCalledWith('Sam'))
})
