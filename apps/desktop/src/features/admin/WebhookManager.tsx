import { useCallback, useEffect, useRef, useState } from 'react'
import type { ServerView } from '../../lib/generated/ServerView'
import type { WebhookView } from '../../lib/generated/WebhookView'
import type { CreatedWebhook } from '../../lib/generated/CreatedWebhook'
import { errorText, onChatEvent } from '../chat/api'
import { createWebhook, revokeWebhook, webhookName, webhookPage } from './webhooks'
import './admin.css'
type Props = { server: string; view: ServerView; connected: boolean; close: () => void }
// Connection/account changes discard one-time credentials and invalidate pending mutations.
export function WebhookManager(props: Props) {
  return (
    <WebhookPanel
      key={`${props.server}:${props.view.ownUserId}:${props.connected}:${props.view.canManageWebhooks}`}
      {...props}
    />
  )
}
function WebhookPanel({ server, view, connected, close }: Props) {
  const dialog = useRef<HTMLDialogElement>(null)
  const alive = useRef(false)
  const generation = useRef(0)
  const confirmation = useRef<HTMLButtonElement>(null)
  const [rows, setRows] = useState<WebhookView[]>([])
  const [loading, setLoading] = useState(false)
  const [hasMore, setHasMore] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [notice, setNotice] = useState('')
  const [name, setName] = useState('')
  const channels = view.channels.filter((channel) => channel.kind === 2)
  const [channel, setChannel] = useState(channels[0]?.id ?? '')
  const [created, setCreated] = useState<CreatedWebhook | null>(null)
  const [pending, setPending] = useState<WebhookView | null>(null)
  const permitted = connected && view.canManageWebhooks
  const load = useCallback(
    async (before: string | null = null) => {
      if (!permitted) return
      const revision = ++generation.current
      setLoading(true)
      try {
        const page = await webhookPage(server, before)
        if (!alive.current || revision !== generation.current) return
        setRows((old) =>
          before
            ? [...old, ...page.filter((row) => !old.some((item) => item.id === row.id))]
            : page,
        )
        setHasMore(page.length === 50)
      } catch (e) {
        if (!alive.current || revision !== generation.current) return
        setRows([])
        setHasMore(false)
        setCreated(null)
        setPending(null)
        setError(errorText(e))
      } finally {
        if (alive.current && revision === generation.current) setLoading(false)
      }
    },
    [server, permitted, setCreated, setPending],
  )
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
    // Synchronize metadata from the remote server on mount and permission/resync events.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    void load()
    const stop = onChatEvent(({ serverId, event }) => {
      if (
        serverId === server &&
        ['resynced', 'rolesChanged', 'membersChanged'].includes(event.kind)
      )
        void load()
    })
    return () => {
      requests.current++
      stop()
    }
  }, [load, server])
  const create = async () => {
    if (busy || !permitted || created) return
    setError('')
    setNotice('')
    let validName: string
    try {
      validName = webhookName(name)
      if (!channels.some((item) => item.id === channel))
        throw new Error('Choose an available text channel.')
    } catch (e) {
      setError(errorText(e))
      return
    }
    setBusy(true)
    try {
      const result = await createWebhook(server, channel, validName)
      if (!alive.current) return
      setCreated(result)
      setName('')
      setNotice('Webhook created. Save the URL before closing this panel.')
      await load()
    } catch (e) {
      if (alive.current)
        setError(
          `${errorText(e)} If the connection was interrupted, refresh before creating another webhook. A lost URL cannot be recovered; revoke that webhook and create a new one.`,
        )
    } finally {
      if (alive.current) setBusy(false)
    }
  }
  const revoke = async () => {
    if (!pending || busy || !permitted) return
    setBusy(true)
    setError('')
    setNotice('')
    try {
      await revokeWebhook(server, pending.id)
      if (!alive.current) return
      if (created?.id === pending.id) setCreated(null)
      setPending(null)
      setNotice('Webhook revoked. Its URL can no longer post messages.')
      await load()
    } catch (e) {
      if (alive.current) setError(errorText(e))
    } finally {
      if (alive.current) setBusy(false)
    }
  }
  const copy = async () => {
    if (!created) return
    try {
      await navigator.clipboard.writeText(created.url)
      if (alive.current) {
        setError('')
        setNotice('Webhook URL copied.')
      }
    } catch {
      if (alive.current) setError('Clipboard access failed. Select the URL and copy it manually.')
    }
  }
  return (
    <dialog
      className="admin-dialog"
      ref={dialog}
      aria-labelledby="webhooks-title"
      onCancel={(event) => {
        event.preventDefault()
        if (!busy) close()
      }}
    >
      <header>
        <h2 id="webhooks-title">Webhooks</h2>
        <button className="button secondary" disabled={busy} onClick={close}>
          Close
        </button>
      </header>
      <p>
        Let integrations post to a text channel. Anyone with the URL can post; keep it private. URLs
        are shown only once.
      </p>
      {!connected && <p role="status">Reconnect to manage webhooks.</p>}
      {!view.canManageWebhooks && <p role="alert">You need Manage server permission.</p>}
      <form
        className="invite-create"
        onSubmit={(event) => {
          event.preventDefault()
          void create()
        }}
      >
        <label>
          Name
          <input
            value={name}
            onChange={(event) => setName(event.target.value)}
            placeholder="e.g. Build notifications"
            disabled={!permitted || busy}
          />
        </label>
        <label>
          Text channel
          <select
            value={channel}
            onChange={(event) => setChannel(event.target.value)}
            disabled={!permitted || busy}
          >
            <option value="">Choose a channel</option>
            {channels.map((item) => (
              <option value={item.id} key={item.id}>
                #{item.name}
              </option>
            ))}
          </select>
        </label>
        <button
          className="button primary"
          disabled={!permitted || busy || created !== null || channels.length === 0}
        >
          Create webhook
        </button>
        {channels.length === 0 && (
          <small>No text channels are available. Create a text channel first.</small>
        )}
      </form>
      {created && (
        <section className="invite-code" aria-label="New webhook URL">
          <label>
            Webhook URL
            <input readOnly value={created.url} onFocus={(event) => event.target.select()} />
          </label>
          <button className="button secondary" onClick={() => void copy()}>
            Copy URL
          </button>
          <button
            className="button secondary"
            disabled={busy}
            onClick={() => {
              setCreated(null)
              setNotice('')
            }}
          >
            I saved the URL
          </button>
        </section>
      )}
      {error && <p role="alert">{error}</p>}
      {notice && <p role="status">{notice}</p>}
      <div className="invite-list-tools">
        <h3>Webhook history</h3>
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
      {loading && <p role="status">Loading webhooks…</p>}
      {permitted && (
        <>
          {!loading && rows.length === 0 && !error && <p>No webhooks to display.</p>}
          <div className="invite-table-wrap">
            <table>
              <thead>
                <tr>
                  <th>Name / creator</th>
                  <th>Channel</th>
                  <th>Status</th>
                  <th>Action</th>
                </tr>
              </thead>
              <tbody>
                {rows.map((row) => (
                  <tr key={row.id}>
                    <td>
                      {row.name}
                      <small>{row.id}</small>
                      <small>
                        By{' '}
                        {view.members.find((m) => m.id === row.createdBy)?.username ??
                          row.createdBy}
                      </small>
                      <small>Created {new Date(row.createdAt).toLocaleString()}</small>
                    </td>
                    <td>
                      {view.channels.find((c) => c.id === row.channelId)?.name ??
                        'Deleted or unavailable channel'}
                      <small>{row.channelId}</small>
                    </td>
                    <td>
                      {row.revokedAt === null ? 'Active' : 'Revoked'}
                      {row.revokedAt !== null && (
                        <small>{new Date(row.revokedAt).toLocaleString()}</small>
                      )}
                    </td>
                    <td>
                      <button
                        className="button secondary"
                        disabled={busy || row.revokedAt !== null}
                        onClick={() => {
                          setPending(row)
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
              disabled={busy || loading}
              onClick={() => void load(rows.at(-1)!.id)}
            >
              Load older webhooks
            </button>
          )}
        </>
      )}
      {pending && (
        <section
          className="invite-confirm"
          role="alertdialog"
          aria-labelledby="revoke-webhook-title"
        >
          <h3 id="revoke-webhook-title">Revoke {pending.name}?</h3>
          <p>
            Integrations using this URL will stop posting. Existing messages stay in the channel.
          </p>
          <button
            ref={confirmation}
            className="button primary"
            disabled={busy || !permitted}
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
