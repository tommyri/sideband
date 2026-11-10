import { create } from 'zustand'
import { invokeBridge } from '../../../lib/tauri'
export interface Binding {
  trigger: { key?: string; mouse?: string }
  modifiers?: { ctrl?: boolean; shift?: boolean; alt?: boolean; meta?: boolean }
  action?: string
}
export interface Preferences {
  ptt: Binding | null
  mute: Binding | null
  deafen: Binding | null
  portal: boolean
  allowEvdev: boolean
}
export interface Snapshot {
  backend?: string
  preferences: Preferences
  labels: string[]
  running: boolean
  capturing: boolean
  suspended: boolean
  captureSupported: boolean
  mouse: boolean
  systemBinding: boolean
  permission: string | null
  limitations: string[]
  linux: boolean
  error: string
  events: string[]
}
export const empty: Preferences = {
  ptt: null,
  mute: null,
  deafen: null,
  portal: false,
  allowEvdev: false,
}
export const useShortcuts = create<{ snapshot: Snapshot | null }>(() => ({ snapshot: null }))
export async function refreshShortcuts() {
  const s = await invokeBridge<Snapshot>('input_snapshot')
  useShortcuts.setState({ snapshot: s })
  return s
}
export function conflicts(p: Preferences) {
  const triggers = [p.ptt, p.mute, p.deafen]
    .filter((b): b is Binding => !!b)
    .map((b) => JSON.stringify(b.trigger))
  return new Set(triggers).size !== triggers.length
}
export const suspendShortcuts = (suspended: boolean, revision: number) =>
  invokeBridge<void>('suspend_shortcuts', { suspended, revision })
export const captureShortcut = () =>
  invokeBridge<{ binding: Binding; label: string }>('capture_shortcut')
export const cancelShortcutCapture = () => invokeBridge<void>('cancel_shortcut_capture')
export const saveShortcuts = (preferences: Preferences) =>
  invokeBridge<Snapshot>('save_shortcuts', { preferences })
export const configureSystemShortcuts = () => invokeBridge<void>('configure_system_shortcuts')
