import type { ChannelView } from '../../lib/generated/ChannelView'
import type { ConnectionStatus } from '../../lib/generated/ConnectionStatus'
import type { ServerView } from '../../lib/generated/ServerView'
import { useAudioStore } from '../settings/audio/store'
import { useVoiceStore } from './store'

const modes = {
  ptt: {
    label: 'Hold to talk',
    beforeJoin: 'After joining, hold the talk button or your push-to-talk binding to transmit.',
    inCall: 'Hold the talk button or your push-to-talk binding to transmit.',
  },
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

export function useChannelMicrophone(
  server: string | undefined,
  view: ServerView | undefined,
  channel: ChannelView | undefined,
  connection: ConnectionStatus['kind'] | undefined,
) {
  const active = useVoiceStore((s) => s.active)
  const mode = useAudioStore((s) => (s.loaded ? s.preferences.mode : null))
  const speaking = useVoiceStore((s) =>
    Boolean(server && view && s.speaking[server]?.includes(view.ownUserId)),
  )
  const joined = !!active && active.serverId === server && active.channelId === channel?.id
  if (connection === 'reconnecting' && joined)
    return blocked('Reconnecting', 'Voice transmission is paused while this call reconnects.')
  if (connection !== 'connected')
    return blocked('Offline', 'Connect to the server to use voice in this channel.')
  if (!channel || !view) return blocked('Status unavailable', 'Microphone status is unavailable.')
  if (joined) {
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

  const configured = mode === 'ptt' || mode === 'vad' || mode === 'open' ? modes[mode] : null
  const transmitting = joined && speaking
  if (transmitting)
    return {
      label: 'Transmitting',
      guidance: 'Your microphone is transmitting to this channel.',
      transmitting,
      canPress: mode === 'ptt',
    }
  if (!configured) return blocked('Mode unavailable', 'Check your talking mode in Audio settings.')
  return {
    label: configured.label,
    guidance: joined ? configured.inCall : configured.beforeJoin,
    transmitting,
    canPress: joined && mode === 'ptt',
  }
}
