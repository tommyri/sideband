import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { ServerView } from '../../lib/generated/ServerView'
import { errorText } from '../../lib/errors'
import { loadOlderDms } from './api'
import { dmName, dmOnline, sortedDms } from './model'
import './dm.css'

export function DmList({
  server,
  view,
  selected,
  connected,
  select,
  create,
}: {
  server: string
  view: ServerView
  selected: string | undefined
  connected: boolean
  select: (id: string) => void
  create: () => void
}) {
  const members = useMemo(() => new Map(view.members.map((m) => [m.id, m])), [view.members])
  const conversations = useMemo(() => sortedDms(view.directMessages), [view.directMessages])
  const more = view.moreDirectMessages && connected
  const end = useRef<HTMLButtonElement>(null)
  const loading = useRef(false)
  const [error, setError] = useState('')
  const loadMore = useCallback(async () => {
    if (loading.current) return
    loading.current = true
    setError('')
    try {
      await loadOlderDms(server)
    } catch (e) {
      setError(errorText(e))
    } finally {
      loading.current = false
    }
  }, [server])
  // The Welcome lists the newest 100 conversations; the next page loads when
  // the end of the list scrolls into view. A new observer per page reports
  // at once if the end is still in view.
  useEffect(() => {
    const target = end.current
    if (!more || !target || typeof IntersectionObserver === 'undefined') return
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) void loadMore()
    })
    observer.observe(target)
    return () => observer.disconnect()
  }, [more, conversations.length, loadMore])
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
      {more && (
        <button ref={end} className="dm-more" onClick={() => void loadMore()}>
          Load older conversations
        </button>
      )}
      {error && (
        <p className="dm-empty" role="alert">
          {error}
        </p>
      )}
    </section>
  )
}
