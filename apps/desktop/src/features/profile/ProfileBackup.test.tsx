import '@testing-library/jest-dom/vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { invokeBridge } from '../../lib/tauri'
import { ProfileBackup } from './ProfileBackup'

vi.mock('../../lib/tauri', () => ({ invokeBridge: vi.fn() }))
beforeEach(() => {
  vi.resetAllMocks()
})
afterEach(cleanup)

it('keeps saving the file, copying the words and confirming them separate', async () => {
  const words = 'example testing words'
  vi.mocked(invokeBridge)
    .mockResolvedValueOnce(null)
    .mockResolvedValueOnce({ filename: 'Sam.sideband-backup', words })
  const writeText = vi.fn().mockResolvedValue(undefined)
  Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } })
  render(<ProfileBackup />)
  expect(screen.getByText(/iCloud Drive or Google Drive/)).toBeVisible()
  fireEvent.click(screen.getByText('Other places to keep the file'))
  expect(screen.getByText(/store the words on paper/)).toBeVisible()
  fireEvent.click(screen.getByRole('button', { name: 'Save backup file' }))
  await waitFor(() =>
    expect(screen.getByRole('button', { name: 'Save backup file' })).toBeEnabled(),
  )
  expect(screen.queryByRole('button', { name: 'Copy words' })).toBeNull()
  fireEvent.click(screen.getByRole('button', { name: 'Save backup file' }))
  fireEvent.click(await screen.findByRole('button', { name: 'Copy words' }))
  expect(writeText).toHaveBeenCalledWith(words)
  expect(await screen.findByRole('status')).toHaveTextContent('Words copied')
  expect(screen.getByText(words)).toBeVisible()
  fireEvent.click(screen.getByRole('button', { name: 'I saved my key' }))
  expect(screen.getByRole('heading', { name: 'Your backup is ready' })).toBeVisible()
  expect(screen.queryByText(words)).toBeNull()
})

it('restores using the selected file and its words and allows a retry with another key', async () => {
  vi.mocked(invokeBridge)
    .mockResolvedValueOnce({ filename: 'Sam.sideband-backup', id: 'ref' })
    .mockRejectedValueOnce({ message: 'These words didn’t open this file.' })
    .mockResolvedValueOnce('Sam')
  const done = vi.fn()
  render(<ProfileBackup initialMode="restore" onDone={done} />)
  const submit = screen.getByRole('button', { name: 'Restore profile' })
  expect(submit).toBeDisabled()
  fireEvent.click(screen.getByText('Missing your file or key?'))
  expect(screen.getByText('Missing the file?')).toBeVisible()
  expect(screen.getByText('Missing the words?')).toBeVisible()
  fireEvent.click(screen.getByRole('button', { name: 'Choose backup file' }))
  expect(await screen.findByText('Sam.sideband-backup')).toBeVisible()
  expect(submit).toBeDisabled()
  const field = screen.getByLabelText('Recovery key')
  fireEvent.change(field, { target: { value: 'wrong words' } })
  fireEvent.click(submit)
  expect(await screen.findByRole('alert')).toHaveTextContent('didn’t open')
  expect(screen.getByText('Sam.sideband-backup')).toBeVisible()
  fireEvent.change(field, { target: { value: 'correct words' } })
  fireEvent.click(submit)
  expect(await screen.findByRole('heading', { name: 'Profile restored' })).toBeVisible()
  expect(done).not.toHaveBeenCalled()
  fireEvent.click(screen.getByRole('button', { name: 'Continue' }))
  expect(done).toHaveBeenCalledWith('Sam')
  expect(invokeBridge).toHaveBeenLastCalledWith('restore_profile_backup', {
    id: 'ref',
    words: 'correct words',
  })
})
