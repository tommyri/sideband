import { invokeBridge } from '../../lib/tauri'
export interface Source {
  id: string
  label: string
  kind: 'display' | 'window' | 'portal'
}
export interface Share {
  server: string
  channel: string
  source: string
  state: string
  viewers: number
  error: string | null
}
export interface Snapshot {
  canShare: boolean
  tracks: { track: number; user: string; channel: string }[]
  share: Share | null
  watching: number[]
}
export const sources = () => invokeBridge<Source[]>('screen_sources')
export const preview = (source: string) =>
  invokeBridge<{ width: number; height: number; rgba: number[] }>('screen_preview', { source })
export const snapshot = (serverId: string) =>
  invokeBridge<Snapshot>('screen_snapshot', { serverId })
export const start = (
  serverId: string,
  channelId: string,
  source: string,
  preset: string,
  cursor: boolean,
) => invokeBridge<void>('start_screen_share', { serverId, channelId, source, preset, cursor })
export const stop = () => invokeBridge<void>('stop_screen_share')
export const watch = (serverId: string, trackId: number) =>
  invokeBridge<string>('open_stream_viewer', { serverId, trackId })
export function message(error: unknown) {
  return error && typeof error === 'object' && 'message' in error
    ? String(error.message)
    : String(error)
}
