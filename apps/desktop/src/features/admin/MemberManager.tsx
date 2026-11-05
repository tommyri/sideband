import { useEffect, useRef, useState } from 'react'
import type { ModerationPage } from '../../lib/generated/ModerationPage'
import { invokeBridge } from '../../lib/tauri'
import { errorText } from '../chat/api'
import './admin.css'
type Props = { server: string; connected: boolean; permitted: boolean; close: () => void }
type Target = { kind: 'kick' | 'ban' | 'unban'; id: string; username: string }
const date = (value: string | null) => {
  const time = value === null ? NaN : Number(value)
  return Number.isFinite(time) && !Number.isNaN(new Date(time).getTime())
    ? new Date(time).toLocaleString()
    : 'Unavailable'
}
export function MemberManager(props: Props) {
  return <MemberPanel key={`${props.server}:${props.connected}:${props.permitted}`} {...props} />
}
function MemberPanel({ server, connected, permitted, close }: Props) {
  const dialog = useRef<HTMLDialogElement>(null)
  const alive = useRef(false)
  const generation = useRef(0)
  const [page, setPage] = useState<ModerationPage | null>(null)
  const [bans, setBans] = useState(false)
  const [query, setQuery] = useState('')
  const [search, setSearch] = useState('')
  const [cursors, setCursors] = useState<(string | null)[]>([null])
  const [loading, setLoading] = useState(true)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [notice, setNotice] = useState('')
  const [target, setTarget] = useState<Target | null>(null)
  const [reason, setReason] = useState('')
  const [duration, setDuration] = useState('86400')
  const [confirmation, setConfirmation] = useState('')
  const [revision, setRevision] = useState(0)
  const allowed = connected && permitted
  const before = cursors[cursors.length - 1]
  useEffect(() => {
    alive.current = true
    const previous = document.activeElement as HTMLElement | null
    const element = dialog.current
    element?.showModal()
    return () => {
      alive.current = false
      element?.close()
      previous?.focus()
    }
  }, [])
  useEffect(() => {
    const current = ++generation.current
    if (!allowed) return
    void invokeBridge<ModerationPage>('get_moderation_page', {
      serverId: server,
      bans,
      before,
      query: search,
    })
      .then((next) => {
        if (alive.current && generation.current === current) setPage(next)
      })
      .catch((e) => {
        if (alive.current && generation.current === current) setError(errorText(e))
      })
      .finally(() => {
        if (alive.current && generation.current === current) setLoading(false)
      })
  }, [server, allowed, bans, before, search, revision])
  const reload = () => {
    setLoading(true)
    setError('')
    setPage(null)
    setRevision((v) => v + 1)
  }
  const select = (next: Target) => {
    setTarget(next)
    setReason('')
    setDuration('86400')
    setConfirmation('')
    setError('')
    setNotice('')
  }
  const submit = async () => {
    if (
      !target ||
      busy ||
      loading ||
      !allowed ||
      [...reason].length > 500 ||
      confirmation !== target.username
    )
      return
    setBusy(true)
    setError('')
    setNotice('')
    try {
      await invokeBridge('moderate_member', {
        serverId: server,
        action: {
          kind: target.kind,
          userId: target.id,
          reason,
          durationSeconds: target.kind === 'ban' && duration !== '' ? Number(duration) : null,
        },
      })
      if (!alive.current) return
      setNotice(
        target.kind === 'kick'
          ? `${target.username} was disconnected.`
          : target.kind === 'ban'
            ? `${target.username} was banned.`
            : `Ban lifted for ${target.username}.`,
      )
      setTarget(null)
      reload()
    } catch (e) {
      if (alive.current) setError(errorText(e))
    } finally {
      if (alive.current) setBusy(false)
    }
  }
  const blocked = loading || busy || !allowed
  return (
    <dialog
      className="admin-dialog member-manager"
      ref={dialog}
      aria-labelledby="member-manager-title"
      onCancel={(e) => {
        e.preventDefault()
        if (!busy) close()
      }}
    >
      <header>
        <h2 id="member-manager-title">Members &amp; moderation</h2>
        <button className="button secondary" onClick={close} disabled={busy}>
          Close
        </button>
      </header>
      {!allowed ? (
        <p role="alert">Connect with moderator permissions to manage members.</p>
      ) : (
        <>
          <p>
            Actions are checked by the server. You cannot moderate yourself or members protected by
            the role hierarchy. Manage role assignments in Roles and permissions.
          </p>
          <div className="channel-tools">
            <button
              className="button secondary"
              aria-pressed={!bans}
              disabled={blocked || !!target}
              onClick={() => {
                setBans(false)
                setCursors([null])
                reload()
              }}
            >
              Members
            </button>
            <button
              className="button secondary"
              aria-pressed={bans}
              disabled={blocked || !!target || !page?.canBan}
              onClick={() => {
                setBans(true)
                setCursors([null])
                reload()
              }}
            >
              Active bans
            </button>
            <button className="button secondary" disabled={blocked || !!target} onClick={reload}>
              Refresh
            </button>
          </div>
          <form
            className="channel-tools"
            onSubmit={(e) => {
              e.preventDefault()
              if (!blocked && !target) {
                setSearch(query.trim())
                setCursors([null])
                reload()
              }
            }}
          >
            <label>
              Search members
              <input
                value={query}
                maxLength={100}
                disabled={blocked || !!target}
                onChange={(e) => setQuery(e.target.value)}
                placeholder="Username or display name"
              />
            </label>
            <button className="button secondary" disabled={blocked || !!target}>
              Search
            </button>
          </form>
          {error && (
            <p role="alert">
              {error} Your action was not confirmed. Refresh to check the current state before
              retrying.
            </p>
          )}
          {notice && <p role="status">{notice}</p>}
          {loading && <p role="status">Loading…</p>}
          {!loading && page && (
            <>
              <p>
                {bans
                  ? 'Only currently active bans are listed. Refresh to check expiration and changes from other moderators.'
                  : 'Last session activity records authentication/session activity, not an exact last-online time.'}
              </p>
              <div className="invite-table-wrap">
                <table>
                  <thead>
                    <tr>
                      <th>Member</th>
                      <th>Roles</th>
                      {bans ? (
                        <>
                          <th>Ban details</th>
                          <th>Expires</th>
                        </>
                      ) : (
                        <>
                          <th>Joined</th>
                          <th>Last session activity</th>
                        </>
                      )}
                      <th>Actions</th>
                    </tr>
                  </thead>
                  <tbody>
                    {!bans &&
                      page.members.map((member) => (
                        <tr key={member.id}>
                          <td>
                            <strong>{member.name}</strong>
                            <br />@{member.username}
                            {member.disabled && <p>Disabled</p>}
                            {member.banned && <p>Banned</p>}
                          </td>
                          <td>
                            {member.roles.map((role, index) => (
                              <span className="member-role" key={index}>
                                {role}
                              </span>
                            ))}
                          </td>
                          <td>{date(member.joinedAt)}</td>
                          <td>{date(member.lastSessionAt)}</td>
                          <td>
                            <div className="channel-tools">
                              <button
                                className="button secondary"
                                disabled={blocked || !!target || !member.canKick}
                                onClick={() =>
                                  select({ kind: 'kick', id: member.id, username: member.username })
                                }
                              >
                                Kick
                              </button>
                              <button
                                className="button primary danger"
                                disabled={blocked || !!target || !member.canBan}
                                onClick={() =>
                                  select({ kind: 'ban', id: member.id, username: member.username })
                                }
                              >
                                Ban
                              </button>
                            </div>
                          </td>
                        </tr>
                      ))}
                    {bans &&
                      page.bans.map((ban) => (
                        <tr key={ban.id}>
                          <td>
                            <strong>{ban.name}</strong>
                            <br />@{ban.username}
                          </td>
                          <td>
                            {ban.roles.map((role, index) => (
                              <span className="member-role" key={index}>
                                {role}
                              </span>
                            ))}
                          </td>
                          <td>
                            <p>{ban.reason || 'No reason provided'}</p>
                            <small>
                              By @{ban.bannedBy} · {date(ban.createdAt)}
                            </small>
                          </td>
                          <td>{ban.expiresAt === null ? 'Permanent' : date(ban.expiresAt)}</td>
                          <td>
                            <button
                              className="button secondary"
                              disabled={blocked || !!target || !ban.canUnban}
                              onClick={() =>
                                select({ kind: 'unban', id: ban.userId, username: ban.username })
                              }
                            >
                              Unban
                            </button>
                          </td>
                        </tr>
                      ))}
                  </tbody>
                </table>
              </div>
              {(bans ? page.bans : page.members).length === 0 && (
                <p>No {bans ? 'active bans' : 'members'} match this search.</p>
              )}
              <div className="channel-tools">
                <button
                  className="button secondary"
                  disabled={blocked || !!target || cursors.length < 2}
                  onClick={() => {
                    setCursors((v) => v.slice(0, -1))
                    reload()
                  }}
                >
                  Previous page
                </button>
                <span>Page {cursors.length}</span>
                <button
                  className="button secondary"
                  disabled={blocked || !!target || !page.nextCursor}
                  onClick={() => {
                    setCursors((v) => [...v, page.nextCursor])
                    reload()
                  }}
                >
                  Next page
                </button>
              </div>
            </>
          )}
          {target && (
            <form
              className="invite-confirm"
              onSubmit={(e) => {
                e.preventDefault()
                void submit()
              }}
            >
              <h3>
                {target.kind === 'unban' ? 'Unban' : target.kind === 'ban' ? 'Ban' : 'Kick'} @
                {target.username}
              </h3>
              <p>
                {target.kind === 'kick'
                  ? 'Disconnect all current sessions. This member may reconnect immediately.'
                  : target.kind === 'ban'
                    ? 'Disconnect all sessions and prevent sign-in until the ban expires or is lifted.'
                    : 'Allow this member to sign in again. All active bans for this member will be lifted.'}
              </p>
              {target.kind !== 'unban' && (
                <label>
                  Reason (optional)
                  <textarea
                    value={reason}
                    disabled={busy}
                    maxLength={1000}
                    onChange={(e) => setReason(e.target.value)}
                  />
                  <small>{[...reason].length}/500 characters</small>
                </label>
              )}
              {target.kind === 'ban' && (
                <label>
                  Ban duration
                  <select
                    value={duration}
                    disabled={busy}
                    onChange={(e) => setDuration(e.target.value)}
                  >
                    <option value="3600">1 hour</option>
                    <option value="86400">1 day</option>
                    <option value="604800">7 days</option>
                    <option value="2592000">30 days</option>
                    <option value="">Permanent</option>
                  </select>
                </label>
              )}
              <label>
                Type {target.username} to confirm
                <input
                  autoFocus
                  value={confirmation}
                  disabled={busy}
                  onChange={(e) => setConfirmation(e.target.value)}
                  autoComplete="off"
                />
              </label>
              <div className="channel-tools">
                <button
                  type="button"
                  className="button secondary"
                  disabled={busy}
                  onClick={() => {
                    setTarget(null)
                    setError('')
                  }}
                >
                  Cancel action
                </button>
                <button
                  className="button primary danger"
                  disabled={blocked || confirmation !== target.username || [...reason].length > 500}
                >
                  {busy ? 'Submitting…' : 'Confirm action'}
                </button>
              </div>
            </form>
          )}
        </>
      )}
    </dialog>
  )
}
