import { memo, useId } from 'react'
import type { MemberView } from '../../lib/generated/MemberView'
import type { ParticipantView } from '../../lib/generated/ParticipantView'
import { useVoiceStore } from '../voice/store'

export const MemberRow = memo(function MemberRow({
  server,
  member,
  participant,
  onMenu,
}: {
  server: string
  member: MemberView
  participant?: ParticipantView
  onMenu: (id: string) => void
}) {
  const speaking = useVoiceStore((s) => s.speaking[server]?.includes(member.id) ?? false)
  const tooltip = useId()
  const talking =
    speaking && !participant?.muted && !participant?.deafened && !participant?.serverMuted
  return (
    <div
      className="live-member"
      draggable={member.canMove && !!participant}
      onDragStart={(event) => {
        event.dataTransfer.setData(
          'application/x-socialspeak-member',
          JSON.stringify({ server, user: member.id }),
        )
        event.dataTransfer.effectAllowed = 'move'
      }}
    >
      <button
        className="live-member-button"
        aria-describedby={tooltip}
        onClick={(event) => {
          event.currentTarget.focus()
          onMenu(member.id)
        }}
        onContextMenu={(event) => {
          event.preventDefault()
          event.currentTarget.focus()
          onMenu(member.id)
        }}
      >
        <span
          className={`live-avatar ${talking ? 'speaking' : ''}`}
          aria-label={talking ? 'Speaking' : undefined}
        >
          {(member.displayName || member.username).slice(0, 2).toUpperCase()}
        </span>
        <span className="member-names">
          <strong>{member.displayName || member.username}</strong>
          <small>@{member.username}</small>
        </span>
        <span className="member-flags">
          {participant?.muted && (
            <span title="Microphone muted" aria-label="Microphone muted">
              M
            </span>
          )}
          {participant?.deafened && (
            <span title="Deafened" aria-label="Deafened">
              D
            </span>
          )}
          {participant?.serverMuted && (
            <span title="Server muted" aria-label="Server muted">
              S
            </span>
          )}
          {member.localMuted && (
            <span title="Muted for you" aria-label="Muted for you">
              L
            </span>
          )}
        </span>
        <span
          className={`member-presence ${member.presence.replaceAll(' ', '-')}`}
          aria-label={member.presence}
        />
      </button>
      <span className="member-hover-card" role="tooltip" id={tooltip}>
        <strong>{member.displayName || member.username}</strong> @{member.username}
        <br />
        {member.roleName} · {member.presence}
        {member.activity && (
          <>
            <br />
            {member.activity}
          </>
        )}
      </span>
    </div>
  )
})
