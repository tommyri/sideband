import { record } from '../testing/report'
import { invokeBridge } from '../../lib/tauri'
import type { MemberAction } from '../../lib/generated/MemberAction'
import type { ServerView } from '../../lib/generated/ServerView'
import type { DeviceRoutePayload } from '../../lib/generated/DeviceRoutePayload'

export const getServerView = (serverId: string) =>
  invokeBridge<ServerView>('get_server_view', { serverId })
export const joinVoice = (
  serverId: string,
  channelId: string,
  state?: { muted: boolean; deafened: boolean },
) =>
  invokeBridge<void>('join_voice', {
    serverId,
    channelId,
    ...(state && { startMuted: state.muted, startDeafened: state.deafened }),
  })
export const leaveVoice = () => invokeBridge<void>('leave_voice')
// Each invoke runs as its own native task, so a fast tap could apply release
// before press and leave the gate open. Send each PTT change only after the
// previous one settled.
let pttQueue: Promise<unknown> = Promise.resolve()
export const voiceControl = (action: 'mute' | 'deafen' | 'ptt', enabled: boolean) => {
  record('local-voice-control', { action, enabled })
  if (action !== 'ptt') return invokeBridge<void>('voice_control', { action, enabled })
  const next = pttQueue.then(() => invokeBridge<void>('voice_control', { action, enabled }))
  pttQueue = next.catch(() => {})
  return next
}
export const memberAction = (input: MemberAction) => invokeBridge<void>('member_action', { input })

// The call's device indicator: empty while both streams are on the device the
// user wants. A missing device is reopened in the background.
export function deviceRouteText(capture: DeviceRoutePayload, playback: DeviceRoutePayload) {
  if (capture === 'missing' && playback === 'missing')
    return 'Audio devices disconnected. Reconnecting…'
  const text = (route: DeviceRoutePayload, device: string) =>
    route === 'missing'
      ? `${device} disconnected. Reconnecting…`
      : route === 'fallback'
        ? `${device} unavailable; using the system default.`
        : ''
  return [text(capture, 'Microphone'), text(playback, 'Audio output')].filter(Boolean).join(' ')
}
