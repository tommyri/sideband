import { useMemo } from 'react'
import type { ServerView } from '../../lib/generated/ServerView'
import { dmName, dmOnline, sortedDms } from './model'
import './dm.css'

export function DmList({
  view,
  selected,
  connected,
  select,
  create,
}: {
  view: ServerView
  selected: string | undefined
  connected: boolean
  select: (id: string) => void
  create: () => void
}) {
  const members = useMemo(() => new Map(view.members.map((m) => [m.id, m])), [view.members])
  const conversations = useMemo(() => sortedDms(view.directMessages), [view.directMessages])
  return (
    <section className="dm-list" aria-label="Direct messages">
      <div className="dm-list-title">
        <h3>Direct messages</h3>
        <button
          className="button secondary"
          disabled={!connected}
          onClick={create}
          aria-label="New direct message"
        >
          +
        </button>
      </div>
      {conversations.length === 0 && (
        <p className="dm-empty">Private conversations on this server.</p>
      )}
      {conversations.map((dm) => {
        const name = dmName(dm, members, view.ownUserId)
        const online = dmOnline(dm, members, view.ownUserId)
        return (
          <button
            key={dm.id}
            className={`live-channel dm-item ${selected === dm.id ? 'selected' : ''}`}
            aria-label={`Open ${dm.kind === 5 ? 'group' : 'direct'} message ${name}`}
            aria-current={selected === dm.id ? 'page' : undefined}
            onClick={() => select(dm.id)}
          >
            <span
              className={`dm-presence ${online ? 'online' : ''}`}
              aria-label={online ? 'Participant online' : 'Participants offline'}
            >
              ●
            </span>
            <span className="dm-name">{name}</span>
            {dm.unread && (
              <span className="unread-marker" aria-label="Unread messages">
                ●
              </span>
            )}
            {dm.mentions > 0 && (
              <span className="unread-count" aria-label={`${dm.mentions} mentions`}>
                {dm.mentions}
              </span>
            )}
          </button>
        )
      })}
    </section>
  )
}
