import { useEffect, useState } from 'react'
import { ServerIdentity } from '../../components/ServerIdentity'
import type { SyncServer } from '../../lib/generated/SyncServer'
import type { SyncOptions } from '../../lib/generated/SyncOptions'
import { listSyncServers, chooseSyncServer, verifySyncCopy } from './syncServersApi'
import { friendlyError } from '../onboarding/validation'
import './sync-servers.css'

export function SyncServers({ onManageBackup }: { onManageBackup?: () => void }) {
  const [options, setOptions] = useState<SyncOptions | null>(null)
  const [confirm, setConfirm] = useState<SyncServer | null>(null)
  const [refresh, setRefresh] = useState(0)
  const [busy, setBusy] = useState(false)
  const [notice, setNotice] = useState('')
  const [error, setError] = useState('')
  const [missing, setMissing] = useState(false)
  useEffect(() => {
    let current = true
    void listSyncServers()
      .then((result) => {
        if (current) setOptions(result)
      })
      .catch((e) => {
        if (current) setError(friendlyError(e))
      })
    return () => {
      current = false
    }
  }, [refresh])

  function reload() {
    setOptions(null)
    setConfirm(null)
    setNotice('')
    setError('')
    setMissing(false)
    setRefresh((n) => n + 1)
  }
  async function choose(server: SyncServer) {
    setBusy(true)
    setError('')
    setNotice('')
    try {
      await chooseSyncServer(server.id)
      setConfirm(null)
      setOptions((current) => current && { ...current, chosen: server })
      setMissing(false)
      setNotice('Encrypted profile copy saved.')
    } catch (e) {
      setError(friendlyError(e))
    } finally {
      setBusy(false)
    }
  }
  async function checkCopy() {
    setBusy(true)
    setNotice('')
    setError('')
    try {
      await verifySyncCopy()
      setMissing(false)
      setNotice('Sideband opened and verified your encrypted copy.')
    } catch (e) {
      setMissing(!!e && typeof e === 'object' && 'code' in e && e.code === 'copy_missing')
      setError(friendlyError(e))
    } finally {
      setBusy(false)
    }
  }

  const chosen = options?.chosen
  const available = options?.available ?? []
  const online = chosen && available.find((server) => server.id === chosen.id)
  return (
    <section className="profile-sync" aria-label="Sync server">
      {confirm ? (
        <>
          <h3>Use {confirm.name} for sync?</h3>
          <ServerIdentity {...confirm} />
          <p>
            Sideband will save an encrypted copy of your profile here. Only your profile can open
            it.
          </p>
          <div className="profile-sync-actions">
            <button className="button primary" disabled={busy} onClick={() => void choose(confirm)}>
              {busy ? 'Saving…' : 'Use this server'}
            </button>
            <button
              className="button secondary"
              disabled={busy}
              onClick={() => {
                setConfirm(null)
                setError('')
              }}
            >
              Cancel
            </button>
          </div>
        </>
      ) : chosen ? (
        <>
          <h3>Your sync server</h3>
          <ServerIdentity {...(online || chosen)} hasPublicIcon={online?.hasPublicIcon ?? false} />
          {online ? (
            <p>Your encrypted profile copy is stored here.</p>
          ) : (
            <p>This server is unavailable. Your profile is still on this device.</p>
          )}
          <div className="profile-sync-actions">
            {missing && online && (
              <button
                className="button primary"
                disabled={busy}
                onClick={() => void choose(chosen)}
              >
                Save a new copy
              </button>
            )}
            <button
              className="button secondary"
              disabled={busy}
              onClick={() => (online ? void checkCopy() : reload())}
            >
              {busy ? 'Checking…' : online ? 'Verify saved copy' : 'Check again'}
            </button>
          </div>
        </>
      ) : (
        <>
          <h3>
            {options && !available.length ? 'No sync servers available' : 'Choose a sync server'}
          </h3>
          {!options && !error && <p role="status">Checking your servers…</p>}
          {!!available.length && (
            <>
              <p>Choose a community you belong to for your encrypted profile copy.</p>
              <ul>
                {available.map((server) => (
                  <li key={server.id}>
                    <ServerIdentity {...server} />
                    <button
                      className="button secondary"
                      aria-label={`Choose ${server.name}`}
                      onClick={() => {
                        setConfirm(server)
                        setError('')
                      }}
                    >
                      Choose
                    </button>
                  </li>
                ))}
              </ul>
            </>
          )}
          {options && !available.length && (
            <p>
              Keep using your profile on this device. Ask a community owner to enable profile sync
              or join a server that offers it.
            </p>
          )}
          {!!options?.unchecked && (
            <p>
              Some joined servers could not be checked. Reconnect to check whether they offer sync.
            </p>
          )}
          {options && !available.length && onManageBackup && (
            <button className="button secondary" onClick={onManageBackup}>
              Manage recovery backup
            </button>
          )}
          {(options || error) && (
            <button className="button secondary" onClick={reload}>
              Check again
            </button>
          )}
        </>
      )}
      {notice && <p role="status">{notice}</p>}
      {error && <p role="alert">{error}</p>}
    </section>
  )
}
