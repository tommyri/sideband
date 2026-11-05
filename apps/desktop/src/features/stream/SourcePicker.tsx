import { useEffect, useRef, useState } from 'react'
import * as api from './api'
import './stream.css'

function Preview({ source }: { source: api.Source }) {
  const canvas = useRef<HTMLCanvasElement>(null)
  const [error, setError] = useState('')
  useEffect(() => {
    let live = true
    void api
      .preview(source.id)
      .then((p) => {
        if (!live || !canvas.current) return
        canvas.current.width = p.width
        canvas.current.height = p.height
        canvas.current
          .getContext('2d')
          ?.putImageData(new ImageData(new Uint8ClampedArray(p.rgba), p.width, p.height), 0, 0)
      })
      .catch((e) => {
        if (live) setError(api.message(e))
      })
    return () => {
      live = false
    }
  }, [source.id])
  return (
    <div className="stream-preview">
      {error ? (
        <p role="status">Preview unavailable: {error}</p>
      ) : (
        <canvas ref={canvas} aria-label={`Preview of ${source.label}`} />
      )}
    </div>
  )
}
export function SourcePicker({
  server,
  channel,
  allowed,
  close,
}: {
  server: string
  channel: string
  allowed: boolean
  close: () => void
}) {
  const dialog = useRef<HTMLDialogElement>(null)
  const [items, setItems] = useState<api.Source[]>([])
  const [selected, setSelected] = useState('')
  const [preset, setPreset] = useState('1080p30')
  const [cursor, setCursor] = useState(true)
  const [loading, setLoading] = useState(true)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [revision, setRevision] = useState(0)
  useEffect(() => {
    dialog.current?.showModal()
    const node = dialog.current
    return () => node?.close()
  }, [])
  useEffect(() => {
    let live = true
    void api
      .sources()
      .then((items) => {
        if (live) setItems(items)
      })
      .catch((e) => {
        if (live) setError(api.message(e))
      })
      .finally(() => {
        if (live) setLoading(false)
      })
    return () => {
      live = false
    }
  }, [revision])
  const source = items.find((s) => s.id === selected)
  async function share() {
    if (!allowed || !source || busy) return
    setBusy(true)
    setError('')
    try {
      await api.start(server, channel, source.id, preset, cursor)
      close()
    } catch (e) {
      setError(api.message(e))
      setBusy(false)
    }
  }
  return (
    <dialog
      ref={dialog}
      className="stream-picker"
      aria-labelledby="stream-title"
      onCancel={(e) => {
        e.preventDefault()
        if (!busy) close()
      }}
    >
      <header>
        <div>
          <p className="eyebrow">SCREEN SHARE</p>
          <h2 id="stream-title">Choose what to share</h2>
        </div>
        <button onClick={close} disabled={busy} aria-label="Close screen picker">
          ×
        </button>
      </header>
      <p>
        Choose a display or window. Only video is shared; your microphone stays controlled by voice
        settings.
      </p>
      <p className="stream-permission">
        macOS may ask for Screen Recording access. Allow Sideband in System Settings → Privacy &
        Security, then reopen the picker. On Wayland, choose and approve a source in the system
        dialog when sharing starts.
      </p>
      {error && (
        <p role="alert" className="form-error">
          {error}
        </p>
      )}
      {!allowed && <p role="alert">Sharing is unavailable. Join voice with STREAM permission.</p>}
      <button
        disabled={loading || busy}
        onClick={() => {
          setLoading(true)
          setError('')
          setSelected('')
          setRevision((r) => r + 1)
        }}
      >
        Refresh sources
      </button>
      {loading ? (
        <p role="status">Loading displays and windows…</p>
      ) : items.length === 0 ? (
        <p>No sources available. Check screen-recording permission and refresh.</p>
      ) : (
        <fieldset disabled={busy} className="stream-sources">
          <legend>Source</legend>
          {items.map((item) => (
            <label key={item.id}>
              <input
                type="radio"
                name="source"
                checked={selected === item.id}
                onChange={() => setSelected(item.id)}
              />
              <span>
                {item.label}
                <small>{item.kind === 'portal' ? 'System picker' : item.kind}</small>
              </span>
            </label>
          ))}
        </fieldset>
      )}
      {source && source.kind !== 'portal' && (
        <Preview key={`${revision}:${source.id}`} source={source} />
      )}
      <label className="stream-option">
        Quality
        <select value={preset} onChange={(e) => setPreset(e.target.value)} disabled={busy}>
          <option value="1080p60">1080p · 60 FPS</option>
          <option value="1080p30">1080p · 30 FPS</option>
          <option value="720p30">720p · 30 FPS</option>
        </select>
      </label>
      <label className="stream-option">
        <input
          type="checkbox"
          checked={cursor}
          onChange={(e) => setCursor(e.target.checked)}
          disabled={busy}
        />
        Show cursor
      </label>
      <footer>
        <button className="button secondary" onClick={close} disabled={busy}>
          Cancel
        </button>
        <button
          className="button primary"
          disabled={!allowed || !source || busy || loading}
          onClick={() => void share()}
        >
          {busy ? 'Starting…' : 'Share selected source'}
        </button>
      </footer>
    </dialog>
  )
}
