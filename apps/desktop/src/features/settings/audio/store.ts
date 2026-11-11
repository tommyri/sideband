import { create } from 'zustand'
import { listen } from '@tauri-apps/api/event'
import { invokeBridge } from '../../../lib/tauri'

import type { AudioPreferences } from '../../../lib/generated/AudioPreferences'
export type { AudioPreferences } from '../../../lib/generated/AudioPreferences'
export const defaults: AudioPreferences = {
  version: 1,
  input: null,
  inputChannel: 1,
  output: null,
  mode: 'ptt',
  binding: '',
  releaseMs: 150,
  threshold: -40,
  hangoverMs: 300,
  echo: false,
  noise: 'off',
  gain: false,
  apple: false,
  master: 100,
  cues: 100,
  attenuation: 0,
  jitterMin: 20,
  jitterMax: 200,
}
// Missing v0 fields inherit safe defaults; malformed/future records never enable a microphone.
export function migratePreferences(value: unknown): AudioPreferences {
  if (!value || typeof value !== 'object') return { ...defaults }
  const data = value as Partial<AudioPreferences>
  if (data.version !== undefined && data.version !== 0 && data.version !== 1)
    throw new Error('Audio settings are from a newer app version.')
  const result = { ...defaults }
  for (const key of Object.keys(defaults) as (keyof AudioPreferences)[]) {
    const v = data[key]
    if (
      typeof v === typeof defaults[key] ||
      ((key === 'input' || key === 'output') && typeof v === 'string')
    )
      Object.assign(result, { [key]: v })
  }
  result.version = 1
  const ranges = {
    inputChannel: [0, 64],
    releaseMs: [0, 2000],
    threshold: [-96, 0],
    hangoverMs: [0, 5000],
    master: [0, 200],
    cues: [0, 100],
    attenuation: [0, 50],
    jitterMin: [20, 200],
    jitterMax: [20, 200],
  } as const
  for (const key of Object.keys(ranges) as (keyof typeof ranges)[]) {
    const [min, max] = ranges[key]
    const value = result[key]
    const whole = key !== 'inputChannel' || Number.isInteger(value)
    if (!Number.isFinite(value) || value < min || value > max || !whole) result[key] = defaults[key]
  }
  if (!['ptt', 'vad', 'open'].includes(result.mode)) result.mode = 'ptt'
  if (!['off', 'sonora', 'rnnoise'].includes(result.noise)) result.noise = 'off'
  if (result.jitterMin > result.jitterMax) {
    result.jitterMin = 20
    result.jitterMax = 200
  }
  return result
}
export interface AudioEnvironment {
  preferences: AudioPreferences
  devices: { id: string; name: string; input: boolean; output: boolean }[]
  microphone: string
  apple: boolean
  sonora: boolean
  rnnoise: boolean
  mouse: boolean
  systemBinding: boolean
  permission: string | null
  limitations: string[]
}
export const useAudioStore = create<{ preferences: AudioPreferences }>(() => ({
  preferences: defaults,
}))
export async function environment() {
  const result = await invokeBridge<AudioEnvironment>('audio_environment')
  result.preferences = migratePreferences(result.preferences)
  useAudioStore.setState({ preferences: result.preferences })
  return result
}
export async function save(preferences: AudioPreferences) {
  await invokeBridge('save_audio', { preferences })
  useAudioStore.setState({ preferences })
}
export const audioLevel = () => invokeBridge<number>('audio_level')
export const testAudio = (preferences: AudioPreferences, kind: string) =>
  invokeBridge<number>('test_audio', { preferences, kind })
export const stopAudioTest = () => invokeBridge<void>('stop_audio_test')
/** Live input level (dBFS) while a test or call is running. */
export const onAudioLevel = (handler: (level: number) => void) =>
  listen<number>('ss://audio-level', (event) => handler(event.payload))
