import { useCallback, useEffect, useRef, useState } from 'react'
import { Download } from 'lucide-react'
import { invokeBridge } from '../../lib/tauri'
import { errorText } from '../chat/api'
import '../admin/admin.css'
import './updates.css'

type Status = { enabled: boolean; version: string | null; notes: string | null }
export const CHECK_INTERVAL = 6 * 60 * 60 * 1000
export function UpdateControl() {
  const [status, setStatus] = useState<Status | null>(null)
  const [open, setOpen] = useState(false)
  const [busy, setBusy] = useState<'check' | 'install' | null>(null)
  const [error, setError] = useState('')
  const locked = useRef(false)
  const alive = useRef(false)
  const dialog = useRef<HTMLDialogElement>(null)
  const check = useCallback(async () => {
    if (locked.current) return
    locked.current = true
    setBusy('check')
    setError('')
    try {
      const next = await invokeBridge<Status>('check_update')
      if (alive.current) setStatus(next)
    } catch (e) {
      if (alive.current) setError(errorText(e))
    } finally {
      locked.current = false
      if (alive.current) setBusy(null)
    }
  }, [])
  useEffect(() => {
    alive.current = true
    const startup = setTimeout(() => void check(), 0)
    const timer = setInterval(() => void check(), CHECK_INTERVAL)
    return () => {
      alive.current = false
      clearTimeout(startup)
      clearInterval(timer)
    }
  }, [check])
  useEffect(() => {
    if (!open) return
    const previous = document.activeElement as HTMLElement | null
    const element = dialog.current
    element?.showModal()
    return () => {
      element?.close()
      previous?.focus()
    }
  }, [open])
  const install = async () => {
    if (locked.current || !status?.version) return
    locked.current = true
    setBusy('install')
    setError('')
    try {
      await invokeBridge('install_update')
    } catch (e) {
      if (alive.current) setError(errorText(e))
    } finally {
      locked.current = false
      if (alive.current) setBusy(null)
    }
  }
  return (
    <>
      <button
        className="server-button"
        aria-label={status?.version ? `Update available: ${status.version}` : 'App updates'}
        title={status?.version ? `Update available: ${status.version}` : 'App updates'}
        onClick={() => setOpen(true)}
      >
        <Download size={20} />
        {status?.version && <span aria-hidden="true">•</span>}
      </button>
      {open && (
        <dialog
          ref={dialog}
          className="admin-dialog update-dialog"
          aria-labelledby="update-title"
          onCancel={(e) => {
            e.preventDefault()
            if (busy !== 'install') setOpen(false)
          }}
        >
          <header>
            <h2 id="update-title">App updates</h2>
            <button
              className="button secondary"
              disabled={busy === 'install'}
              onClick={() => setOpen(false)}
            >
              Close
            </button>
          </header>
          {status?.enabled === false ? (
            <p>
              This build uses manual updates. Download and install a newer installer. On Linux,
              automatic updates require the AppImage package.
            </p>
          ) : (
            <>
              <p>
                {status?.version
                  ? `Sideband ${status.version} is available.`
                  : status
                    ? 'You’re up to date.'
                    : 'Checking for updates…'}
              </p>
              {status?.notes && (
                <pre style={{ whiteSpace: 'pre-wrap', overflowWrap: 'anywhere' }}>
                  {status.notes}
                </pre>
              )}
              {status?.version && (
                <>
                  <p>
                    Save your work and leave voice chat first. Updating will close and restart
                    Sideband.
                  </p>
                  <button
                    className="button primary"
                    disabled={!!busy}
                    onClick={() => void install()}
                  >
                    Install and restart
                  </button>
                </>
              )}
            </>
          )}
          {busy && (
            <p role="status">
              {busy === 'install'
                ? 'Downloading, verifying and installing…'
                : 'Checking for updates…'}
            </p>
          )}
          {error && <p role="alert">{error}</p>}
          <button className="button secondary" disabled={!!busy} onClick={() => void check()}>
            Check for updates
          </button>
        </dialog>
      )}
    </>
  )
}
