import { useEffect, useRef, useState } from 'react'
import { invokeBridge } from '../../../lib/tauri'
import {
  conflicts,
  empty,
  refreshShortcuts,
  useShortcuts,
  type Binding,
  type Preferences,
  type Snapshot,
} from './store'
const actions = ['ptt', 'mute', 'deafen'] as const
const titles = ['Push-to-talk', 'Toggle mute', 'Toggle deafen'] as const
function message(e: unknown) {
  return e && typeof e === 'object' && 'message' in e ? String(e.message) : String(e)
}
export function ShortcutSettings({ close }: { close: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null)
  const current = useShortcuts((s) => s.snapshot)
  const [draft, setDraft] = useState<Preferences>(empty)
  const [labels, setLabels] = useState(['Not bound', 'Not bound', 'Not bound'])
  const [busy, setBusy] = useState(false)
  const [capturing, setCapturing] = useState(false)
  const [error, setError] = useState('')
  const [saved, setSaved] = useState(false)
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null
    dialog.current?.showModal()
    let live = true
    void refreshShortcuts()
      .then((s) => {
        if (live) {
          setDraft(s.preferences)
          setLabels(s.labels)
        }
      })
      .catch((e) => {
        if (live) setError(message(e))
      })
    const timer = setInterval(() => void refreshShortcuts().catch(() => {}), 1000)
    return () => {
      live = false
      clearInterval(timer)
      void invokeBridge('cancel_shortcut_capture').catch(() => {})
      previous?.focus()
    }
  }, [])
  async function capture(index: number) {
    setBusy(true)
    setCapturing(true)
    setError('')
    setSaved(false)
    try {
      const result = await invokeBridge<{ binding: Binding; label: string }>('capture_shortcut')
      setDraft((d) => ({ ...d, [actions[index]!]: result.binding }))
      setLabels((l) => l.map((v, i) => (i === index ? result.label : v)))
    } catch (e) {
      setError(message(e))
    } finally {
      setBusy(false)
      setCapturing(false)
      void refreshShortcuts().catch(() => {})
    }
  }
  async function apply(preferences: Preferences = draft) {
    setBusy(true)
    setError('')
    setSaved(false)
    try {
      const snapshot = await invokeBridge<Snapshot>('save_shortcuts', { preferences })
      useShortcuts.setState({ snapshot })
      setLabels(snapshot.labels)
      setSaved(true)
    } catch (e) {
      setError(message(e))
    } finally {
      setBusy(false)
    }
  }
  return (
    <dialog
      ref={dialog}
      className="audio-settings shortcut-settings"
      aria-labelledby="shortcut-title"
      onCancel={(e) => {
        e.preventDefault()
        e.stopPropagation()
        close()
      }}
      onKeyDown={(e) => {
        e.stopPropagation()
        if (e.key !== 'Tab') return
        const nodes = Array.from(
          e.currentTarget.querySelectorAll<HTMLElement>('button,input'),
        ).filter((n) => !n.matches(':disabled') && n.getClientRects().length > 0)
        if (!nodes.length) return
        e.preventDefault()
        const i = nodes.indexOf(document.activeElement as HTMLElement)
        nodes[(i + (e.shiftKey ? -1 : 1) + nodes.length) % nodes.length]?.focus()
      }}
    >
      <header>
        <div>
          <p className="eyebrow">WHILE YOU PLAY</p>
          <h2 id="shortcut-title">Global shortcuts</h2>
        </div>
        <button aria-label="Close global shortcuts" onClick={close}>
          ×
        </button>
      </header>
      <div className="audio-body">
        <p>
          Shortcuts work outside Sideband when your platform permits it. Nothing listens until you
          save a binding or enable system-managed shortcuts. Capture may request input permission.
        </p>
        {error && (
          <p role="alert" className="form-error">
            {error}
          </p>
        )}
        {current?.error && (
          <p role="alert" className="form-error">
            {current.error}
          </p>
        )}
        {saved && (
          <p role="status">
            Saved.{' '}
            {current?.running
              ? 'Global shortcuts are listening.'
              : current?.suspended
                ? 'Listening is suspended.'
                : current?.preferences.ptt ||
                    current?.preferences.mute ||
                    current?.preferences.deafen ||
                    current?.preferences.portal
                  ? 'The listener is stopped; check the platform notice below.'
                  : 'No global bindings configured; not listening.'}
          </p>
        )}
        <fieldset disabled={busy || !current}>
          <legend>Bindings</legend>
          {current?.systemBinding ? (
            <>
              <label>
                <input
                  type="checkbox"
                  checked={draft.portal}
                  onChange={(e) => {
                    setDraft((d) => ({
                      ...d,
                      portal: e.target.checked,
                      ptt: null,
                      mute: null,
                      deafen: null,
                    }))
                    setSaved(false)
                  }}
                />
                Enable system-managed PTT, mute and deafen
              </label>
              <p>
                Your compositor chooses the keys. Mouse capture is unavailable. Save to register the
                actions, then configure them in system settings.
              </p>
            </>
          ) : (
            actions.map((action, i) => (
              <div className="shortcut-row" key={action}>
                <strong>{titles[i]}</strong>
                <span>{labels[i]}</span>
                <button
                  disabled={!current?.captureSupported}
                  onClick={(e) => {
                    e.currentTarget.focus()
                    void capture(i)
                  }}
                >
                  Capture {titles[i]!.toLowerCase()}
                </button>
                <button
                  aria-label={`Clear ${titles[i]!.toLowerCase()}`}
                  onClick={() => {
                    setDraft((d) => ({ ...d, [action]: null }))
                    setLabels((l) => l.map((v, j) => (j === i ? 'Not bound' : v)))
                    setSaved(false)
                  }}
                >
                  Clear
                </button>
              </div>
            ))
          )}
          {current?.linux && (
            <>
              <label>
                <input
                  type="checkbox"
                  checked={draft.allowEvdev}
                  onChange={(e) => {
                    setDraft((d) => ({ ...d, allowEvdev: e.target.checked }))
                    setSaved(false)
                  }}
                />
                Allow evdev fallback when the Wayland portal is unavailable
              </label>
              <p>
                evdev can read all keys in every application and requires access to Linux input
                devices. This app does not change system permissions. Save to select this backend,
                then refresh capabilities.
              </p>
            </>
          )}
        </fieldset>
        {capturing && (
          <div role="status">
            <p>
              Press and release a key{current?.mouse ? ' or mouse button' : ''}. Global actions are
              suspended during capture. Capture times out after 15 seconds.
            </p>
            <button
              onClick={() =>
                void invokeBridge('cancel_shortcut_capture').catch((e) => setError(message(e)))
              }
            >
              Cancel capture
            </button>
          </div>
        )}
        {conflicts(draft) && (
          <p role="alert">
            Two shortcuts share a trigger and could fire together. Choose different keys or mouse
            buttons.
          </p>
        )}
        <fieldset>
          <legend>Platform status</legend>
          <p>
            {current?.running
              ? 'Listening'
              : current?.capturing
                ? 'Capturing'
                : current?.suspended
                  ? 'Suspended for private input'
                  : 'Not listening'}
          </p>
          {current?.permission && (
            <p>
              Requires {current.permission}. Grant permission only if you want global input
              monitoring.
            </p>
          )}
          {current?.limitations.map((line, i) => (
            <p key={i}>{line}</p>
          ))}
          {(current?.permission === 'Input Monitoring' || current?.systemBinding) && (
            <button
              disabled={busy}
              onClick={() =>
                void invokeBridge('configure_system_shortcuts').catch((e) => setError(message(e)))
              }
            >
              Open system shortcut permissions/settings
            </button>
          )}
          <button disabled={busy} onClick={() => void apply(current?.preferences)}>
            Retry saved bindings
          </button>
        </fieldset>
        <fieldset>
          <legend>Recent input events · local only</legend>
          <p>
            Only shortcut actions and listener state are shown. No typed text, telemetry, or disk
            log.
          </p>
          <ol>
            {current?.events.map((event, i) => (
              <li key={i}>{event}</li>
            ))}
          </ol>
          {!current?.events.length && <p>No events yet.</p>}
        </fieldset>
        <footer>
          <button disabled={busy} onClick={close}>
            Cancel
          </button>
          <button
            className="button primary"
            disabled={busy || !current || conflicts(draft)}
            onClick={() => void apply()}
          >
            Save shortcuts
          </button>
        </footer>
      </div>
    </dialog>
  )
}
