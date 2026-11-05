import { useEffect, useRef, useState } from 'react'
import { Bell, X } from 'lucide-react'
import { invokeBridge } from '../../lib/tauri'
import { useAppStore } from '../../store/app-store'
import { useVoiceStore } from '../voice/store'
import { defaults, notificationLevel, type Preferences, type Level } from './model'

export function NotificationSettings({ close }: { close: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null)
  const [draft, setDraft] = useState<Preferences>(defaults)
  const [loaded, setLoaded] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [status, setStatus] = useState('')
  const server = useAppStore((s) => s.selectedServerId)
  const label = useAppStore((s) => s.servers.find((v) => v.id === server)?.label)
  const view = useVoiceStore((s) => (server ? s.views[server] : undefined))
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null
    dialog.current?.showModal()
    let live = true
    void invokeBridge<Preferences>('notification_preferences')
      .then((p) => {
        if (live) {
          setDraft(p)
          setLoaded(true)
        }
      })
      .catch((e) => {
        if (live)
          setError(e && typeof e === 'object' && 'message' in e ? String(e.message) : String(e))
      })
    return () => {
      live = false
      previous?.focus()
    }
  }, [])
  async function save() {
    setBusy(true)
    setError('')
    setStatus('')
    try {
      await invokeBridge('save_notification_preferences', { preferences: draft })
      setStatus('Notification settings saved.')
    } catch (e) {
      setError(e && typeof e === 'object' && 'message' in e ? String(e.message) : String(e))
    } finally {
      setBusy(false)
    }
  }
  async function test() {
    setBusy(true)
    setError('')
    try {
      await invokeBridge('save_notification_preferences', { preferences: draft })
      await invokeBridge('test_notification')
      setStatus(
        'Test sent. If it does not appear, check your operating system notification settings.',
      )
    } catch (e) {
      setError(e && typeof e === 'object' && 'message' in e ? String(e.message) : String(e))
    } finally {
      setBusy(false)
    }
  }
  return (
    <dialog
      ref={dialog}
      className="audio-settings notification-settings"
      aria-labelledby="notification-title"
      onCancel={close}
    >
      <header>
        <div>
          <span className="eyebrow">PREFERENCES</span>
          <h2 id="notification-title">
            <Bell size={20} /> Notifications
          </h2>
        </div>
        <button className="icon-button" onClick={close} aria-label="Close notifications">
          <X size={20} />
        </button>
      </header>
      <div className="notification-body">
        <p>
          Choose when Sideband gets your attention. Notification previews are private by default.
        </p>
        {error && (
          <p role="alert" className="form-error">
            {error}
          </p>
        )}
        <fieldset disabled={!loaded || busy}>
          <label>
            <input
              type="checkbox"
              checked={draft.dnd}
              onChange={(e) => setDraft({ ...draft, dnd: e.target.checked })}
            />{' '}
            Do not disturb
          </label>
          <p>
            Silence sounds and desktop notifications. Your presence is shown as Do not disturb on
            connected servers.
          </p>
          <label>
            <input
              type="checkbox"
              checked={draft.desktop}
              onChange={(e) => setDraft({ ...draft, desktop: e.target.checked })}
            />{' '}
            Desktop notifications when Sideband is in the background
          </label>
          <label>
            <input
              type="checkbox"
              checked={draft.previews}
              onChange={(e) => setDraft({ ...draft, previews: e.target.checked })}
            />{' '}
            Show message text in desktop notifications
          </label>
          <button
            className="secondary-button"
            onClick={() => void test()}
            disabled={!draft.desktop || draft.dnd}
          >
            Save and send test notification
          </button>
          <p>
            Operating system settings can block notifications. Sound volume and output follow Audio
            settings, including outside voice calls.
          </p>
          {server && (
            <>
              <h3>{label ?? 'Selected server'}</h3>
              <label>
                <input
                  type="checkbox"
                  checked={draft.mutedServers.includes(server)}
                  onChange={(e) =>
                    setDraft({
                      ...draft,
                      mutedServers: e.target.checked
                        ? [...draft.mutedServers, server]
                        : draft.mutedServers.filter((id) => id !== server),
                    })
                  }
                />{' '}
                Mute this server
              </label>
              <p>
                Channels default to mentions; direct conversations notify for incoming messages. Do
                not disturb and server mute override every channel.
              </p>
              {[
                ...(view?.channels.filter((c) => c.kind === 2) ?? []),
                ...(view?.directMessages ?? []),
              ].map((c) => (
                <label className="notification-channel" key={c.id}>
                  <span>
                    {c.kind === 2 ? '#' : ''}
                    {c.name}
                    <small>Effective: {notificationLevel(draft, server, c.id)}</small>
                  </span>
                  <select
                    aria-label={`Notifications for ${c.name}`}
                    value={draft.channels[`${server}/${c.id}`] ?? 'mentions'}
                    onChange={(e) =>
                      setDraft({
                        ...draft,
                        channels: {
                          ...draft.channels,
                          [`${server}/${c.id}`]: e.target.value as Level,
                        },
                      })
                    }
                  >
                    <option value="all">All messages</option>
                    <option value="mentions">Mentions only</option>
                    <option value="none">None</option>
                  </select>
                </label>
              ))}
              {!view && <p>Connect to this server to configure individual channels.</p>}
            </>
          )}
        </fieldset>
        <p role="status">{status}</p>
      </div>
      <footer>
        <button className="secondary-button" onClick={close}>
          Close
        </button>
        <button className="primary-button" disabled={!loaded || busy} onClick={() => void save()}>
          {busy ? 'Saving…' : 'Save settings'}
        </button>
      </footer>
    </dialog>
  )
}
