import { useCallback, useEffect, useRef, useState } from 'react'
import type { AdminChannel } from '../../lib/generated/AdminChannel'
import type { ChannelAdminSnapshot } from '../../lib/generated/ChannelAdminSnapshot'
import type { ChannelAdminAction } from '../../lib/generated/ChannelAdminAction'
import type { ServerView } from '../../lib/generated/ServerView'
import { errorText, onChatEvent } from '../chat/api'
import { refreshVoice } from '../voice/store'
import { channelAction, channelKind, channelSnapshot } from './channels'
import { ChannelEditor } from './ChannelEditor'
import { ChannelMove } from './ChannelMove'
import './admin.css'
type Props = { server: string; view: ServerView; connected: boolean; close: () => void }
type Mode = { kind: 'create' } | { kind: 'edit' | 'move' | 'delete'; channel: AdminChannel }
export function ChannelManager(props: Props) {
  return (
    <ChannelPanel
      key={`${props.server}:${props.view.ownUserId}:${props.connected}:${props.view.canManageChannels}`}
      {...props}
    />
  )
}
function ChannelPanel({ server, view, connected, close }: Props) {
  const dialog = useRef<HTMLDialogElement>(null)
  const alive = useRef(false)
  const generation = useRef(0)
  const [data, setData] = useState<ChannelAdminSnapshot | null>(null)
  const [loading, setLoading] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [notice, setNotice] = useState('')
  const [mode, setMode] = useState<Mode | null>(null)
  const [confirmation, setConfirmation] = useState('')
  const permitted = connected && view.canManageChannels
  const load = useCallback(async () => {
    if (!permitted) return
    const revision = ++generation.current
    setLoading(true)
    try {
      const next = await channelSnapshot(server)
      if (alive.current && revision === generation.current) setData(next)
    } catch (e) {
      if (alive.current && revision === generation.current) {
        setData(null)
        setError(errorText(e))
      }
    } finally {
      if (alive.current && revision === generation.current) setLoading(false)
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
    const requests = generation
    // Synchronize the administration view with server snapshots and live channel changes.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    void load()
    const stop = onChatEvent(({ serverId, event }) => {
      if (
        serverId === server &&
        ['channelsChanged', 'resynced', 'rolesChanged', 'membersChanged'].includes(event.kind)
      )
        void load()
    })
    return () => {
      requests.current++
      stop()
    }
  }, [load, server])
  const mutate = async (action: ChannelAdminAction) => {
    if (!permitted || busy) return
    setBusy(true)
    setError('')
    setNotice('')
    try {
      await channelAction(server, action)
      if (!alive.current) return
      setMode(null)
      setConfirmation('')
      setNotice(
        action.kind === 'delete'
          ? 'Channel deleted.'
          : action.kind === 'create'
            ? 'Channel created.'
            : action.kind === 'move'
              ? 'Channel moved.'
              : 'Channel settings saved.',
      )
      await refreshVoice(server)
      if (alive.current) await load()
    } catch (e) {
      if (alive.current)
        setError(
          `${errorText(e)} If the connection was interrupted, refresh the list before retrying.`,
        )
    } finally {
      if (alive.current) setBusy(false)
    }
  }
  const choose = (next: Mode) => {
    setMode(next)
    setConfirmation('')
    setError('')
    setNotice('')
  }
  return (
    <dialog
      ref={dialog}
      className="admin-dialog"
      aria-labelledby="channels-admin-title"
      onCancel={(e) => {
        e.preventDefault()
        if (!busy) {
          if (mode) setMode(null)
          else close()
        }
      }}
    >
      <header>
        <h2 id="channels-admin-title">Manage channels</h2>
        <button className="button secondary" disabled={busy} onClick={close}>
          Close
        </button>
      </header>
      <p>
        Organize categories, text and voice channels. Only channels visible to your account are
        listed.
      </p>
      {!connected && <p role="status">Reconnect to manage channels.</p>}
      {!view.canManageChannels && <p role="alert">Manage channels permission is required.</p>}
      {error && <p role="alert">{error}</p>}
      {notice && <p role="status">{notice}</p>}
      <div className="channel-tools">
        <button
          className="button primary"
          disabled={!permitted || busy || !data || mode !== null}
          onClick={() => choose({ kind: 'create' })}
        >
          New channel
        </button>
        <button
          className="button secondary"
          disabled={!permitted || busy || loading}
          onClick={() => {
            setError('')
            void load()
          }}
        >
          Refresh
        </button>
      </div>
      {loading && <p role="status">Loading channels…</p>}
      {data && permitted && (
        <>
          {mode?.kind === 'create' && (
            <ChannelEditor
              key="create"
              original={null}
              snapshot={data}
              busy={busy}
              save={mutate}
              cancel={() => setMode(null)}
            />
          )}
          {mode?.kind === 'edit' && (
            <ChannelEditor
              key={`edit:${mode.channel.id}`}
              original={mode.channel}
              snapshot={data}
              busy={busy}
              save={mutate}
              cancel={() => setMode(null)}
            />
          )}
          {mode?.kind === 'move' && (
            <ChannelMove
              key={`move:${mode.channel.id}`}
              channel={mode.channel}
              channels={data.channels}
              busy={busy}
              save={mutate}
              cancel={() => setMode(null)}
            />
          )}
          {mode?.kind === 'delete' && (
            <section
              className="invite-confirm"
              role="alertdialog"
              aria-labelledby="delete-channel-title"
            >
              <h3 id="delete-channel-title">Delete {mode.channel.name}?</h3>
              <p>
                {mode.channel.kind === 1
                  ? 'This deletes the category and every channel inside it, including channels hidden from you. '
                  : ''}
                Messages will no longer be accessible and anyone in affected voice channels will be
                disconnected. This cannot be undone in the app.
              </p>
              <label className="channel-delete-label">
                Type {mode.channel.name} to confirm
                <input
                  autoFocus
                  value={confirmation}
                  onChange={(e) => setConfirmation(e.target.value)}
                  disabled={busy}
                />
              </label>
              <button
                className="button primary"
                disabled={
                  busy ||
                  confirmation !== mode.channel.name ||
                  !data.channels.some((c) => c.id === mode.channel.id)
                }
                onClick={() => void mutate({ kind: 'delete', channel: mode.channel.id })}
              >
                Confirm delete
              </button>
              <button className="button secondary" disabled={busy} onClick={() => setMode(null)}>
                Cancel
              </button>
            </section>
          )}
          <div className="invite-table-wrap">
            <table>
              <thead>
                <tr>
                  <th>Channel</th>
                  <th>Type / settings</th>
                  <th>Order</th>
                  <th>Actions</th>
                </tr>
              </thead>
              <tbody>
                {data.channels.map((channel) => {
                  const siblings = data.channels
                    .filter((c) => c.parentId === channel.parentId)
                    .sort((a, b) => a.position - b.position || a.id.localeCompare(b.id))
                  const index = siblings.findIndex((c) => c.id === channel.id)
                  const disabled = busy || loading || mode !== null
                  const profile = data.profiles.find((p) => p.id === channel.profile)
                  return (
                    <tr key={channel.id}>
                      <td>
                        {channel.name}
                        <small>
                          {channel.parentId
                            ? (data.channels.find((c) => c.id === channel.parentId)?.name ??
                              'Unavailable category')
                            : 'Root'}
                        </small>
                        {channel.topic && <small>{channel.topic}</small>}
                      </td>
                      <td>
                        {channelKind(channel.kind)}
                        {channel.kind === 3 && (
                          <small>
                            {profile?.name ?? 'Unknown profile'} ·{' '}
                            {(channel.bitrate ?? profile?.defaultBitrate ?? 0) / 1000} kb/s ·{' '}
                            {channel.userLimit || 'Unlimited'} users
                          </small>
                        )}
                      </td>
                      <td>
                        {channel.position + 1}
                        <div className="channel-tools">
                          <button
                            className="button secondary"
                            aria-label={`Move ${channel.name} up`}
                            disabled={disabled || index === 0}
                            onClick={() =>
                              void mutate({
                                kind: 'move',
                                channel: channel.id,
                                parentId: channel.parentId,
                                position: siblings[index - 1]!.position,
                              })
                            }
                          >
                            ↑
                          </button>
                          <button
                            className="button secondary"
                            aria-label={`Move ${channel.name} down`}
                            disabled={disabled || index === siblings.length - 1}
                            onClick={() =>
                              void mutate({
                                kind: 'move',
                                channel: channel.id,
                                parentId: channel.parentId,
                                position: siblings[index + 1]!.position,
                              })
                            }
                          >
                            ↓
                          </button>
                        </div>
                      </td>
                      <td>
                        <div className="channel-tools">
                          <button
                            className="button secondary"
                            disabled={disabled}
                            onClick={() => choose({ kind: 'edit', channel })}
                          >
                            Edit
                          </button>
                          <button
                            className="button secondary"
                            disabled={disabled}
                            onClick={() => choose({ kind: 'move', channel })}
                          >
                            Move
                          </button>
                          <button
                            className="button secondary"
                            disabled={disabled}
                            onClick={() => choose({ kind: 'delete', channel })}
                          >
                            Delete
                          </button>
                        </div>
                      </td>
                    </tr>
                  )
                })}
              </tbody>
            </table>
          </div>
          {!loading && data.channels.length === 0 && (
            <p>No channels to display. Create a category, text or voice channel to get started.</p>
          )}
        </>
      )}
    </dialog>
  )
}
