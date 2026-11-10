import { afterEach, expect, test, vi } from 'vitest'
import { invokeBridge } from '../../lib/tauri'
import { deviceRouteText, voiceControl } from './api'

vi.mock('../../lib/tauri', () => ({ invokeBridge: vi.fn() }))
vi.mock('../testing/report', () => ({ record: vi.fn() }))

afterEach(() => vi.mocked(invokeBridge).mockReset())

test('a fast PTT tap sends release only after press settled', async () => {
  let finishPress = () => {}
  vi.mocked(invokeBridge).mockImplementationOnce(
    () => new Promise<undefined>((resolve) => (finishPress = () => resolve(undefined))),
  )
  vi.mocked(invokeBridge).mockResolvedValue(undefined)
  const press = voiceControl('ptt', true)
  const release = voiceControl('ptt', false)
  await vi.waitFor(() => expect(invokeBridge).toHaveBeenCalledTimes(1))
  await new Promise((resolve) => setTimeout(resolve, 10))
  expect(invokeBridge).toHaveBeenCalledTimes(1)
  expect(invokeBridge).toHaveBeenLastCalledWith('voice_control', { action: 'ptt', enabled: true })
  finishPress()
  await press
  await release
  expect(invokeBridge).toHaveBeenCalledTimes(2)
  expect(invokeBridge).toHaveBeenLastCalledWith('voice_control', { action: 'ptt', enabled: false })
})

test('a failed press does not block the release', async () => {
  vi.mocked(invokeBridge).mockRejectedValueOnce(new Error('gate'))
  vi.mocked(invokeBridge).mockResolvedValue(undefined)
  await expect(voiceControl('ptt', true)).rejects.toThrow('gate')
  await voiceControl('ptt', false)
  expect(invokeBridge).toHaveBeenLastCalledWith('voice_control', { action: 'ptt', enabled: false })
})

test('mute is not queued behind a pending PTT change', async () => {
  let finishPress = () => {}
  vi.mocked(invokeBridge).mockImplementationOnce(
    () => new Promise<undefined>((resolve) => (finishPress = () => resolve(undefined))),
  )
  vi.mocked(invokeBridge).mockResolvedValue(undefined)
  const press = voiceControl('ptt', true)
  await vi.waitFor(() => expect(invokeBridge).toHaveBeenCalledTimes(1))
  await voiceControl('mute', true)
  expect(invokeBridge).toHaveBeenLastCalledWith('voice_control', { action: 'mute', enabled: true })
  finishPress()
  await press
})

test('the device indicator names what is missing or on the fallback and clears on recovery', () => {
  expect(deviceRouteText('wanted', 'wanted')).toBe('')
  expect(deviceRouteText('missing', 'wanted')).toBe('Microphone disconnected. Reconnecting…')
  expect(deviceRouteText('wanted', 'fallback')).toBe(
    'Audio output unavailable; using the system default.',
  )
  expect(deviceRouteText('fallback', 'missing')).toBe(
    'Microphone unavailable; using the system default. Audio output disconnected. Reconnecting…',
  )
  expect(deviceRouteText('missing', 'missing')).toBe('Audio devices disconnected. Reconnecting…')
})
