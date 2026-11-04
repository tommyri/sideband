import { useEffect, useRef, useState } from 'react'
import type { InviteView } from '../../lib/generated/InviteView'
import type { CreatedInvite } from '../../lib/generated/CreatedInvite'
import type { ServerView } from '../../lib/generated/ServerView'
import { errorText, onChatEvent } from '../chat/api'
import {
  canRevoke,
  createInvite,
  invitePage,
  inviteStatus,
  parseUses,
  revokeInvite,
} from './invites'
import './admin.css'
const date = (value: number | null) => (value === null ? 'Never' : new Date(value).toLocaleString())
export function InviteManager({
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
  const [rows, setRows] = useState<InviteView[]>([])
  const [loading, setLoading] = useState(false)
  const [hasMore, setHasMore] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [notice, setNotice] = useState('')
  const [uses, setUses] = useState('1')
  const [expiry, setExpiry] = useState('24')
  const [created, setCreated] = useState<CreatedInvite | null>(null)
  const [pending, setPending] = useState<InviteView | null>(null)
  const [now, setNow] = useState(Date.now)
  const generation = useRef(0)
  const alive = useRef(true)
  const confirmation = useRef<HTMLButtonElement>(null)
  const refresh = useRef<() => void>(() => {})
  const load = async (before: string | null = null) => {
    const revision = ++generation.current
    setLoading(true)
    try {
      const page = await invitePage(server, before)
      if (!alive.current || revision !== generation.current) return
      setRows((old) =>
        before ? [...old.filter((i) => !page.some((p) => p.id === i.id)), ...page] : page,
      )
      setHasMore(page.length === 50)
    } catch (e) {
      if (alive.current && revision === generation.current) {
        setRows([])
        setHasMore(false)
        setError(errorText(e))
      }
    } finally {
      if (alive.current && revision === generation.current) setLoading(false)
    }
  }
  useEffect(() => {
    refresh.current = () => {
      void load()
    }
  })
  useEffect(() => {
    alive.current = true
    generation.current++
    const previous = document.activeElement as HTMLElement | null
    const element = dialog.current
    element?.showModal()
    if (connected) refresh.current()
    const stop = onChatEvent(({ serverId, event }) => {
      if (
        serverId === server &&
        ['invitesChanged', 'resynced', 'membersChanged', 'rolesChanged'].includes(event.kind)
      )
        refresh.current()
    })
    const timer = setInterval(() => setNow(Date.now()), 30_000)
    return () => {
      alive.current = false
      stop()
      clearInterval(timer)
      element?.close()
      previous?.focus()
    }
  }, [server, connected])
  const create = async () => {
    if (busy || !connected || !view.canCreateInvites) return
    setError('')
    setNotice('')
    let maxUses: number | null
    try {
      maxUses = parseUses(uses)
    } catch (e) {
      setError(errorText(e))
      return
    }
    setBusy(true)
    try {
      const result = await createInvite({
        serverId: server,
        maxUses,
        expiresInHours: expiry ? Number(expiry) : null,
      })
      if (!alive.current) return
      setCreated(result)
      setNotice('Invite created. Copy the code before closing this panel.')
      await load()
    } catch (e) {
      if (alive.current)
        setError(
          `${errorText(e)} If the connection was interrupted, refresh the list before creating another invite.`,
        )
    } finally {
      if (alive.current) setBusy(false)
    }
  }
  const revoke = async () => {
    if (
      !pending ||
      busy ||
      !connected ||
      !canRevoke(pending, view.ownUserId, view.canCreateInvites, view.canManageInvites)
    )
      return
    setBusy(true)
    setError('')
    setNotice('')
    try {
      await revokeInvite(server, pending.id)
      if (!alive.current) return
      if (created?.id === pending.id) setCreated(null)
      setPending(null)
      setNotice('Invite revoked. It can no longer be used.')
      await load()
    } catch (e) {
      if (alive.current) setError(errorText(e))
    } finally {
      if (alive.current) setBusy(false)
    }
  }
  return (
    <dialog
      ref={dialog}
      className="admin-dialog"
      aria-labelledby="invites-title"
      onCancel={(e) => {
        e.preventDefault()
        if (!busy) close()
      }}
    >
      <header>
        <div>
          <p className="eyebrow">Server administration</p>
          <h2 id="invites-title">Invites</h2>
        </div>
        <button className="button secondary" disabled={busy} onClick={close}>
          Close
        </button>
      </header>
      {!connected && <p role="status">Reconnect to manage invites.</p>}
      <p>
        {view.canManageInvites ? 'All server invites' : 'Your invites'} · Codes are shown once when
        created and cannot be recovered later.
      </p>
      {view.canCreateInvites && (
        <form
          className="invite-create"
          onSubmit={(e) => {
            e.preventDefault()
            void create()
          }}
        >
          <label>
            Maximum uses
            <input
              inputMode="numeric"
              value={uses}
              onChange={(e) => setUses(e.target.value)}
              placeholder="Unlimited"
              disabled={busy || !connected}
            />
          </label>
          <label>
            Expires after
            <select
              value={expiry}
              onChange={(e) => setExpiry(e.target.value)}
              disabled={busy || !connected}
            >
              <option value="1">1 hour</option>
              <option value="24">1 day</option>
              <option value="168">7 days</option>
              <option value="720">30 days</option>
              <option value="">Never</option>
            </select>
          </label>
          <button
            className="button primary"
            disabled={busy || !connected || created !== null}
            type="submit"
          >
            Create invite
          </button>
          <small>Leave maximum uses blank for unlimited uses.</small>
        </form>
      )}
      {created && (
        <section className="invite-code" aria-label="New invite code">
          <label>
            Invite code
            <input readOnly value={created.code} onFocus={(e) => e.target.select()} />
          </label>
          <button
            className="button secondary"
            onClick={() => {
              void navigator.clipboard.writeText(created.code).then(
                () => setNotice('Invite code copied.'),
                () => setError('Clipboard access failed. Select the code and copy it manually.'),
              )
            }}
          >
            Copy code
          </button>
          <button className="button secondary" disabled={busy} onClick={() => setCreated(null)}>
            I saved the code
          </button>
        </section>
      )}
      {error && <p role="alert">{error}</p>}
      {notice && <p role="status">{notice}</p>}
      <div className="invite-list-tools">
        <h3>Invite history</h3>
        <button
          className="button secondary"
          disabled={loading || busy || !connected}
          onClick={() => {
            setError('')
            void load()
          }}
        >
          Refresh
        </button>
      </div>
      {loading && <p role="status">Loading invites…</p>}
      {!loading && rows.length === 0 && <p>No invites to display.</p>}
      <div className="invite-table-wrap">
        <table>
          <thead>
            <tr>
              <th>Invite / creator</th>
              <th>Uses</th>
              <th>Expires</th>
              <th>Status</th>
              <th>Action</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((invite) => (
              <tr key={invite.id}>
                <td>
                  <code title={invite.id}>{invite.id.slice(-8)}</code>
                  <small>
                    {view.members.find((m) => m.id === invite.createdBy)?.username ??
                      invite.createdBy}
                  </small>
                  <small>Created {date(invite.createdAt)}</small>
                </td>
                <td>
                  {invite.uses} / {invite.maxUses ?? '∞'}
                </td>
                <td>{date(invite.expiresAt)}</td>
                <td>{inviteStatus(invite, now)}</td>
                <td>
                  <button
                    className="button secondary"
                    disabled={
                      busy ||
                      !connected ||
                      !canRevoke(
                        invite,
                        view.ownUserId,
                        view.canCreateInvites,
                        view.canManageInvites,
                      )
                    }
                    onClick={() => {
                      setPending(invite)
                      requestAnimationFrame(() => confirmation.current?.focus())
                    }}
                  >
                    Revoke
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {hasMore && (
        <button
          className="button secondary"
          disabled={loading || busy || !connected}
          onClick={() => void load(rows.at(-1)?.id ?? null)}
        >
          Load older invites
        </button>
      )}
      {pending && (
        <section className="invite-confirm" role="alertdialog" aria-labelledby="revoke-title">
          <h3 id="revoke-title">Revoke invite {pending.id.slice(-8)}?</h3>
          <p>
            New members will no longer be able to register with this code. Existing members keep
            their accounts.
          </p>
          <button
            ref={confirmation}
            className="button primary"
            disabled={busy || !connected}
            onClick={() => void revoke()}
          >
            Confirm revoke
          </button>
          <button className="button secondary" disabled={busy} onClick={() => setPending(null)}>
            Cancel
          </button>
        </section>
      )}
    </dialog>
  )
}
