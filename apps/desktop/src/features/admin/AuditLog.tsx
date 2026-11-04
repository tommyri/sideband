import { useCallback, useEffect, useRef, useState } from 'react'
import type { ServerView } from '../../lib/generated/ServerView'
import type { AuditEntryView } from '../../lib/generated/AuditEntryView'
import { errorText, onChatEvent } from '../chat/api'
import { auditFilters, auditPage, emptyFilters, type AuditFilters } from './audit'
import './admin.css'

export function AuditLog({
  server,
  view,
  connected,
  close,
}: {
  server: string
  view: ServerView
  connected: boolean
  close: () => void
}) {
  const dialog = useRef<HTMLDialogElement>(null)
  const generation = useRef(0)
  const [rows, setRows] = useState<AuditEntryView[]>([])
  const [loading, setLoading] = useState(false)
  const [hasMore, setHasMore] = useState(false)
  const [error, setError] = useState('')
  const [actor, setActor] = useState('')
  const [action, setAction] = useState('')
  const [since, setSince] = useState('')
  const [until, setUntil] = useState('')
  const [filters, setFilters] = useState<AuditFilters>(emptyFilters)
  const permitted = connected && view.canViewAuditLog
  const load = useCallback(
    async (before: string | null = null) => {
      if (!permitted) return
      const revision = ++generation.current
      setLoading(true)
      setError('')
      if (!before) {
        setRows([])
        setHasMore(false)
      }
      try {
        const page = await auditPage({ serverId: server, before, ...filters })
        if (revision !== generation.current) return
        setRows((old) =>
          before ? [...old, ...page.filter((row) => !old.some((r) => r.id === row.id))] : page,
        )
        setHasMore(page.length === 50)
      } catch (e) {
        if (revision !== generation.current) return
        setRows([])
        setHasMore(false)
        setError(errorText(e))
      } finally {
        if (revision === generation.current) setLoading(false)
      }
    },
    [server, permitted, filters],
  )
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null
    const element = dialog.current
    element?.showModal()
    return () => {
      element?.close()
      previous?.focus()
    }
  }, [])
  useEffect(() => {
    const requests = generation
    // Synchronize the page with the remote server when its query or connection changes.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    void load()
    const stop = onChatEvent(({ serverId, event }) => {
      if (
        serverId === server &&
        ['resynced', 'membersChanged', 'rolesChanged'].includes(event.kind)
      )
        void load()
    })
    return () => {
      requests.current++
      stop()
    }
  }, [load, server])
  const apply = () => {
    try {
      setFilters(auditFilters(actor, action, since, until))
      setError('')
    } catch (e) {
      setError(errorText(e))
    }
  }
  const reset = () => {
    setActor('')
    setAction('')
    setSince('')
    setUntil('')
    setFilters({ ...emptyFilters })
  }
  return (
    <dialog
      className="admin-dialog audit-dialog"
      ref={dialog}
      aria-labelledby="audit-title"
      onCancel={(event) => {
        event.preventDefault()
        close()
      }}
    >
      <header>
        <h2 id="audit-title">Audit log</h2>
        <button className="button secondary" onClick={close}>
          Close
        </button>
      </header>
      <p>Review server activity. Filters match exactly; dates use your local time.</p>
      {!connected && <p role="status">Reconnect to view the audit log.</p>}
      {!view.canViewAuditLog && (
        <p role="alert">You no longer have permission to view the audit log.</p>
      )}
      <form
        className="audit-filters"
        onSubmit={(event) => {
          event.preventDefault()
          apply()
        }}
      >
        <label>
          Actor ID
          <input
            list="audit-actors"
            value={actor}
            onChange={(e) => setActor(e.target.value)}
            placeholder="All actors"
          />
        </label>
        <datalist id="audit-actors">
          {view.members.map((member) => (
            <option key={member.id} value={member.id}>
              {member.displayName || member.username}
            </option>
          ))}
        </datalist>
        <label>
          Action
          <input
            value={action}
            onChange={(e) => setAction(e.target.value)}
            placeholder="e.g. invite.create"
            maxLength={128}
          />
        </label>
        <label>
          From (inclusive)
          <input type="datetime-local" value={since} onChange={(e) => setSince(e.target.value)} />
        </label>
        <label>
          Before (exclusive)
          <input type="datetime-local" value={until} onChange={(e) => setUntil(e.target.value)} />
        </label>
        <div className="audit-tools">
          <button className="button primary" disabled={!permitted}>
            Apply filters
          </button>
          <button type="button" className="button secondary" disabled={!permitted} onClick={reset}>
            Reset filters
          </button>
          <button
            type="button"
            className="button secondary"
            disabled={!permitted || loading}
            onClick={() => void load()}
          >
            Refresh
          </button>
        </div>
      </form>
      {error && <p role="alert">{error}</p>}
      {permitted && (
        <>
          <div className="invite-table-wrap" aria-busy={loading}>
            <table>
              <thead>
                <tr>
                  <th>Time</th>
                  <th>Actor</th>
                  <th>Action / target</th>
                  <th>Details</th>
                </tr>
              </thead>
              <tbody>
                {rows.map((row) => {
                  const member = view.members.find((m) => m.id === row.actorId)
                  return (
                    <tr key={row.id}>
                      <td>
                        <time dateTime={new Date(row.createdAt).toISOString()}>
                          {new Date(row.createdAt).toLocaleString()}
                        </time>
                      </td>
                      <td>
                        {row.actorId === null
                          ? 'System'
                          : member?.displayName || member?.username || 'Unknown member'}
                        {row.actorId && <small>{row.actorId}</small>}
                      </td>
                      <td>
                        {row.action}
                        <small>
                          {row.targetKind}
                          {row.targetId ? ` · ${row.targetId}` : ''}
                        </small>
                      </td>
                      <td>
                        <details>
                          <summary>Inspect event</summary>
                          <small>Event ID: {row.id}</small>
                          <pre>{row.details}</pre>
                        </details>
                      </td>
                    </tr>
                  )
                })}
              </tbody>
            </table>
          </div>
          {loading ? (
            <p role="status">Loading audit log…</p>
          ) : rows.length === 0 && !error ? (
            <p role="status">No events match these filters.</p>
          ) : null}
          {hasMore && (
            <button
              className="button secondary"
              disabled={loading}
              onClick={() => void load(rows.at(-1)!.id)}
            >
              Load older events
            </button>
          )}
        </>
      )}
    </dialog>
  )
}
