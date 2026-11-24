import { ChevronDown, Volume2 } from 'lucide-react'
import { ActionMenu } from '../../components/ActionMenu'
import type { ChannelView } from '../../lib/generated/ChannelView'
import type { MemberView } from '../../lib/generated/MemberView'
import type { ParticipantView } from '../../lib/generated/ParticipantView'
import { useAppStore } from '../../store/app-store'
import { useVoiceStore } from '../voice/store'
import { RoomJoinButton } from './RoomJoinButton'

function occupants(participants: ParticipantView[], members: Map<string, MemberView>) {
  const people = [...new Set(participants.map((p) => p.userId))]
  if (!people.length) return 'Room is empty'
  const names = people.slice(0, 2).map((id) => {
    const member = members.get(id)
    return member?.displayName || member?.username || 'Unknown member'
  })
  if (people.length > 2) return `${names.join(', ')} and ${people.length - 2} more`
  return names.join(' and ')
}

export function VoiceLobby({
  server,
  rooms,
  members,
  participants,
  connected,
  preview,
}: {
  server: string
  rooms: ChannelView[]
  members: Map<string, MemberView>
  participants: Map<string, ParticipantView[]>
  connected: boolean
  preview: (room: ChannelView) => void
}) {
  const active = useVoiceStore((s) => s.active)
  return (
    <section className="server-landing" aria-labelledby="voice-lobby-heading">
      <h1 id="voice-lobby-heading">Voice rooms</h1>
      <p>Choose a room to see who’s there or join the conversation.</p>
      {active && <p>Joining another room will leave your current call.</p>}
      <ul className="lobby-rooms">
        {rooms.map((room) => {
          const joined = active?.serverId === server && active.channelId === room.id
          return (
            <li key={room.id}>
              <button
                type="button"
                className="lobby-preview"
                id={`lobby-preview-${room.id}`}
                aria-label={`Preview ${room.name}`}
                aria-describedby={`lobby-occupancy-${room.id}`}
                onClick={() => preview(room)}
              >
                <Volume2 size={20} />
                <span>
                  <strong>{room.name}</strong>
                  <small id={`lobby-occupancy-${room.id}`}>
                    {occupants(participants.get(room.id) ?? [], members)}
                    {!room.canJoin && ' · No permission to join'}
                  </small>
                </span>
              </button>
              {joined ? (
                <span className="room-kind">In voice</span>
              ) : (
                <RoomJoinButton
                  server={server}
                  channel={room}
                  connected={connected}
                  openPreview={() => preview(room)}
                />
              )}
            </li>
          )
        })}
      </ul>
    </section>
  )
}

export function NoChannels({ server }: { server: string }) {
  const servers = useAppStore((s) => s.servers)
  const others = servers.filter((s) => s.id !== server)
  const addServer = () => useAppStore.getState().setConnectOpen(true)
  return (
    <section className="server-landing" aria-labelledby="no-channels-heading">
      <h1 id="no-channels-heading">No channels available</h1>
      <p>
        There are no channels you can open on this server. Ask a server admin for access, or switch
        servers.
      </p>
      {others.length ? (
        <ActionMenu
          label="Switch server"
          className="button secondary"
          actions={[
            ...others.map((s) => ({
              label: s.label,
              onSelect: () => useAppStore.getState().selectServer(s.id),
            })),
            { label: 'Add server', divider: true, onSelect: addServer },
          ]}
        >
          Switch server <ChevronDown size={16} />
        </ActionMenu>
      ) : (
        <button className="button secondary" onClick={addServer}>
          Connect to another server
        </button>
      )}
    </section>
  )
}
