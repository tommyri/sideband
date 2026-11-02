import '@testing-library/jest-dom/vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { defaults, migratePreferences, useAudioStore } from './store'
import { AudioSettings } from './AudioSettings'
import { invokeBridge } from '../../../lib/tauri'
import { useVoiceStore } from '../../voice/store'
vi.mock('../../../lib/tauri', () => ({ invokeBridge: vi.fn() }))
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }))
const env = {
  preferences: defaults,
  devices: [{ id: 'mic', name: 'USB microphone', input: true, output: false }],
  microphone: 'Granted',
  apple: true,
  sonora: false,
  rnnoise: true,
  mouse: false,
  systemBinding: false,
  permission: 'Input Monitoring',
  limitations: [],
}
beforeEach(() => {
  HTMLDialogElement.prototype.showModal = function () {
    this.open = true
  }
  HTMLDialogElement.prototype.close = function () {
    this.open = false
  }
  useVoiceStore.setState({ active: null })
  useAudioStore.setState({ preferences: defaults })
  vi.mocked(invokeBridge).mockImplementation(async (command) =>
    command === 'audio_environment' ? env : undefined,
  )
})
afterEach(() => {
  cleanup()
  vi.clearAllMocks()
})
describe('audio preferences', () => {
  it('migrates old records without enabling processing or open mic', () => {
    expect(migratePreferences({ version: 0, master: 75 })).toEqual({ ...defaults, master: 75 })
    expect(
      migratePreferences({
        mode: 'garbage',
        threshold: NaN,
        master: 999,
        jitterMin: 190,
        jitterMax: 30,
      }),
    ).toEqual(defaults)
    expect(migratePreferences(null)).toEqual(defaults)
    expect(() => migratePreferences({ version: 9 })).toThrow('newer')
  })
  it('offers only supported models and applies native settings', async () => {
    render(<AudioSettings close={() => {}} />)
    await screen.findByRole('option', { name: 'USB microphone' })
    expect(screen.queryByRole('option', { name: /WebRTC/ })).not.toBeInTheDocument()
    expect(screen.getByLabelText(/Echo cancellation/)).toBeDisabled()
    fireEvent.change(screen.getByLabelText('Microphone'), { target: { value: 'mic' } })
    fireEvent.change(screen.getByLabelText('Talking mode'), { target: { value: 'vad' } })
    fireEvent.change(screen.getByLabelText('Activation threshold (dBFS)'), {
      target: { value: -35 },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Apply settings' }))
    await screen.findByText('Applied and saved.')
    expect(invokeBridge).toHaveBeenCalledWith('save_audio', {
      preferences: { ...defaults, input: 'mic', mode: 'vad', threshold: -35 },
    })
  })
  it('reports save failures without publishing unsaved preferences', async () => {
    render(<AudioSettings close={() => {}} />)
    await screen.findByRole('button', { name: 'Apply settings' })
    vi.mocked(invokeBridge).mockRejectedValueOnce({ message: 'Device disappeared' })
    fireEvent.change(screen.getByLabelText('Master volume (%)'), { target: { value: 50 } })
    fireEvent.click(screen.getByRole('button', { name: 'Apply settings' }))
    await screen.findByRole('alert')
    expect(screen.getByRole('alert')).toHaveTextContent('Device disappeared')
    expect(useAudioStore.getState().preferences.master).toBe(100)
  })
  it('Apple mode resets incompatible software stages and device selection', async () => {
    render(<AudioSettings close={() => {}} />)
    await screen.findByLabelText('Microphone')
    fireEvent.change(screen.getByLabelText('Microphone'), { target: { value: 'mic' } })
    fireEvent.click(screen.getByLabelText('Use Apple voice processing'))
    expect(screen.getByLabelText('Microphone')).toBeDisabled()
    expect(screen.getByLabelText('Microphone')).toHaveValue('')
    expect(screen.getByLabelText('Noise suppression')).toBeDisabled()
  })
  it('focuses binding capture and records a keyboard gesture', async () => {
    render(<AudioSettings close={() => {}} />)
    const button = await screen.findByRole('button', { name: 'Capture in-app binding' })
    fireEvent.click(button)
    expect(button).toHaveFocus()
    fireEvent.keyDown(button, { key: 'v', code: 'KeyV' })
    expect(button).toHaveTextContent('KeyV')
    fireEvent.click(screen.getByRole('button', { name: 'Apply settings' }))
    await screen.findByText('Applied and saved.')
    expect(useAudioStore.getState().preferences.binding).toBe('KeyV')
  })
  it('does not offer capture when the compositor owns the binding', async () => {
    vi.mocked(invokeBridge).mockResolvedValue({ ...env, systemBinding: true })
    render(<AudioSettings close={() => {}} />)
    expect(await screen.findByRole('button', { name: 'Capture in-app binding' })).toBeDisabled()
  })
  it('calibration uses local samples and changes the draft until Apply', async () => {
    render(<AudioSettings close={() => {}} />)
    await screen.findByLabelText('Talking mode')
    fireEvent.change(screen.getByLabelText('Talking mode'), { target: { value: 'vad' } })
    vi.mocked(invokeBridge).mockResolvedValueOnce(-28)
    fireEvent.click(screen.getByRole('button', { name: /Auto-calibrate/ }))
    await waitFor(() =>
      expect(screen.getByLabelText('Activation threshold (dBFS)')).toHaveValue('-28'),
    )
    expect(useAudioStore.getState().preferences.threshold).toBe(-40)
  })
})
