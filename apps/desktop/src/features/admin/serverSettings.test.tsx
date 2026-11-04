import '@testing-library/jest-dom/vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { ServerSettings } from './ServerSettings'
import { validServerSettings } from './serverSettingsModel'
import { invokeBridge } from '../../lib/tauri'
import type { ServerSettingsView } from '../../lib/generated/ServerSettingsView'
vi.mock('../../lib/tauri', () => ({ invokeBridge: vi.fn() }))
const original: ServerSettingsView = {
  fields: {
    name: 'Friends',
    description: 'Hello',
    iconId: null,
    registrationMode: 'invite',
    revision: 'revision-1',
  },
  version: '0.1.0',
  fingerprint: 'AB:CD:EF',
  icon: null,
  iconWarning: null,
}
let current: ServerSettingsView
beforeEach(() => {
  current = structuredClone(original)
  HTMLDialogElement.prototype.showModal = function () {
    this.open = true
  }
  HTMLDialogElement.prototype.close = function () {
    this.open = false
  }
  vi.mocked(invokeBridge)
    .mockReset()
    .mockImplementation(async (command, args) => {
      if (command === 'get_server_settings') return structuredClone(current)
      if (command === 'upload_server_icon') return 'uploaded-icon'
      if (command === 'save_server_settings') {
        current = {
          ...current,
          fields: {
            ...(args as { fields: ServerSettingsView['fields'] }).fields,
            revision: 'revision-2',
          },
        }
        return structuredClone(current)
      }
    })
})
afterEach(cleanup)
const mount = (close = vi.fn()) =>
  render(<ServerSettings server="server" connected permitted close={close} />)
it('validates character counts and only supported registration policies', () => {
  expect(validServerSettings({ ...original.fields, name: '😀'.repeat(100) })).toBe('')
  expect(validServerSettings({ ...original.fields, name: ' ' })).toContain('Name')
  expect(validServerSettings({ ...original.fields, name: '😀'.repeat(101) })).toContain('Name')
  expect(validServerSettings({ ...original.fields, description: 'x'.repeat(2049) })).toContain(
    'Description',
  )
  expect(validServerSettings({ ...original.fields, registrationMode: 'open' })).toContain(
    'registration',
  )
})
it('loads identity, saves all fields with the revision and explains closed registration', async () => {
  mount()
  await screen.findByDisplayValue('Friends')
  expect(screen.getByText('AB:CD:EF')).toBeInTheDocument()
  expect(screen.getByRole('button', { name: 'Save settings' })).toBeDisabled()
  fireEvent.change(screen.getByLabelText('Server name'), { target: { value: ' New name ' } })
  fireEvent.change(screen.getByLabelText(/^Description/), { target: { value: '' } })
  fireEvent.change(screen.getByLabelText(/^Registration/), { target: { value: 'closed' } })
  expect(screen.getByText(/Existing members can still sign in/)).toBeInTheDocument()
  fireEvent.click(screen.getByRole('button', { name: 'Save settings' }))
  await screen.findByText('Server settings saved.')
  expect(invokeBridge).toHaveBeenCalledWith('save_server_settings', {
    serverId: 'server',
    fields: { ...original.fields, name: 'New name', description: '', registrationMode: 'closed' },
  })
  expect(screen.getByRole('button', { name: 'Save settings' })).toBeDisabled()
})
it('preserves denied and conflicting drafts and explicitly discards them on reload', async () => {
  mount()
  await screen.findByDisplayValue('Friends')
  fireEvent.change(screen.getByLabelText('Server name'), { target: { value: 'Draft' } })
  vi.mocked(invokeBridge).mockImplementation(async (command) => {
    if (command === 'save_server_settings')
      throw new Error('Settings changed. Reload before saving.')
    return structuredClone({ ...current, fields: { ...current.fields, name: 'Remote edit' } })
  })
  fireEvent.click(screen.getByRole('button', { name: 'Save settings' }))
  await screen.findByText('Settings changed. Reload before saving.')
  expect(screen.getByLabelText('Server name')).toHaveValue('Draft')
  fireEvent.click(screen.getByRole('button', { name: 'Reload settings' }))
  expect(screen.getByRole('alertdialog')).toBeInTheDocument()
  fireEvent.click(screen.getByRole('button', { name: 'Keep editing' }))
  expect(screen.getByLabelText('Server name')).toHaveValue('Draft')
  fireEvent.click(screen.getByRole('button', { name: 'Reload settings' }))
  fireEvent.click(screen.getByRole('button', { name: 'Discard changes' }))
  await screen.findByDisplayValue('Remote edit')
})
it('requires discarding a dirty draft before closing', async () => {
  const close = vi.fn()
  mount(close)
  await screen.findByDisplayValue('Friends')
  fireEvent.change(screen.getByLabelText('Server name'), { target: { value: 'Unsaved' } })
  fireEvent.click(screen.getByRole('button', { name: 'Close' }))
  expect(close).not.toHaveBeenCalled()
  fireEvent.click(screen.getByRole('button', { name: 'Discard changes' }))
  expect(close).toHaveBeenCalledOnce()
})
it('uploads the selected raster only on save and keeps its identifier after a failed save', async () => {
  mount()
  await screen.findByDisplayValue('Friends')
  fireEvent.change(screen.getByLabelText(/^Choose icon/), {
    target: { files: [new File(['png'], 'icon.png', { type: 'image/png' })] },
  })
  await screen.findByAltText('Server icon preview')
  expect(
    vi.mocked(invokeBridge).mock.calls.some(([command]) => command === 'upload_server_icon'),
  ).toBe(false)
  vi.mocked(invokeBridge).mockImplementation(async (command) => {
    if (command === 'upload_server_icon') return 'uploaded-icon'
    if (command === 'save_server_settings') throw new Error('Permission denied')
    return structuredClone(current)
  })
  fireEvent.click(screen.getByRole('button', { name: 'Save settings' }))
  await screen.findByText('Permission denied')
  expect(invokeBridge).toHaveBeenCalledWith('save_server_settings', {
    serverId: 'server',
    fields: { ...original.fields, iconId: 'uploaded-icon' },
  })
  fireEvent.click(screen.getByRole('button', { name: 'Save settings' }))
  await waitFor(() =>
    expect(
      vi.mocked(invokeBridge).mock.calls.filter(([c]) => c === 'save_server_settings'),
    ).toHaveLength(2),
  )
  expect(
    vi.mocked(invokeBridge).mock.calls.filter(([c]) => c === 'upload_server_icon'),
  ).toHaveLength(1)
})
it('rejects unsupported icon files and allows removing an existing icon', async () => {
  current = {
    ...current,
    fields: { ...current.fields, iconId: 'existing' },
    icon: 'data:image/png;base64,AAAA',
  }
  mount()
  await screen.findByDisplayValue('Friends')
  fireEvent.change(screen.getByLabelText(/^Choose icon/), {
    target: { files: [new File(['svg'], 'icon.svg', { type: 'image/svg+xml' })] },
  })
  await screen.findByText('Choose a PNG, JPEG or WebP image up to 2 MiB.')
  fireEvent.click(screen.getByRole('button', { name: 'Remove icon' }))
  fireEvent.click(screen.getByRole('button', { name: 'Save settings' }))
  await screen.findByText('Server settings saved.')
  expect(current.fields.iconId).toBeNull()
})
it('discards editor state on disconnect and hides forms without permission', async () => {
  const ui = mount()
  await screen.findByDisplayValue('Friends')
  ui.rerender(<ServerSettings server="server" connected={false} permitted close={() => {}} />)
  expect(screen.queryByLabelText('Server name')).not.toBeInTheDocument()
  expect(screen.getByRole('alert')).toHaveTextContent('Connect with Manage server')
})
