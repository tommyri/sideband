import { Volume2 } from 'lucide-react'
import type { ChannelView } from '../../lib/generated/ChannelView'
import { joinVoice } from '../voice/api'
import { runVoice, useVoiceStore } from '../voice/store'

export function RoomJoinButton({
  server,
  channel,
  connected,
  openPreview,
}: {
  server: string
  channel: ChannelView
  connected: boolean
  openPreview?: () => void
}) {
  const busy = useVoiceStore((s) => s.busy)
  const joining = useVoiceStore(
    (s) => s.joining?.serverId === server && s.joining.channelId === channel.id,
  )
  const label = joining ? 'Joining…' : 'Join voice'
  return (
    <button
      className="button primary"
      disabled={!connected || !channel.canJoin}
      aria-disabled={busy || !connected || !channel.canJoin}
      aria-label={openPreview ? `${label} in ${channel.name}` : undefined}
      onClick={() => {
        if (busy) return
        openPreview?.()
        void runVoice(() => joinVoice(server, channel.id), {
          serverId: server,
          channelId: channel.id,
        })
      }}
    >
      <Volume2 size={18} /> {label}
    </button>
  )
}
