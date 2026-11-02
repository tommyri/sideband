import '@testing-library/jest-dom/vitest'
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { ConnectDialog } from './ConnectDialog'
import { useAppStore } from '../../store/app-store'
import { approveFingerprint, connect, getSnapshot, probeServer } from '../../lib/api'
import { authError, parseAddress } from './validation'

vi.mock('../../lib/api', () => ({
  probeServer: vi.fn(),
  approveFingerprint: vi.fn(),
  connect: vi.fn(),
  getSnapshot: vi.fn(),
}))
const metadata = {
  name: 'Friends',
  registrationMode: 'invite',
  fingerprint: 'ab'.repeat(32),
  previousFingerprint: null,
  trusted: false,
}

beforeEach(() => {
  vi.resetAllMocks()
  HTMLDialogElement.prototype.showModal = function () {
    this.setAttribute('open', '')
  }
  HTMLDialogElement.prototype.close = function () {
    this.removeAttribute('open')
  }
  useAppStore.setState({
    connectOpen: true,
    connectServer: null,
    servers: [],
    selectedServerId: null,
  })
  vi.mocked(probeServer).mockResolvedValue(metadata)
  vi.mocked(approveFingerprint).mockResolvedValue()
  vi.mocked(getSnapshot).mockResolvedValue({ servers: [] })
})
afterEach(cleanup)

async function discover() {
  render(<ConnectDialog />)
  fireEvent.change(screen.getByLabelText('Server address'), {
    target: { value: 'ss://friends.example:5781' },
  })
  await screen.findByText('Friends · Invite required')
  fireEvent.click(screen.getByRole('button', { name: 'Continue' }))
}
async function trust() {
  await discover()
  fireEvent.click(screen.getByRole('button', { name: 'Trust this server' }))
  await screen.findByLabelText('Username')
}
describe('onboarding', () => {
  it('does not send credentials before explicit trust and submits invite registration', async () => {
    await discover()
    expect(connect).not.toHaveBeenCalled()
    expect(approveFingerprint).not.toHaveBeenCalled()
    fireEvent.click(screen.getByRole('button', { name: 'Trust this server' }))
    await screen.findByLabelText('Username')
    expect(approveFingerprint).toHaveBeenCalledWith({
      host: 'friends.example',
      port: 5781,
      fingerprint: metadata.fingerprint,
    })
    fireEvent.click(screen.getByRole('button', { name: 'Create account' }))
    fireEvent.change(screen.getByLabelText('Username'), { target: { value: 'friend' } })
    fireEvent.change(screen.getByLabelText('Password'), {
      target: { value: 'a unique passphrase' },
    })
    fireEvent.change(screen.getByLabelText('Invite code'), { target: { value: 'invite' } })
    vi.mocked(connect).mockResolvedValue({
      serverId: '[friends.example]:5781',
      label: 'Friends',
      status: { kind: 'connected' },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Create account and join' }))
    await waitFor(() => expect(useAppStore.getState().connectOpen).toBe(false))
    expect(connect).toHaveBeenCalledWith(
      expect.objectContaining({ mode: 'register', inviteCode: 'invite', username: 'friend' }),
    )
  })
  it('shows both fingerprints and requires explicit replacement on certificate change', async () => {
    vi.mocked(probeServer).mockResolvedValue({ ...metadata, previousFingerprint: 'cd'.repeat(32) })
    await discover()
    expect(screen.getByText('Server certificate changed')).toBeInTheDocument()
    expect(screen.getByText(/intercepting/)).toBeInTheDocument()
    expect(approveFingerprint).not.toHaveBeenCalled()
    fireEvent.click(screen.getByRole('button', { name: 'Replace trusted certificate' }))
    await screen.findByLabelText('Username')
    expect(approveFingerprint).toHaveBeenCalledTimes(1)
  })
  it('maps login errors and clears the failed password', async () => {
    await trust()
    fireEvent.change(screen.getByLabelText('Username'), { target: { value: 'friend' } })
    fireEvent.change(screen.getByLabelText('Password'), { target: { value: 'wrong-password' } })
    vi.mocked(connect).mockRejectedValue({ code: 'server_1', message: 'internal detail' })
    fireEvent.click(screen.getByRole('button', { name: 'Log in and join' }))
    expect(await screen.findByRole('alert')).toHaveTextContent('not accepted')
    expect(screen.getByLabelText('Password')).toHaveValue('')
    expect(useAppStore.getState().servers).toHaveLength(0)
  })
  it('rejects invalid registration without invoking the bridge', async () => {
    await trust()
    fireEvent.click(screen.getByRole('button', { name: 'Create account' }))
    fireEvent.change(screen.getByLabelText('Username'), { target: { value: 'UPPER' } })
    fireEvent.change(screen.getByLabelText('Password'), { target: { value: 'long-enough' } })
    fireEvent.change(screen.getByLabelText('Invite code'), { target: { value: 'invite' } })
    fireEvent.click(screen.getByRole('button', { name: 'Create account and join' }))
    expect(await screen.findByRole('alert')).toHaveTextContent('lowercase')
    expect(connect).not.toHaveBeenCalled()
  })
  it('ignores late discovery responses after the address changes', async () => {
    let finish!: (value: typeof metadata) => void
    vi.mocked(probeServer).mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve
        }),
    )
    render(<ConnectDialog />)
    fireEvent.change(screen.getByLabelText('Server address'), { target: { value: 'old.example' } })
    await waitFor(() => expect(probeServer).toHaveBeenCalled())
    fireEvent.change(screen.getByLabelText('Server address'), { target: { value: '' } })
    await act(async () => finish(metadata))
    expect(screen.queryByText('Friends · Invite required')).not.toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Continue' })).toBeDisabled()
  })
  it('validates addresses, IPv6, ports and password byte limits', () => {
    expect(parseAddress('host')).toEqual({ host: 'host', port: 5781, httpsPort: 5781 })
    expect(parseAddress('ss://[::1]:9000', '9443')).toEqual({
      host: '::1',
      port: 9000,
      httpsPort: 9443,
    })
    for (const value of ['https://host', 'host:0', 'host:65536', 'host/path', 'user@host', '::1'])
      expect(() => parseAddress(value)).toThrow()
    expect(authError('friend', 'é'.repeat(65), true)).toContain('128 bytes')
  })
  it('keeps keyboard focus within the dialog and focuses each new step', async () => {
    const geometry = vi
      .spyOn(HTMLElement.prototype, 'getClientRects')
      .mockReturnValue({ length: 1 } as DOMRectList)
    try {
      await discover()
      expect(screen.getByRole('heading', { name: 'Trust this server?' })).toHaveFocus()
      fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Tab', shiftKey: true })
      expect(screen.getByRole('button', { name: 'Trust this server' })).toHaveFocus()
      fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Tab' })
      expect(screen.getByRole('button', { name: 'Close' })).toHaveFocus()
      fireEvent.click(screen.getByRole('button', { name: 'Trust this server' }))
      await waitFor(() => expect(screen.getByLabelText('Username')).toHaveFocus())
    } finally {
      geometry.mockRestore()
    }
  })
  it('skips inputs inside collapsed advanced options during keyboard navigation', () => {
    const geometry = vi
      .spyOn(HTMLElement.prototype, 'getClientRects')
      .mockReturnValue({ length: 1 } as DOMRectList)
    try {
      render(<ConnectDialog />)
      expect(screen.getByLabelText('Server address')).toHaveFocus()
      fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Tab' })
      expect(screen.getByText('Advanced connection options')).toHaveFocus()
      fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Tab' })
      expect(screen.getByRole('button', { name: 'Cancel' })).toHaveFocus()
    } finally {
      geometry.mockRestore()
    }
  })
  it('restores focus to the invoking control when closed', () => {
    const opener = document.createElement('button')
    document.body.append(opener)
    opener.focus()
    render(<ConnectDialog />)
    fireEvent.click(screen.getByRole('button', { name: 'Close' }))
    expect(opener).toHaveFocus()
    opener.remove()
  })
})
