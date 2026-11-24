import type { Ref } from 'react'
import { Mic, SlidersHorizontal, Volume2 } from 'lucide-react'
import type { ChannelView } from '../../lib/generated/ChannelView'
import type { ConnectionStatus } from '../../lib/generated/ConnectionStatus'
import type { MemberView } from '../../lib/generated/MemberView'
import type { ParticipantView } from '../../lib/generated/ParticipantView'
import type { ServerView } from '../../lib/generated/ServerView'
import { useMicrophoneGuidance } from '../voice/microphone'
import { useVoiceStore } from '../voice/store'
import { RoomJoinButton } from './RoomJoinButton'
import { MemberRow } from './MemberRow'

export function VoiceRoomPreview({
  headingRef,
  server,
  serverName,
  channel,
  connection,
  view,
  members,
  participants,
  openMember,
  openAudio,
}: {
  headingRef: Ref<HTMLHeadingElement>
  server: string
  serverName: string | undefined
  channel: ChannelView
  connection?: ConnectionStatus['kind']
  view: ServerView
  members: Map<string, MemberView>
  participants: ParticipantView[]
  openMember: (id: string) => void
  openAudio: () => void
}) {
  const active = useVoiceStore((s) => s.active)
  const joining = useVoiceStore(
    (s) => s.joining?.serverId === server && s.joining.channelId === channel.id,
  )
  const joined = active?.serverId === server && active.channelId === channel.id
  const mic = useMicrophoneGuidance({ server, view, channel, connection })
  return (
    <section className="room-preview" aria-labelledby="voice-room-name">
      <header className="room-intro">
        <span className="room-kind">
          <Volume2 size={18} /> Voice room
        </span>
        <h1 id="voice-room-name" ref={headingRef} tabIndex={-1}>
          {channel.name}
        </h1>
        <p>
          {[serverName, channel.profile && `${channel.profile} profile`]
            .filter(Boolean)
            .join(' · ')}
        </p>
      </header>
      <div className="room-columns">
        <section className="room-people" aria-labelledby="room-people-title">
          <header>
            <h2 id="room-people-title">In the room</h2>
            <span>
              {participants.length} {participants.length === 1 ? 'person' : 'people'}
            </span>
          </header>
          {participants.length ? (
            <ul>
              {participants.map((participant) => {
                const member = members.get(participant.userId)
                return (
                  <li key={participant.connectionId}>
                    {member ? (
                      <MemberRow
                        server={server}
                        member={member}
                        participant={participant}
                        onMenu={openMember}
                      />
                    ) : (
                      'Unknown member'
                    )}
                  </li>
                )
              })}
            </ul>
          ) : (
            <p>{joined ? 'Waiting for the room roster…' : 'No one is here yet.'}</p>
          )}
        </section>
        <section className="room-microphone" aria-labelledby="room-mic-title">
          <h2 id="room-mic-title">
            <Mic size={18} /> Your microphone
          </h2>
          <h3>{mic.label === 'Hold to talk' ? 'Push to talk' : mic.label}</h3>
          <p>{mic.guidance}</p>
          <div role="status" className="room-membership">
            {joined
              ? `You’re in ${channel.name}.`
              : joining
                ? `Joining ${channel.name}…`
                : !channel.canJoin
                  ? 'You don’t have permission to join this room.'
                  : active
                    ? 'Joining this room will leave your current call.'
                    : ''}
          </div>
          {joined ? (
            <button
              className="button secondary"
              onClick={(event) => {
                event.currentTarget.focus()
                openAudio()
              }}
            >
              <SlidersHorizontal size={16} /> Audio settings
            </button>
          ) : (
            <RoomJoinButton
              server={server}
              channel={channel}
              connected={connection === 'connected'}
            />
          )}
        </section>
      </div>
    </section>
  )
}
