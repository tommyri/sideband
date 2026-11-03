import { useEffect, useRef, useState } from 'react'
import { invokeBridge } from '../../lib/tauri'
import { useAppStore } from '../../store/app-store'
import { useVoiceStore } from '../voice/store'
import { environment } from '../settings/audio/store'
import { refreshShortcuts, type Snapshot as InputSnapshot } from '../settings/shortcuts/store'
import { context, missing, type Json } from '../testing/report'
import schema from '../testing/report.schema.json'
import { graphPaths, metrics, report, retain, type Sample, type Snapshot } from './model'
export function DiagnosticsPanel() {
  const active = useVoiceStore((s) => s.active?.serverId)
  const selected = useAppStore((s) => s.selectedServerId)
  const server = active ?? selected
  const [open, setOpen] = useState(false)
  useEffect(() => {
    const show = () => setOpen(true)
    const key = (e: KeyboardEvent) => {
      if (e.ctrlKey && e.shiftKey && !e.altKey && !e.metaKey && e.code === 'KeyD' && !e.repeat) {
        e.preventDefault()
        show()
      }
    }
    window.addEventListener('ss:open-diagnostics', show)
    window.addEventListener('keydown', key)
    return () => {
      window.removeEventListener('ss:open-diagnostics', show)
      window.removeEventListener('keydown', key)
    }
  }, [])
  return (
    <>
      {open && (
        <Panel key={server ?? 'none'} server={server ?? undefined} close={() => setOpen(false)} />
      )}
    </>
  )
}
function Panel({ close, server }: { close: () => void; server: string | undefined }) {
  const dialog = useRef<HTMLDialogElement>(null)
  const [samples, setSamples] = useState<Sample[]>([])
  const [build, setBuild] = useState<Json>(missing('Build identity pending'))
  const [settings, setSettings] = useState<Json>(missing('Settings pending'))
  const [input, setInput] = useState<InputSnapshot | null>(null)
  const [error, setError] = useState('')
  const [notes, setNotes] = useState('')
  const [bundle, setBundle] = useState<Json | null>(null)
  const [status, setStatus] = useState('')
  const [busy, setBusy] = useState(false)
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null
    dialog.current?.showModal()
    return () => previous?.focus()
  }, [])
  useEffect(() => {
    let canceled = false
    let timer: ReturnType<typeof setTimeout> | undefined
    void invokeBridge<Json>('test_build_identity')
      .then((v) => {
        if (!canceled) setBuild(v)
      })
      .catch(() => {
        if (!canceled) setBuild(missing('Build identity unavailable'))
      })
    async function poll() {
      try {
        const value = await invokeBridge<Snapshot>('diagnostics_snapshot', {
          serverId: server ?? null,
        })
        if (!canceled) {
          setSamples((old) => retain(old, { at: performance.now(), snapshot: value }))
          setError('')
        }
      } catch {
        if (!canceled) {
          setError('Diagnostics unavailable; reconnect or retry by reopening the panel.')
          setSamples([])
        }
      }
      if (!canceled) timer = setTimeout(() => void poll(), 500)
    }
    void Promise.all([environment(), refreshShortcuts()])
      .then(([a, i]) => {
        if (!canceled) {
          setSettings(context(a, i))
          setInput(i)
        }
      })
      .catch(() => {
        if (!canceled) setSettings(missing('Audio/input environment unavailable'))
      })
    void poll()
    const inputTimer = setInterval(() => {
      void refreshShortcuts()
        .then((v) => {
          if (!canceled) setInput(v)
        })
        .catch(() => undefined)
    }, 1000)
    return () => {
      canceled = true
      clearTimeout(timer)
      clearInterval(inputTimer)
    }
  }, [server])
  const current = samples.at(-1)?.snapshot
  const val = (value: number | undefined | null, unit = '') =>
    value === null || value === undefined
      ? 'Unavailable'
      : `${Number(value.toFixed(2))}${unit ? ' ' + unit : ''}`
  const unavailable = current?.unavailable ?? {}
  function prepare() {
    setBundle(
      report(
        build,
        {
          audio: settings,
          shortcuts: {
            running: input?.running ?? false,
            systemBinding: input?.systemBinding ?? false,
            backend: input?.backend ?? 'Unavailable',
            events: input?.events ?? [],
          },
        },
        samples,
        notes,
      ),
    )
    setStatus('')
  }
  async function save() {
    if (!bundle) return
    setBusy(true)
    setStatus('')
    const bytes = (s: string) => Array.from(new TextEncoder().encode(s))
    const r = bundle as Record<string, Json>
    try {
      const path = await invokeBridge<string>('export_test_report', {
        files: [
          { name: 'report.json', bytes: bytes(JSON.stringify(bundle, null, 2)) },
          { name: 'schema.json', bytes: bytes(JSON.stringify(schema, null, 2)) },
          { name: 'diagnostics.log', bytes: bytes(JSON.stringify(r.events)) },
          { name: 'notes.txt', bytes: bytes(String(r.notes ?? '')) },
        ],
        consent: false,
        includeRecording: false,
      })
      setStatus(`Saved locally: ${path}`)
    } catch {
      setStatus('Could not save the bundle. Check that Downloads is writable.')
    } finally {
      setBusy(false)
    }
  }
  async function copy() {
    if (!bundle) return
    try {
      await navigator.clipboard.writeText(JSON.stringify(bundle, null, 2))
      setStatus('Redacted diagnostics copied.')
    } catch {
      setStatus('Clipboard unavailable. Select the preview text or save the bundle.')
    }
  }
  return (
    <dialog
      ref={dialog}
      className="audio-settings diagnostics-panel"
      aria-labelledby="diagnostics-title"
      onCancel={(e) => {
        e.preventDefault()
        e.stopPropagation()
        close()
      }}
      onKeyDown={(e) => e.stopPropagation()}
    >
      <header>
        <h2 id="diagnostics-title">Voice diagnostics</h2>
        <button onClick={close}>Close diagnostics</button>
      </header>
      <p>
        Last 60 seconds · sampled about every 500 ms while open. Voice uses the same core counters
        as the CLI. No remote telemetry.
      </p>
      {error && <p role="alert">{error}</p>}
      {!current?.transport && <p>No live transport. Connect to a server to inspect its path.</p>}
      <section className="diagnostics-values" aria-label="Transport counters">
        <p>
          RTT <strong>{val(current?.transport?.rttMs, 'ms')}</strong>
        </p>
        <p>
          QUIC packets lost <strong>{val(current?.transport?.lostPackets)}</strong>
        </p>
        <p>
          QUIC packets sent <strong>{val(current?.transport?.sentPackets)}</strong>
        </p>
        <p>
          Congestion window{' '}
          <strong>{val(current?.transport?.congestionWindowBytes, 'bytes')}</strong>
        </p>
        <p>
          Path MTU (UDP payload) <strong>{val(current?.transport?.pathMtuBytes, 'bytes')}</strong>
        </p>
        <p>
          App datagram ceiling <strong>{val(current?.transport?.maxDatagramBytes, 'bytes')}</strong>
        </p>
      </section>
      <p>
        Loss counters are cumulative on the current path/track, not percentages. They reset on
        reconnect/reopen. RTT and buffer estimates are not mouth-to-ear latency.
      </p>
      <div className="diagnostics-graphs">
        {metrics.map((metric) => {
          const graph = graphPaths(samples, metric.read, samples.at(-1)?.at ?? 0)
          return (
            <figure key={metric.name}>
              <figcaption>
                {metric.name} · {metric.unit} · scale 0–{graph.maximum.toFixed(1)}
              </figcaption>
              <svg viewBox="0 0 600 110" role="img" aria-label={`${metric.name}, last 60 seconds`}>
                <path d="M0,100H600" className="graph-baseline" />
                {graph.paths.map((d, i) => (
                  <path key={i} d={d} className="graph-line" />
                ))}
              </svg>
              <small>−60 s → now · gaps mean unavailable/delayed samples</small>
            </figure>
          )
        })}
      </div>
      <h3>Audio & processing</h3>
      {!current?.voice && (
        <p>Join voice to see audio counters. No sample is substituted with zero.</p>
      )}
      <div className="diagnostics-values">
        <p>
          Actual backend{' '}
          <strong>{current?.details?.backend ?? 'Unavailable (no hardware stream)'}</strong>
        </p>
        <p>
          Input / output rate{' '}
          <strong>
            {val(current?.details?.inputRateHz, 'Hz')} / {val(current?.details?.outputRateHz, 'Hz')}
          </strong>
        </p>
        <p>
          Encoder <strong>{val(current?.voice?.encoderBitrate, 'bit/s')}</strong>
        </p>
        <p>
          Profile FEC{' '}
          <strong>
            {current?.details?.encoderFec == null
              ? 'Unavailable'
              : current.details.encoderFec
                ? 'On'
                : 'Off'}
          </strong>
        </p>
        <p>
          Playback underruns <strong>{val(current?.voice?.underruns)}</strong>
        </p>
        <p>
          Capture overruns <strong>{val(current?.voice?.overruns)}</strong>
        </p>
        <p>
          Processing delay estimate{' '}
          <strong>{val(current?.voice?.processingLatencyMs, 'ms')}</strong>
        </p>
        <p>
          Over-budget frames <strong>{val(current?.voice?.processingOverBudget)}</strong>
        </p>
      </div>
      <p>
        Software CPU time per 10 ms frame: last / mean / maximum since chain creation. Apple
        system-stage CPU is unavailable.
      </p>
      <ul>
        {current?.details?.processingStages.map((s) => (
          <li key={s.stage}>
            {s.stage} ({s.backend}): {val(s.lastMs, 'ms')} / {val(s.meanMs, 'ms')} /{' '}
            {val(s.maxMs, 'ms')} · {s.frames} frames
          </li>
        ))}
      </ul>
      {current?.details && !current.details.processingStages.length && (
        <p>No software processing stages enabled.</p>
      )}
      <h3>Per-speaker receiver counters</h3>
      <table>
        <thead>
          <tr>
            <th>Track</th>
            <th>Buffer (ms)</th>
            <th>Lost frames</th>
            <th>PLC calls</th>
            <th>Late packets</th>
          </tr>
        </thead>
        <tbody>
          {current?.details?.speakers.map((s) => (
            <tr key={s.track}>
              <td>{s.track}</td>
              <td>{val(s.jitterDelayMs)}</td>
              <td>{s.lost}</td>
              <td>{s.plc}</td>
              <td>{s.late}</td>
            </tr>
          ))}
        </tbody>
      </table>
      {!current?.details?.speakers.length && <p>No remote speaker samples.</p>}
      <h3>Unavailable measurements</h3>
      <ul>
        {Object.entries(unavailable).map(([key, reason]) => (
          <li key={key}>{reason}</li>
        ))}
      </ul>
      <h3>Global input</h3>
      <p>
        {input
          ? input.running
            ? 'Listening'
            : input.suspended
              ? 'Suspended'
              : 'Not listening'
          : 'Input status unavailable'}{' '}
        · {input?.backend ?? 'Backend identity unavailable'} ·{' '}
        {input?.permission ?? 'No permission requirement reported'}
      </p>
      <ul>
        {input?.events.map((event, i) => (
          <li key={i}>{event}</li>
        ))}
      </ul>
      <details>
        <summary>Device selections and processing configuration</summary>
        <pre>{JSON.stringify(settings, null, 2)}</pre>
      </details>
      <label>
        Notes (no credentials or message contents)
        <textarea
          maxLength={4000}
          value={notes}
          onChange={(e) => {
            setNotes(e.target.value)
            setBundle(null)
          }}
        />
      </label>
      <p>
        Preview freezes the displayed evidence for copying/export. Device identities are omitted. No
        raw logs, messages, audio recordings or credentials are collected.
      </p>
      <button disabled={!samples.length} onClick={prepare}>
        Preview redacted diagnostics
      </button>
      {bundle && (
        <>
          <pre aria-label="Diagnostics export preview">{JSON.stringify(bundle, null, 2)}</pre>
          <button onClick={() => void copy()}>Copy diagnostics</button>
          <button disabled={busy} onClick={() => void save()}>
            Save log bundle
          </button>
        </>
      )}
      {status && <p role="status">{status}</p>}
    </dialog>
  )
}
