import '@testing-library/jest-dom/vitest'
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { ConnectDialog } from './ConnectDialog'
import { useAppStore } from '../../store/app-store'
import {
  approveFingerprint,
  connect,
  getSnapshot,
  getPublicServerIcon,
  probeServer,
  enablePublicCertificate,
} from '../../lib/api'
import { accountErrors, parseAddress } from './validation'

vi.mock('../../lib/api', () => ({
  probeServer: vi.fn(),
  approveFingerprint: vi.fn(),
  enablePublicCertificate: vi.fn(),
  connect: vi.fn(),
  getSnapshot: vi.fn(),
  getPublicServerIcon: vi.fn(),
}))
const metadata = {
  name: 'Friends',
  capabilities: [],
  hasPublicIcon: false,
  registrationMode: 'invite',
  fingerprint: 'ab'.repeat(32),
  previousFingerprint: null,
  trusted: false,
  publicCertificate: false,
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
  fireEvent.click(screen.getByRole('button', { name: 'Find server' }))
  await waitFor(() => expect(screen.queryByLabelText('Server address')).not.toBeInTheDocument())
}
async function trust() {
  await discover()
  fireEvent.click(screen.getByRole('checkbox', { name: /I compared/ }))
  fireEvent.click(screen.getByRole('button', { name: 'Trust this server' }))
  await screen.findByLabelText('Username')
}
describe('onboarding', () => {
  it('does not send credentials before explicit trust and submits invite registration', async () => {
    await discover()
    expect(connect).not.toHaveBeenCalled()
    expect(approveFingerprint).not.toHaveBeenCalled()
    fireEvent.click(screen.getByRole('checkbox', { name: /I compared/ }))
    fireEvent.click(screen.getByRole('button', { name: 'Trust this server' }))
    await screen.findByLabelText('Username')
    expect(approveFingerprint).toHaveBeenCalledWith({
      host: 'friends.example',
      port: 5781,
      fingerprint: metadata.fingerprint,
    })
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
  it('shows the invitation identity and fetches its logo only after certificate approval', async () => {
    vi.mocked(probeServer).mockResolvedValue({ ...metadata, hasPublicIcon: true })
    vi.mocked(getPublicServerIcon).mockResolvedValue('data:image/png;base64,AA==')
    await discover()
    expect(getPublicServerIcon).not.toHaveBeenCalled()
    fireEvent.click(screen.getByRole('checkbox', { name: /I compared/ }))
    fireEvent.click(screen.getByRole('button', { name: 'Trust this server' }))
    expect(await screen.findByText('friends.example:5781')).toBeVisible()
    expect(screen.getByText('Friends')).toBeVisible()
    expect(screen.getByLabelText('Invite code')).toBeVisible()
    await waitFor(() =>
      expect(document.querySelector('.server-preview img')).toHaveAttribute(
        'src',
        'data:image/png;base64,AA==',
      ),
    )
    fireEvent.error(document.querySelector('.server-preview img')!)
    expect(screen.getByText('F')).toBeVisible()
  })
  it('shows initials when the server has no public icon', async () => {
    await trust()
    expect(screen.getByText('F')).toBeVisible()
    expect(getPublicServerIcon).not.toHaveBeenCalled()
  })
  it('shows both fingerprints and requires explicit replacement on certificate change', async () => {
    vi.mocked(probeServer).mockResolvedValue({ ...metadata, previousFingerprint: 'cd'.repeat(32) })
    await discover()
    expect(screen.getByText('Server certificate changed')).toBeInTheDocument()
    expect(screen.getByText(/intercepting/)).toBeInTheDocument()
    expect(approveFingerprint).not.toHaveBeenCalled()
    fireEvent.click(screen.getByRole('button', { name: 'Replace trusted certificate' }))
    expect(screen.getByRole('checkbox')).toHaveFocus()
    expect(approveFingerprint).not.toHaveBeenCalled()
    fireEvent.click(screen.getByRole('checkbox'))
    fireEvent.click(screen.getByRole('button', { name: 'Replace trusted certificate' }))
    await screen.findByLabelText('Username')
    expect(approveFingerprint).toHaveBeenCalledTimes(1)
  })
  it('maps login errors and clears the failed password', async () => {
    await trust()
    fireEvent.click(screen.getByRole('button', { name: 'Log in instead' }))
    fireEvent.change(screen.getByLabelText('Username'), { target: { value: 'friend' } })
    fireEvent.change(screen.getByLabelText('Password'), { target: { value: 'wrong-password' } })
    vi.mocked(connect).mockRejectedValue({ code: 'server_1', message: 'internal detail' })
    fireEvent.click(screen.getByRole('button', { name: 'Log in and join' }))
    expect(await screen.findByRole('alert')).toHaveTextContent('not accepted')
    expect(screen.getByLabelText('Password')).toHaveValue('')
    expect(useAppStore.getState().servers).toHaveLength(0)
  })
  it('rediscovers the server identity if the certificate changes during login', async () => {
    await trust()
    fireEvent.click(screen.getByRole('button', { name: 'Log in instead' }))
    fireEvent.change(screen.getByLabelText('Username'), { target: { value: 'friend' } })
    fireEvent.change(screen.getByLabelText('Password'), { target: { value: 'a password' } })
    const fingerprint = 'cd'.repeat(32)
    vi.mocked(connect).mockRejectedValue({ fingerprint })
    vi.mocked(probeServer).mockResolvedValue({
      ...metadata,
      name: 'New community',
      previousFingerprint: metadata.fingerprint,
      fingerprint,
    })
    fireEvent.click(screen.getByRole('button', { name: 'Log in and join' }))
    await screen.findByRole('heading', { name: 'Server certificate changed' })
    expect(screen.queryByText('Friends')).not.toBeInTheDocument()
    fireEvent.click(screen.getByRole('checkbox', { name: /I compared/ }))
    fireEvent.click(screen.getByRole('button', { name: 'Replace trusted certificate' }))
    expect(await screen.findByText('New community')).toBeVisible()
    expect(screen.getByLabelText('Password')).toHaveValue('')
  })
  it('rejects invalid registration without invoking the bridge', async () => {
    await trust()
    fireEvent.change(screen.getByLabelText('Username'), { target: { value: 'UPPER' } })
    fireEvent.change(screen.getByLabelText('Password'), { target: { value: 'long-enough' } })
    fireEvent.change(screen.getByLabelText('Invite code'), { target: { value: 'invite' } })
    fireEvent.click(screen.getByRole('button', { name: 'Create account and join' }))
    expect(await screen.findByRole('alert')).toHaveTextContent('lowercase')
    expect(connect).not.toHaveBeenCalled()
  })
  it.each([
    ['open', true],
    ['closed', false],
  ])('uses the registration policy advertised as %s', async (registrationMode, register) => {
    vi.mocked(probeServer).mockResolvedValue({
      ...metadata,
      registrationMode: String(registrationMode),
      trusted: true,
    })
    await discover()
    expect(screen.queryByLabelText('Invite code')).toBeNull()
    expect(
      screen.getByRole('button', {
        name: register ? 'Create account and join' : 'Log in and join',
      }),
    ).toBeEnabled()
    if (!register) expect(screen.queryByRole('button', { name: 'Create an account' })).toBeNull()
  })
  it('keeps the username and focuses the rejected invite after clearing the password', async () => {
    await trust()
    fireEvent.change(screen.getByLabelText('Username'), { target: { value: 'friend' } })
    fireEvent.change(screen.getByLabelText('Password'), { target: { value: 'long-enough' } })
    fireEvent.change(screen.getByLabelText('Invite code'), { target: { value: 'old' } })
    vi.mocked(connect).mockRejectedValue({ code: 'server_4', message: 'invite is not valid' })
    fireEvent.click(screen.getByRole('button', { name: 'Create account and join' }))
    await screen.findByRole('alert')
    expect(screen.getByLabelText('Invite code')).toHaveAccessibleDescription(/new code/)
    expect(screen.getByLabelText('Invite code')).toHaveFocus()
    expect(screen.getByLabelText('Password')).toHaveValue('')
    expect(screen.getByLabelText('Username')).toHaveValue('friend')
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
    expect(probeServer).not.toHaveBeenCalled()
    fireEvent.click(screen.getByRole('button', { name: 'Find server' }))
    await waitFor(() => expect(probeServer).toHaveBeenCalled())
    fireEvent.change(screen.getByLabelText('Server address'), { target: { value: '' } })
    await act(async () => finish(metadata))
    expect(screen.queryByRole('checkbox')).not.toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Find server' })).toBeEnabled()
  })
  it('validates addresses, IPv6, ports and password byte limits', () => {
    expect(parseAddress('host')).toEqual({ host: 'host', port: 443, httpsPort: 443 })
    expect(parseAddress('ss://[::1]:9000', '9443')).toEqual({
      host: '::1',
      port: 9000,
      httpsPort: 9443,
    })
    for (const value of ['https://host', 'host:0', 'host:65536', 'host/path', 'user@host', '::1'])
      expect(() => parseAddress(value)).toThrow()
    expect(accountErrors('friend', 'é'.repeat(65), true, false, '').password).toContain('128 bytes')
  })
  it('keeps keyboard focus within the dialog and focuses each new step', async () => {
    const geometry = vi
      .spyOn(HTMLElement.prototype, 'getClientRects')
      .mockReturnValue({ length: 1 } as DOMRectList)
    try {
      await discover()
      expect(screen.getByRole('heading', { name: 'Check the server fingerprint' })).toHaveFocus()
      fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Tab', shiftKey: true })
      expect(screen.getByRole('button', { name: 'Go back' })).toHaveFocus()
      fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Tab' })
      expect(screen.getByRole('button', { name: 'Close' })).toHaveFocus()
      fireEvent.click(screen.getByRole('checkbox', { name: /I compared/ }))
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
      expect(screen.getByRole('button', { name: 'Find server' })).toHaveFocus()
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

it('uses domain validation without asking for a fingerprint', async () => {
  vi.mocked(probeServer).mockResolvedValue({ ...metadata, publicCertificate: true, trusted: true })
  await discover()
  expect(screen.getByText(/Verified domain certificate/)).toBeInTheDocument()
  expect(screen.getByLabelText('Password')).toBeInTheDocument()
  expect(approveFingerprint).not.toHaveBeenCalled()
})

it('explicitly switches a legacy pin only through the validated domain action', async () => {
  vi.mocked(enablePublicCertificate).mockResolvedValue({
    ...metadata,
    publicCertificate: true,
    trusted: true,
  })
  render(<ConnectDialog />)
  fireEvent.change(screen.getByLabelText('Server address'), {
    target: { value: 'friends.example:443' },
  })
  fireEvent.click(screen.getByText('Advanced connection options'))
  fireEvent.click(screen.getByRole('button', { name: 'Use domain certificate validation' }))
  await screen.findByText(/Verified domain certificate/)
  expect(enablePublicCertificate).toHaveBeenCalledWith({
    host: 'friends.example',
    port: 443,
    httpsPort: 443,
  })
  expect(approveFingerprint).not.toHaveBeenCalled()
  expect(connect).not.toHaveBeenCalled()
})

it('keeps a failed domain switch out of the login step', async () => {
  vi.mocked(enablePublicCertificate).mockRejectedValue({ message: 'Certificate validation failed' })
  render(<ConnectDialog />)
  fireEvent.change(screen.getByLabelText('Server address'), {
    target: { value: 'friends.example:443' },
  })
  fireEvent.click(screen.getByText('Advanced connection options'))
  fireEvent.click(screen.getByRole('button', { name: 'Use domain certificate validation' }))
  await screen.findByText('Certificate validation failed')
  expect(screen.queryByLabelText('Password')).not.toBeInTheDocument()
  fireEvent.click(screen.getByRole('button', { name: 'Retry domain validation' }))
  await screen.findByText('Certificate validation failed')
  expect(enablePublicCertificate).toHaveBeenCalledTimes(2)
  expect(probeServer).not.toHaveBeenCalled()
  expect(approveFingerprint).not.toHaveBeenCalled()
  expect(connect).not.toHaveBeenCalled()
})
