import '@testing-library/jest-dom/vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { defaults, environment, migratePreferences, save, useAudioStore } from './store'
import { AudioSettings } from './AudioSettings'
import { invokeBridge } from '../../../lib/tauri'
import { useVoiceStore } from '../../voice/store'
import { ServerHome } from '../../onboarding/ServerHome'
import { useAppStore } from '../../../store/app-store'
vi.mock('../../../lib/tauri', () => ({ invokeBridge: vi.fn() }))
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }))
const env = {
  preferences: defaults,
  devices: [
    { id: 'mic', name: 'USB microphone', input: true, output: false, inputChannels: 2 },
    { id: 'desk', name: 'Desk interface', input: true, output: true, inputChannels: 8 },
    { id: 'headset', name: 'Headset', input: true, output: false, inputChannels: 1 },
    { id: 'virtual', name: 'Virtual cable', input: true, output: false, inputChannels: null },
  ],
  defaultInput: 'mic',
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
async function expand(label = 'More audio controls') {
  fireEvent.click(await screen.findByText(label, { selector: 'summary' }))
}

describe('audio preferences', () => {
  it.each([
    ['Pick your microphone and output', 'Microphone'],
    ['Choose a push-to-talk key or voice activation', 'Talking mode'],
  ])('opens the control named by the setup link "%s" after devices load', async (link, control) => {
    useAppStore.setState({
      servers: [
        {
          id: 'a',
          label: 'Friends',
          host: 'localhost',
          port: 443,
          httpsPort: 443,
          unread: 0,
          status: { kind: 'disconnected' },
        },
      ],
      selectedServerId: 'a',
    })
    let finishLoading!: (value: typeof env) => void
    vi.mocked(invokeBridge).mockReturnValueOnce(
      new Promise((resolve) => {
        finishLoading = resolve
      }),
    )
    render(<ServerHome />)
    const opener = screen.getByRole('button', { name: link })
    fireEvent.click(opener)
    expect(screen.getByText('Loading audio devices…')).toBeInTheDocument()
    finishLoading(env)
    expect(await screen.findByRole('combobox', { name: control })).toHaveFocus()
    await expand()
    const refresh = screen.getByRole('button', { name: 'Refresh devices' })
    refresh.focus()
    fireEvent.click(refresh)
    await screen.findByText('Device list refreshed.')
    expect(refresh).toHaveFocus()
    fireEvent.click(screen.getByRole('button', { name: 'Done' }))
    expect(opener).toHaveFocus()
  })
  it('focuses the setting that controls the system microphone when Apple processing locks device choices', async () => {
    vi.mocked(invokeBridge).mockResolvedValue({ ...env, preferences: { ...defaults, apple: true } })
    render(<AudioSettings initialFocus="microphone" close={() => {}} />)
    expect(await screen.findByLabelText('Microphone')).toBeDisabled()
    expect(screen.getByLabelText('Use Apple voice processing')).toHaveFocus()
  })
  it('starts a local mic check on request and treats an intentional stop as completion', async () => {
    let stop!: () => void
    vi.mocked(invokeBridge).mockImplementation(async (command) => {
      if (command === 'audio_environment') return env
      if (command === 'test_audio')
        return new Promise((_, reject) => {
          stop = () => reject({ message: 'Audio test stopped' })
        })
      if (command === 'stop_audio_test') stop()
    })
    const { unmount } = render(<AudioSettings close={() => {}} />)
    const check = await screen.findByRole('button', { name: 'Check mic' })
    expect(screen.getByText('Ready')).toBeVisible()
    expect(invokeBridge).not.toHaveBeenCalledWith('test_audio', expect.anything())
    expect(invokeBridge).not.toHaveBeenCalledWith('audio_level')
    fireEvent.click(check)
    expect(invokeBridge).toHaveBeenCalledWith('test_audio', { preferences: defaults, kind: 'mic' })
    fireEvent.click(screen.getByRole('button', { name: 'Stop test' }))
    expect(await screen.findByText('Local audio test stopped.')).toBeVisible()
    expect(screen.queryByRole('alert')).not.toBeInTheDocument()
    unmount()
    expect(
      vi.mocked(invokeBridge).mock.calls.filter(([cmd]) => cmd === 'stop_audio_test'),
    ).toHaveLength(1)
  })
  it('stops an unfinished local mic check when the sheet closes', async () => {
    vi.mocked(invokeBridge).mockImplementation(async (command) => {
      if (command === 'audio_environment') return env
      if (command === 'test_audio') return new Promise(() => {})
    })
    const { unmount } = render(<AudioSettings close={() => {}} />)
    fireEvent.click(await screen.findByRole('button', { name: 'Check mic' }))
    unmount()
    expect(invokeBridge).toHaveBeenCalledWith('stop_audio_test')
  })
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
    expect(migratePreferences({ version: 1, inputChannel: 3 }).inputChannel).toBe(3)
    expect(migratePreferences({ version: 1, inputChannel: 0 }).inputChannel).toBe(0)
    for (const inputChannel of [65, -1, 1.5, NaN])
      expect(migratePreferences({ version: 1, inputChannel }).inputChannel).toBe(1)
  })
  it('takes the first input by default and saves the chosen one', async () => {
    render(<AudioSettings close={() => {}} />)
    await expand()
    const select = await screen.findByLabelText('Microphone input')
    expect(select).toHaveValue('1')
    fireEvent.change(select, { target: { value: '2' } })
    fireEvent.click(screen.getByRole('button', { name: 'Apply settings' }))
    await screen.findByText('Applied and saved.')
    expect(invokeBridge).toHaveBeenCalledWith('save_audio', {
      preferences: { ...defaults, inputChannel: 2 },
    })
    fireEvent.change(select, { target: { value: '0' } })
    expect(select).toHaveDisplayValue('Average of all inputs')
  })
  it('offers only the inputs the device has', async () => {
    render(<AudioSettings close={() => {}} />)
    await expand()
    const select = await screen.findByLabelText('Microphone input')
    const offered = () =>
      Array.from((select as HTMLSelectElement).options, (option) => option.textContent)
    // The system default is the two-input USB microphone.
    expect(offered()).toEqual(['Input 1', 'Input 2', 'Average of all inputs'])
    fireEvent.change(screen.getByLabelText('Microphone'), { target: { value: 'mic' } })
    expect(offered()).toEqual(['Input 1', 'Input 2', 'Average of all inputs'])
    fireEvent.change(screen.getByLabelText('Microphone'), { target: { value: 'desk' } })
    expect(offered()).toEqual([
      ...Array.from({ length: 8 }, (_, i) => `Input ${i + 1}`),
      'Average of all inputs',
    ])
    // Nothing to average on a mono device.
    fireEvent.change(screen.getByLabelText('Microphone'), { target: { value: 'headset' } })
    expect(offered()).toEqual(['Input 1'])
    // A host that does not say keeps the full list.
    fireEvent.change(screen.getByLabelText('Microphone'), { target: { value: 'virtual' } })
    expect(offered()).toHaveLength(17)
  })
  it('shows input 1 for a saved input the device does not have, and keeps it', async () => {
    vi.mocked(invokeBridge).mockImplementation(async (command) =>
      command === 'audio_environment'
        ? { ...env, preferences: { ...defaults, input: 'desk', inputChannel: 5 } }
        : undefined,
    )
    render(<AudioSettings close={() => {}} />)
    await expand()
    const select = await screen.findByLabelText('Microphone input')
    expect(select).toHaveValue('5')
    fireEvent.change(screen.getByLabelText('Microphone'), { target: { value: 'mic' } })
    expect(select).toHaveValue('1')
    expect(screen.getByText(/has no Input 5; it uses Input 1/)).toBeInTheDocument()
    // Saved as chosen, so it comes back with the eight-input interface.
    fireEvent.click(screen.getByRole('button', { name: 'Apply settings' }))
    await screen.findByText('Applied and saved.')
    expect(invokeBridge).toHaveBeenCalledWith('save_audio', {
      preferences: { ...defaults, input: 'mic', inputChannel: 5 },
    })
  })
  it('falls back from Average on a mono device', async () => {
    vi.mocked(invokeBridge).mockImplementation(async (command) =>
      command === 'audio_environment'
        ? { ...env, preferences: { ...defaults, input: 'headset', inputChannel: 0 } }
        : undefined,
    )
    render(<AudioSettings close={() => {}} />)
    await expand()
    expect(await screen.findByLabelText('Microphone input')).toHaveValue('1')
    expect(screen.getByText(/one input; it uses Input 1/)).toBeInTheDocument()
  })
  it('offers only supported models and applies native settings', async () => {
    render(<AudioSettings close={() => {}} />)
    await expand('Advanced processing')
    await expand()
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
    await expand()
    vi.mocked(invokeBridge).mockRejectedValueOnce({ message: 'Device disappeared' })
    fireEvent.change(screen.getByLabelText('Master volume (%)'), { target: { value: 50 } })
    fireEvent.click(screen.getByRole('button', { name: 'Apply settings' }))
    await screen.findByRole('alert')
    expect(screen.getByRole('alert')).toHaveTextContent('Device disappeared')
    expect(useAudioStore.getState().preferences.master).toBe(100)
  })
  it('keeps the applied talking mode when an older device request finishes late', async () => {
    let finishLoading!: (value: typeof env) => void
    vi.mocked(invokeBridge).mockReturnValueOnce(
      new Promise((resolve) => {
        finishLoading = resolve
      }),
    )
    const earlier = environment()
    await save({ ...defaults, mode: 'open' })
    finishLoading(env)
    expect((await earlier).preferences.mode).toBe('open')
    expect(useAudioStore.getState().preferences.mode).toBe('open')
  })
  it('Apple mode resets incompatible software stages and device selection', async () => {
    render(<AudioSettings close={() => {}} />)
    await screen.findByLabelText('Microphone')
    fireEvent.change(screen.getByLabelText('Microphone'), { target: { value: 'mic' } })
    await expand('Advanced processing')
    fireEvent.click(screen.getByLabelText('Use Apple voice processing'))
    expect(screen.getByLabelText('Microphone')).toBeDisabled()
    expect(screen.getByLabelText('Microphone')).toHaveValue('')
    expect(screen.getByLabelText('Microphone input')).toBeDisabled()
    expect(screen.getByLabelText('Noise suppression')).toBeDisabled()
  })
  it('focuses binding capture and records a keyboard gesture', async () => {
    render(<AudioSettings close={() => {}} />)
    await expand()
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
    await expand()
    expect(await screen.findByRole('button', { name: 'Capture in-app binding' })).toBeDisabled()
  })
  it('calibration uses local samples and changes the draft until Apply', async () => {
    render(<AudioSettings close={() => {}} />)
    await screen.findByLabelText('Talking mode')
    fireEvent.change(screen.getByLabelText('Talking mode'), { target: { value: 'vad' } })
    await expand()
    vi.mocked(invokeBridge).mockResolvedValueOnce(-28)
    fireEvent.click(screen.getByRole('button', { name: /Auto-calibrate/ }))
    await waitFor(() =>
      expect(screen.getByLabelText('Activation threshold (dBFS)')).toHaveValue('-28'),
    )
    expect(useAudioStore.getState().preferences.threshold).toBe(-40)
  })
})
