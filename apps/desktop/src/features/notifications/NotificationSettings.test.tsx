import '@testing-library/jest-dom/vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { NotificationSettings } from './NotificationSettings'
import { defaults, notificationLevel } from './model'
import { invokeBridge } from '../../lib/tauri'
vi.mock('../../lib/tauri', () => ({ invokeBridge: vi.fn() }))
beforeEach(() => {
  HTMLDialogElement.prototype.showModal = function () {
    this.open = true
  }
  vi.mocked(invokeBridge).mockImplementation(async (command) =>
    command === 'notification_preferences' ? structuredClone(defaults) : undefined,
  )
})
afterEach(() => {
  cleanup()
  vi.clearAllMocks()
})
it('resolves channel defaults with DND and server mute taking precedence', () => {
  expect(notificationLevel(defaults, 's', 'c')).toBe('mentions')
  const p = { ...defaults, channels: { 's/c': 'all' as const } }
  expect(notificationLevel(p, 's', 'c')).toBe('all')
  expect(notificationLevel({ ...p, dnd: true }, 's', 'c')).toBe('none')
  expect(notificationLevel({ ...p, mutedServers: ['s'] }, 's', 'c')).toBe('none')
  expect(notificationLevel({ ...p, mutedServers: ['other'] }, 's', 'c')).toBe('all')
})
it('does not request permissions or send notifications on opening; saves only explicitly', async () => {
  render(<NotificationSettings close={() => {}} />)
  await waitFor(() => expect(screen.getByRole('button', { name: 'Save settings' })).toBeEnabled())
  expect(invokeBridge).toHaveBeenCalledTimes(1)
  expect(screen.getByLabelText('Show message text in desktop notifications')).not.toBeChecked()
  fireEvent.click(screen.getByLabelText('Do not disturb'))
  fireEvent.click(screen.getByRole('button', { name: 'Save settings' }))
  await screen.findByText('Notification settings saved.')
  expect(invokeBridge).toHaveBeenCalledWith('save_notification_preferences', {
    preferences: { ...defaults, dnd: true },
  })
  expect(invokeBridge).not.toHaveBeenCalledWith('test_notification')
})
it('persists explicit opt-in before sending a test and surfaces errors', async () => {
  render(<NotificationSettings close={() => {}} />)
  await waitFor(() => expect(screen.getByRole('button', { name: 'Save settings' })).toBeEnabled())
  fireEvent.click(screen.getByLabelText('Desktop notifications when Sideband is in the background'))
  vi.mocked(invokeBridge).mockImplementation(async (command) => {
    if (command === 'test_notification') throw new Error('OS notification failure')
  })
  fireEvent.click(screen.getByRole('button', { name: 'Save and send test notification' }))
  await screen.findByRole('alert')
  expect(screen.getByRole('alert')).toHaveTextContent('OS notification failure')
  expect(invokeBridge).toHaveBeenCalledWith('save_notification_preferences', {
    preferences: { ...defaults, desktop: true },
  })
})
