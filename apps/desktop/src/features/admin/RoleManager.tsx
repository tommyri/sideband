import { useCallback, useEffect, useRef, useState } from 'react'
import type { ServerView } from '../../lib/generated/ServerView'
import type { RoleSnapshot } from '../../lib/generated/RoleSnapshot'
import type { RoleAction } from '../../lib/generated/RoleAction'
import type { AdminRole } from '../../lib/generated/AdminRole'
import type { RoleMember } from '../../lib/generated/RoleMember'
import { errorText, onChatEvent } from '../chat/api'
import { refreshVoice } from '../voice/store'
import { roleSnapshot, roleAction } from './roles'
import { RoleEditor } from './RoleEditor'
import { MemberRoles } from './MemberRoles'
import { ChannelOverrides } from './ChannelOverrides'
import './admin.css'
type Props = { server: string; view: ServerView; connected: boolean; close: () => void }
type Mode =
  | { kind: 'edit'; role: AdminRole | null }
  | { kind: 'assign'; member: RoleMember }
  | { kind: 'delete'; role: AdminRole }
export function RoleManager(props: Props) {
  return (
    <RolePanel
      key={`${props.server}:${props.view.ownUserId}:${props.connected}:${props.view.canManageRoles}:${props.view.canManageChannels}`}
      {...props}
    />
  )
}
function RolePanel({ server, view, connected, close }: Props) {
  const dialog = useRef<HTMLDialogElement>(null)
  const alive = useRef(false)
  const generation = useRef(0)
  const [data, setData] = useState<RoleSnapshot | null>(null)
  const [busy, setBusy] = useState(false)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState('')
  const [notice, setNotice] = useState('')
  const [mode, setMode] = useState<Mode | null>(null)
  const [tab, setTab] = useState('roles')
  const [query, setQuery] = useState('')
  const [dragged, setDragged] = useState('')
  const [confirmation, setConfirmation] = useState('')
  const [saved, setSaved] = useState(0)
  const permitted = connected && (view.canManageRoles || view.canManageChannels)
  const load = useCallback(async () => {
    if (!permitted) return
    const revision = ++generation.current
    setLoading(true)
    try {
      const next = await roleSnapshot(server)
      if (alive.current && generation.current === revision) setData(next)
    } catch (e) {
      if (alive.current && generation.current === revision) {
        setData(null)
        setError(errorText(e))
      }
    } finally {
      if (alive.current && generation.current === revision) setLoading(false)
    }
  }, [server, permitted])
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
    // Synchronize this dialog with the server's live role and member state.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    void load()
    const requests = generation
    const stop = onChatEvent((e) => {
      if (
        e.serverId === server &&
        ['membersChanged', 'channelsChanged', 'resynced'].includes(e.event.kind)
      )
        void load()
    })
    return () => {
      requests.current++
      stop()
    }
  }, [load, server])
  const mutate = async (action: RoleAction) => {
    if (!permitted || busy || loading) return
    setBusy(true)
    setError('')
    setNotice('')
    try {
      await roleAction(server, action)
      if (!alive.current) return
      setMode(null)
      setConfirmation('')
      setSaved((n) => n + 1)
      setNotice('Permissions saved.')
      await refreshVoice(server)
      if (alive.current) await load()
    } catch (e) {
      if (alive.current) setError(errorText(e))
    } finally {
      if (alive.current) setBusy(false)
    }
  }
  return (
    <dialog
      ref={dialog}
      className="admin-dialog role-dialog"
      aria-labelledby="roles-title"
      onCancel={(e) => {
        e.preventDefault()
        if (!busy) close()
      }}
    >
      <header>
        <h2 id="roles-title">Roles and permissions</h2>
        <button className="button secondary" disabled={busy} onClick={close}>
          Close
        </button>
      </header>
      {!permitted && (
        <p role="alert">Connect to the server with Manage roles or Manage channels permission.</p>
      )}
      {error && <p role="alert">{error}</p>}
      {notice && <p role="status">{notice}</p>}
      <nav className="channel-tools" aria-label="Permission settings">
        {['roles', 'members', 'overrides'].map((t) => (
          <button
            key={t}
            className="button secondary"
            disabled={busy || !!mode}
            aria-pressed={tab === t}
            onClick={() => setTab(t)}
          >
            {t === 'roles' ? 'Roles' : t === 'members' ? 'Member roles' : 'Channel overrides'}
          </button>
        ))}
        <button
          className="button secondary"
          disabled={busy || !permitted || loading || !!mode}
          onClick={() => void load()}
        >
          Refresh
        </button>
      </nav>
      {loading && <p role="status">Refreshing permissions…</p>}
      {permitted && data && (
        <>
          {mode?.kind === 'edit' && (
            <RoleEditor
              key={mode.role?.id ?? 'new'}
              original={mode.role}
              data={data}
              busy={busy || loading}
              save={mutate}
              cancel={() => setMode(null)}
            />
          )}
          {mode?.kind === 'assign' && (
            <MemberRoles
              key={mode.member.id}
              original={mode.member}
              data={data}
              own={view.ownUserId}
              busy={busy || loading}
              save={mutate}
              cancel={() => setMode(null)}
            />
          )}
          {mode?.kind === 'delete' && (
            <form
              onSubmit={(e) => {
                e.preventDefault()
                if (confirmation === mode.role.name)
                  void mutate({ kind: 'delete', role: mode.role.id })
              }}
            >
              <h3>Delete {mode.role.name}?</h3>
              <p>This removes all member assignments and channel overrides for this role.</p>
              <label>
                Type the role name to confirm
                <input
                  value={confirmation}
                  disabled={busy}
                  onChange={(e) => setConfirmation(e.target.value)}
                />
              </label>
              <button
                className="button primary danger"
                disabled={
                  busy ||
                  loading ||
                  confirmation !== mode.role.name ||
                  !data.manageRoles ||
                  !data.roles.find((r) => r.id === mode.role.id)?.editable
                }
              >
                Delete role permanently
              </button>
              <button
                type="button"
                className="button secondary"
                disabled={busy}
                onClick={() => {
                  setMode(null)
                  setConfirmation('')
                }}
              >
                Cancel
              </button>
            </form>
          )}
          {!mode && tab === 'roles' && (
            <>
              <p>
                Highest role first. Drag an editable role onto another to place it above that role,
                or use Move up and Move down. Protected roles are above your authority.
              </p>
              {!data.supportsV2 && (
                <p>Update the server to enable reordering and clearing role colors.</p>
              )}
              <button
                className="button primary"
                disabled={busy || loading || !data.manageRoles || data.createPosition === null}
                onClick={() => setMode({ kind: 'edit', role: null })}
              >
                Create role
              </button>
              <ol className="role-list">
                {data.roles.map((r, index) => (
                  <li
                    key={r.id}
                    draggable={
                      !busy && !loading && data.manageRoles && data.supportsV2 && r.editable
                    }
                    onDragStart={(e) => {
                      setDragged(r.id)
                      e.dataTransfer.setData('text/plain', r.id)
                    }}
                    onDragEnd={() => setDragged('')}
                    onDragOver={(e) => {
                      if (r.editable && dragged !== r.id) e.preventDefault()
                    }}
                    onDrop={(e) => {
                      e.preventDefault()
                      if (
                        data.manageRoles &&
                        data.supportsV2 &&
                        r.editable &&
                        data.roles.find((v) => v.id === dragged)?.editable &&
                        dragged !== r.id
                      )
                        void mutate({ kind: 'move', role: dragged, before: r.id })
                      setDragged('')
                    }}
                  >
                    <span
                      className="role-swatch"
                      style={{
                        backgroundColor:
                          r.color === null
                            ? 'currentColor'
                            : `#${r.color.toString(16).padStart(6, '0')}`,
                      }}
                    />
                    <strong>{r.name}</strong>
                    <small>
                      {r.isDefault
                        ? 'Everyone'
                        : !r.editable
                          ? 'Protected'
                          : `Position ${r.position}`}
                    </small>
                    <div className="channel-tools">
                      <button
                        className="button secondary"
                        disabled={busy || loading || !data.manageRoles || !r.editable}
                        onClick={() => setMode({ kind: 'edit', role: r })}
                      >
                        Edit {r.name}
                      </button>
                      <button
                        className="button secondary"
                        aria-label={`Move ${r.name} up`}
                        disabled={
                          busy ||
                          loading ||
                          !data.manageRoles ||
                          !data.supportsV2 ||
                          !r.editable ||
                          !data.roles[index - 1]?.editable
                        }
                        onClick={() =>
                          void mutate({
                            kind: 'move',
                            role: r.id,
                            before: data.roles[index - 1]!.id,
                          })
                        }
                      >
                        ↑
                      </button>
                      <button
                        className="button secondary"
                        aria-label={`Move ${r.name} down`}
                        disabled={
                          busy ||
                          loading ||
                          !data.manageRoles ||
                          !data.supportsV2 ||
                          !r.editable ||
                          !data.roles[index + 1]?.editable ||
                          (index + 2 < data.roles.length && !data.roles[index + 2]?.editable)
                        }
                        onClick={() =>
                          void mutate({
                            kind: 'move',
                            role: r.id,
                            before: data.roles[index + 2]?.id ?? null,
                          })
                        }
                      >
                        ↓
                      </button>
                      <button
                        className="button secondary"
                        disabled={
                          busy || loading || !data.manageRoles || !r.editable || r.isDefault
                        }
                        onClick={() => setMode({ kind: 'delete', role: r })}
                      >
                        Delete {r.name}
                      </button>
                    </div>
                  </li>
                ))}
              </ol>
            </>
          )}
          {!mode && tab === 'members' && (
            <>
              <label>
                Find a member
                <input value={query} onChange={(e) => setQuery(e.target.value)} />
              </label>
              <ul className="role-list">
                {data.members
                  .filter((m) => m.name.toLowerCase().includes(query.toLowerCase()))
                  .map((m) => (
                    <li key={m.id}>
                      <strong>{m.name}</strong>
                      <small>
                        {data.roles
                          .filter((r) => r.isDefault || m.roleIds.includes(r.id))
                          .map((r) => r.name)
                          .join(', ')}
                      </small>
                      <button
                        className="button secondary"
                        disabled={busy || loading || !m.editable}
                        onClick={() => setMode({ kind: 'assign', member: m })}
                      >
                        Assign roles to {m.name}
                      </button>
                    </li>
                  ))}
              </ul>
            </>
          )}
          {!mode &&
            tab === 'overrides' &&
            (data.manageChannels ? (
              <ChannelOverrides
                key={saved}
                server={server}
                channels={view.channels}
                data={data}
                busy={busy || loading}
                save={mutate}
              />
            ) : (
              <p>Manage channels permission is required to read and edit overrides.</p>
            ))}
        </>
      )}
    </dialog>
  )
}
