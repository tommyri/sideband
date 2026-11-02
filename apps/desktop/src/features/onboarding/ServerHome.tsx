import { useState } from 'react'
import { getSnapshot, removeServer, reorderServers } from '../../lib/api'
import { useAppStore } from '../../store/app-store'
import { friendlyError } from './validation'

export function ServerHome() {
  const servers = useAppStore((s) => s.servers)
  const selectedServerId = useAppStore((s) => s.selectedServerId)
  const setConnectOpen = useAppStore((s) => s.setConnectOpen)
  const hydrate = useAppStore((s) => s.hydrate)
  const server = servers.find((s) => s.id === selectedServerId)
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)
  const [removing, setRemoving] = useState<string | null>(null)
  const [settings, setSettings] = useState(false)

  async function change(action: () => Promise<void>) {
    setBusy(true)
    setError('')
    try {
      await action()
      hydrate((await getSnapshot()).servers)
      setRemoving(null)
    } catch (e) {
      setError(friendlyError(e))
    } finally {
      setBusy(false)
    }
  }
  function move(delta: number) {
    if (!server) return
    const ids = servers.map((s) => s.id)
    const index = ids.indexOf(server.id)
    const other = ids[index + delta]
    if (!other) return
    ids[index + delta] = server.id
    ids[index] = other
    void change(() => reorderServers(ids))
  }
  return (
    <main className="server-home">
      <p className="eyebrow">SocialSpeak · Testing app</p>
      <h1>{server?.label ?? 'A place for your people'}</h1>
      {!server ? (
        <>
          <p>Add the server address and invite shared by your friends.</p>
          <button
            className="button primary"
            onClick={(event) => {
              event.currentTarget.focus()
              setConnectOpen(true)
            }}
          >
            Add your first server
          </button>
        </>
      ) : (
        <>
          <p>
            {server.host}:{server.port} · <strong>{server.status.kind}</strong>
          </p>
          {server.status.kind === 'reconnecting' && (
            <p className="connection-banner" role="status">
              Connection lost. Reconnecting automatically (attempt {server.status.attempt})…
            </p>
          )}
          {server.status.kind === 'failed' && (
            <p className="form-error" role="alert">
              Connection ended. Reconnect to check the certificate and sign in again.
            </p>
          )}
          {server.status.kind !== 'connected' && server.status.kind !== 'reconnecting' && (
            <button
              className="button primary"
              onClick={(event) => {
                event.currentTarget.focus()
                setConnectOpen(true, server)
              }}
            >
              Reconnect / log in
            </button>
          )}
          <section className="setup-checklist" aria-labelledby="setup-title">
            <h2 id="setup-title">Before your first voice chat</h2>
            <ol>
              <li>
                {server.status.kind === 'connected'
                  ? '✓ Connected to your server'
                  : 'Connect to your server'}
              </li>
              <li>
                <button onClick={() => setSettings(true)}>Pick your microphone and output</button>
              </li>
              <li>
                <button onClick={() => setSettings(true)}>
                  Choose a push-to-talk key or voice activation
                </button>
              </li>
            </ol>
            {settings && (
              <p role="status">
                Audio settings are coming in a future test build. Device and talking-mode setup is
                still pending.
              </p>
            )}
            <p>
              Connect to browse channels and join voice using Hold to talk. Device selection and
              global hotkeys are coming in a future test build.
            </p>
          </section>
          <section className="server-management" aria-label="Manage server">
            <h2>Manage server</h2>
            <div className="dialog-actions">
              <button
                className="button secondary"
                disabled={busy || servers[0]?.id === server.id}
                onClick={() => move(-1)}
              >
                Move earlier
              </button>
              <button
                className="button secondary"
                disabled={busy || servers.at(-1)?.id === server.id}
                onClick={() => move(1)}
              >
                Move later
              </button>
              <button
                className="button secondary"
                disabled={busy}
                onClick={() => setRemoving(server.id)}
              >
                Remove server
              </button>
            </div>
            {removing === server.id && (
              <div role="group" aria-label="Confirm removal">
                <p>
                  Remove {server.label}? This disconnects you and forgets the saved session on this
                  computer. Your account stays on the server.
                </p>
                <button
                  className="button secondary"
                  disabled={busy}
                  onClick={() => setRemoving(null)}
                >
                  Keep server
                </button>
                <button
                  className="button primary danger"
                  disabled={busy}
                  onClick={() => void change(() => removeServer(server.id))}
                >
                  Confirm removal
                </button>
              </div>
            )}
          </section>
        </>
      )}
      {error && (
        <p role="alert" className="form-error">
          {error}
        </p>
      )}
    </main>
  )
}
