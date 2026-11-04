import { validServerSettings } from './serverSettingsModel'
import { useEffect, useRef, useState } from 'react'
import type { ServerSettingsView } from '../../lib/generated/ServerSettingsView'
import type { ServerSettingsFields } from '../../lib/generated/ServerSettingsFields'
import { invokeBridge } from '../../lib/tauri'
import { errorText } from '../chat/api'
import './admin.css'
type Props = { server: string; connected: boolean; permitted: boolean; close: () => void }
type Icon = { data: string; preview: string }
const read = (serverId: string) =>
  invokeBridge<ServerSettingsView>('get_server_settings', { serverId })
export function ServerSettings(props: Props) {
  return <SettingsPanel key={`${props.server}:${props.connected}:${props.permitted}`} {...props} />
}
function SettingsPanel({ server, connected, permitted, close }: Props) {
  const dialog = useRef<HTMLDialogElement>(null)
  const alive = useRef(false)
  const [snapshot, setSnapshot] = useState<ServerSettingsView | null>(null)
  const [fields, setFields] = useState<ServerSettingsFields | null>(null)
  const [icon, setIcon] = useState<Icon | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [notice, setNotice] = useState('')
  const [discard, setDiscard] = useState<'close' | 'reload' | null>(null)
  const allowed = connected && permitted
  const dirty = !!icon || JSON.stringify(fields) !== JSON.stringify(snapshot?.fields ?? null)
  const apply = (next: ServerSettingsView) => {
    setSnapshot(next)
    setFields(next.fields)
    setIcon(null)
  }
  useEffect(() => {
    alive.current = true
    const previous = document.activeElement as HTMLElement | null
    const element = dialog.current
    element?.showModal()
    let cancelled = false
    if (allowed) {
      void read(server)
        .then((next) => {
          if (!cancelled) apply(next)
        })
        .catch((e) => {
          if (!cancelled) setError(errorText(e))
        })
    }
    return () => {
      cancelled = true
      alive.current = false
      element?.close()
      previous?.focus()
    }
  }, [server, allowed])
  const reload = async () => {
    if (!allowed || busy) return
    setBusy(true)
    setError('')
    setNotice('')
    try {
      const next = await read(server)
      if (alive.current) {
        apply(next)
        setDiscard(null)
      }
    } catch (e) {
      if (alive.current) setError(errorText(e))
    } finally {
      if (alive.current) setBusy(false)
    }
  }
  const requestClose = () => {
    if (busy) return
    if (dirty) setDiscard('close')
    else close()
  }
  const chooseIcon = async (file: File | undefined) => {
    if (!file || busy || !allowed) return
    setError('')
    setNotice('')
    if (
      !['image/png', 'image/jpeg', 'image/webp'].includes(file.type) ||
      file.size === 0 ||
      file.size > 2 * 1024 * 1024
    ) {
      setError('Choose a PNG, JPEG or WebP image up to 2 MiB.')
      return
    }
    setBusy(true)
    try {
      const preview = await new Promise<string>((resolve, reject) => {
        const reader = new FileReader()
        reader.onload = () => resolve(String(reader.result))
        reader.onerror = () => reject(new Error('Could not read the image.'))
        reader.readAsDataURL(file)
      })
      if (alive.current) setIcon({ preview, data: preview.slice(preview.indexOf(',') + 1) })
    } catch (e) {
      if (alive.current) setError(errorText(e))
    } finally {
      if (alive.current) setBusy(false)
    }
  }
  const save = async () => {
    if (!allowed || busy || !fields) return
    const problem = validServerSettings(fields)
    if (problem) {
      setError(problem)
      return
    }
    setBusy(true)
    setError('')
    setNotice('')
    try {
      let next = { ...fields, name: fields.name.trim() }
      if (icon) {
        const iconId = await invokeBridge<string>('upload_server_icon', {
          serverId: server,
          data: icon.data,
        })
        if (!alive.current) return
        next = { ...next, iconId }
        setFields(next)
        setIcon(null)
      }
      const result = await invokeBridge<ServerSettingsView>('save_server_settings', {
        serverId: server,
        fields: next,
      })
      if (alive.current) {
        apply(result)
        setNotice('Server settings saved.')
        setDiscard(null)
      }
    } catch (e) {
      if (alive.current) setError(errorText(e))
    } finally {
      if (alive.current) setBusy(false)
    }
  }
  const preview =
    icon?.preview ?? (fields?.iconId === snapshot?.fields.iconId ? snapshot?.icon : null)
  return (
    <dialog
      ref={dialog}
      className="admin-dialog server-settings"
      aria-labelledby="server-settings-title"
      onCancel={(e) => {
        e.preventDefault()
        requestClose()
      }}
    >
      <header>
        <h2 id="server-settings-title">Server settings</h2>
        <button className="button secondary" disabled={busy} onClick={requestClose}>
          Close
        </button>
      </header>
      {!allowed && <p role="alert">Connect with Manage server permission to edit settings.</p>}
      {error && <p role="alert">{error}</p>}
      {notice && <p role="status">{notice}</p>}
      {discard && (
        <section
          className="invite-confirm"
          role="alertdialog"
          aria-label="Discard unsaved settings"
        >
          <p>
            Discard your unsaved changes
            {discard === 'reload' ? ' and load the latest server settings' : ''}?
          </p>
          <button
            className="button primary danger"
            disabled={busy}
            onClick={() => {
              if (discard === 'close') close()
              else void reload()
            }}
          >
            Discard changes
          </button>
          <button className="button secondary" disabled={busy} onClick={() => setDiscard(null)}>
            Keep editing
          </button>
        </section>
      )}
      {allowed && !fields && !error && <p role="status">Loading server settings…</p>}
      {allowed && (
        <button
          className="button secondary"
          disabled={busy || (!fields && !error)}
          onClick={() => (dirty ? setDiscard('reload') : void reload())}
        >
          Reload settings
        </button>
      )}
      {allowed && fields && (
        <form
          onSubmit={(e) => {
            e.preventDefault()
            void save()
          }}
        >
          <fieldset disabled={busy || !!discard} className="server-settings-fields">
            <label>
              Server name
              <input
                required
                value={fields.name}
                onChange={(e) => setFields({ ...fields, name: e.target.value })}
              />
            </label>
            <label>
              Description
              <textarea
                rows={4}
                value={fields.description}
                onChange={(e) => setFields({ ...fields, description: e.target.value })}
              />
              <small>{[...fields.description].length} / 2048 characters</small>
            </label>
            <section>
              <h3>Server icon</h3>
              <div className="server-icon-row">
                {preview ? (
                  <img className="server-icon-preview" src={preview} alt="Server icon preview" />
                ) : (
                  <span className="server-icon-placeholder">
                    {fields.iconId ? 'Saved icon' : 'No icon'}
                  </span>
                )}
                <label>
                  Choose icon
                  <input
                    type="file"
                    accept="image/png,image/jpeg,image/webp"
                    onChange={(e) => {
                      void chooseIcon(e.target.files?.[0])
                      e.target.value = ''
                    }}
                  />
                  <small>
                    PNG, JPEG or WebP. Up to 2 MiB and 4096 × 4096 pixels. Applied when you save.
                  </small>
                </label>
                <button
                  type="button"
                  className="button secondary"
                  disabled={!icon && !fields.iconId}
                  onClick={() => {
                    setIcon(null)
                    setFields({ ...fields, iconId: null })
                  }}
                >
                  Remove icon
                </button>
              </div>
              {snapshot?.iconWarning && <p role="note">{snapshot.iconWarning}</p>}
            </section>
            <label>
              Registration
              <select
                value={fields.registrationMode}
                onChange={(e) => setFields({ ...fields, registrationMode: e.target.value })}
              >
                <option value="invite">Invite only</option>
                <option value="closed">Closed</option>
              </select>
              <small>
                {fields.registrationMode === 'closed'
                  ? 'New accounts cannot register, even with an invite. Existing members can still sign in.'
                  : 'New members need a valid invite. Anyone with an invite can register until it expires or is revoked.'}
              </small>
            </label>
            {fields.registrationMode === 'closed' &&
              snapshot?.fields.registrationMode !== 'closed' && (
                <p role="note">
                  Saving will pause new registrations immediately. Existing invites are kept and
                  work again if you reopen registration.
                </p>
              )}
            <button className="button primary" disabled={!dirty}>
              {busy ? 'Saving…' : 'Save settings'}
            </button>
          </fieldset>
        </form>
      )}
      {snapshot && (
        <section className="server-identity">
          <h3>Server identity</h3>
          <dl>
            <dt>Version</dt>
            <dd>{snapshot.version || 'Unavailable'}</dd>
            <dt>Approved TLS fingerprint</dt>
            <dd>
              <code>{snapshot.fingerprint || 'Unavailable'}</code>
            </dd>
          </dl>
          <p>
            This is the certificate identity approved on this computer. Names and icons do not
            change that trust.
          </p>
          <small>
            Name changes appear in new connections and refreshed server details. Your local server
            label is unchanged.
          </small>
        </section>
      )}
    </dialog>
  )
}
