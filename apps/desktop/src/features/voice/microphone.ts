import type { ChannelView } from '../../lib/generated/ChannelView'
import type { ConnectionStatus } from '../../lib/generated/ConnectionStatus'
import type { ServerView } from '../../lib/generated/ServerView'
import { useAudioStore } from '../settings/audio/store'
import { useShortcuts } from '../settings/shortcuts/store'
import { useVoiceStore } from './store'

const modes = {
  vad: {
    label: 'Voice activation',
    beforeJoin: 'After joining, your microphone will transmit when it detects speech.',
    inCall: 'Voice activation is listening for speech.',
  },
  open: {
    label: 'Open microphone',
    beforeJoin: 'Your microphone will transmit automatically as soon as you join.',
    inCall: 'Open microphone is on. Audio transmits automatically while you are unmuted.',
  },
}

function blocked(label: string, guidance: string) {
  return { label, guidance, transmitting: false, canPress: false }
}

// Without a room, describe the saved talking mode before a call starts.
export function useMicrophoneGuidance(room?: {
  server?: string
  view?: ServerView
  channel?: ChannelView
  connection?: ConnectionStatus['kind']
}) {
  const { server, view, channel, connection } = room ?? {}
  const active = useVoiceStore((s) => s.active)
  const mode = useAudioStore((s) => (s.loaded ? s.preferences.mode : null))
  const inAppKey = useAudioStore((s) => s.preferences.binding)
  const globalKey = useShortcuts(({ snapshot }) => {
    if (!snapshot?.preferences.ptt && !snapshot?.preferences.portal) return null
    if (!snapshot.running || snapshot.suspended) return ''
    return snapshot.systemBinding ? 'your system push-to-talk shortcut' : (snapshot.labels[0] ?? '')
  })
  const speaking = useVoiceStore((s) =>
    Boolean(server && view && s.speaking[server]?.includes(view.ownUserId)),
  )
  const joined = !!active && active.serverId === server && active.channelId === channel?.id
  if (room) {
    if (connection === 'restoring' && joined)
      return blocked('Rejoining voice', 'Your microphone is off while voice reconnects.')
    if (connection === 'reconnecting' && joined)
      return blocked('Reconnecting', 'Voice transmission is paused while this call reconnects.')
    if (connection !== 'connected')
      return blocked('Offline', 'Connect to the server to use voice in this channel.')
    if (!channel || !view) return blocked('Status unavailable', 'Microphone status is unavailable.')
    if (joined) {
      if (!active.ready)
        return blocked('Rejoining voice', 'Your microphone is off while voice reconnects.')
      if (active.deafened) return blocked('Deafened', 'You are deafened. Others cannot hear you.')
      if (active.muted) return blocked('Muted', 'You are muted. Others cannot hear you.')
      if (
        view.participants.some(
          (p) => p.userId === view.ownUserId && p.channelId === channel.id && p.serverMuted,
        )
      )
        return blocked('Server muted', 'A moderator muted you in this channel.')
    }
    if (!channel.canSpeak) return blocked('Listen only', 'You can only listen in this channel.')
  }
  const transmitting = joined && speaking
  if (transmitting)
    return {
      label: 'Transmitting',
      guidance: 'Your microphone is transmitting to this channel.',
      transmitting,
      canPress: mode === 'ptt',
    }
  if (mode === 'ptt') {
    const key = globalKey ?? inAppKey.replace(/^(Key|Digit)([A-Z0-9])$/, '$2')
    const control = key || 'the talk button'
    return {
      label: 'Hold to talk',
      guidance: joined ? `Hold ${control} to speak.` : `After joining, hold ${control} to speak.`,
      transmitting: false,
      canPress: joined,
    }
  }
  const configured = mode === 'vad' || mode === 'open' ? modes[mode] : null
  if (!configured) return blocked('Mode unavailable', 'Check your talking mode in Audio settings.')
  return {
    label: configured.label,
    guidance: joined ? configured.inCall : configured.beforeJoin,
    transmitting,
    canPress: false,
  }
}
