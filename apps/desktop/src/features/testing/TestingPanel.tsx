import { useEffect, useRef, useState } from 'react'
import { invokeBridge } from '../../lib/tauri'
import { useAppStore } from '../../store/app-store'
import { environment, useAudioStore } from '../settings/audio/store'
import { refreshShortcuts, useShortcuts } from '../settings/shortcuts/store'
import { useVoiceStore } from '../voice/store'
import {
  acousticResult,
  begin,
  context,
  definitions,
  preview,
  record,
  recorder,
  type Json,
  type Outcome,
  type Run,
  type TestId,
} from './report'
import schema from './report.schema.json'

async function captureContext(): Promise<Json> {
  try {
    const [audio, shortcuts] = await Promise.all([environment(), refreshShortcuts()])
    return context(audio, shortcuts)
  } catch {
    return { value: null, reason: 'Could not query audio/input environment' }
  }
}
const message = (e: unknown) =>
  e instanceof Error ? e.message : 'The native operation failed. Check that Downloads is writable.'
export function TestingPanel() {
  const [open, setOpen] = useState(false)
  const [run, setRun] = useState<Run | null>(null)
  const [testId, setTestId] = useState<TestId>('ptt-focus')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [omit, setOmit] = useState(true)
  const [consent, setConsent] = useState(false)
  const [includeRecording, setIncludeRecording] = useState(false)
  const [wav, setWav] = useState<number[] | null>(null)
  const [bundle, setBundle] = useState<Json | null>(null)
  const [saved, setSaved] = useState('')
  const dialog = useRef<HTMLDialogElement>(null)
  const running = !!run && !run.endedAt
  useEffect(() => {
    if (!open) return
    const previous = document.activeElement as HTMLElement | null
    dialog.current?.showModal()
    return () => previous?.focus()
  }, [open])
  useEffect(() => {
    if (!running) return
    const update = () => {
      if (recorder) {
        if (performance.now() - recorder.start >= 30 * 60 * 1000) {
          record('limit', { reason: '30-minute run limit' })
          recorder.finish({ value: null, reason: 'Time limit; final snapshot unavailable' })
        }
        setRun({ ...recorder.run })
      }
    }
    const timer = setInterval(update, 500)
    const audioStop = useAudioStore.subscribe((next, previous) => {
      if (JSON.stringify(next.preferences) !== JSON.stringify(previous.preferences))
        record('audio-settings', { ...next.preferences })
    })
    let lastInput = ''
    const inputStop = useShortcuts.subscribe(({ snapshot }) => {
      if (!snapshot) return
      const state = {
        running: snapshot.running,
        suspended: snapshot.suspended,
        capturing: snapshot.capturing,
        error: !!snapshot.error,
      }
      const key = JSON.stringify(state)
      if (key !== lastInput) {
        lastInput = key
        record('input-status', state)
      }
    })
    const voiceStop = useVoiceStore.subscribe((next, previous) => {
      if (JSON.stringify(next.active) !== JSON.stringify(previous.active))
        record('voice-session', {
          active: !!next.active,
          muted: next.active?.muted ?? null,
          deafened: next.active?.deafened ?? null,
        })
    })
    const focus = () =>
      record('window-focus', { focused: document.hasFocus(), visibility: document.visibilityState })
    window.addEventListener('focus', focus)
    window.addEventListener('blur', focus)
    document.addEventListener('visibilitychange', focus)
    return () => {
      clearInterval(timer)
      audioStop()
      inputStop()
      voiceStop()
      window.removeEventListener('focus', focus)
      window.removeEventListener('blur', focus)
      document.removeEventListener('visibilitychange', focus)
    }
  }, [running])
  const update = (fn: (r: Run) => Run) => {
    if (!run) return
    const next = fn(recorder?.run ?? run)
    if (recorder) recorder.run = next
    setRun(next)
    setBundle(null)
    setSaved('')
  }
  async function start() {
    setBusy(true)
    setError('')
    setBundle(null)
    setSaved('')
    setWav(null)
    setConsent(false)
    setIncludeRecording(false)
    try {
      const [build, settings] = await Promise.all([
        invokeBridge<Json>('test_build_identity'),
        captureContext(),
      ])
      const r = begin(testId, build, settings)
      r.serverId =
        useVoiceStore.getState().active?.serverId ??
        useAppStore.getState().selectedServerId ??
        undefined
      setRun({ ...r.run })
      record('run-start', { voiceActive: !!useVoiceStore.getState().active })
    } catch (e) {
      setError(message(e))
    } finally {
      setBusy(false)
    }
  }
  async function finish() {
    setBusy(true)
    try {
      recorder?.finish(await captureContext())
      if (recorder) setRun({ ...recorder.run })
    } finally {
      setBusy(false)
    }
  }
  async function attach(file: File | undefined) {
    setBundle(null)
    setWav(null)
    setIncludeRecording(false)
    if (!file || !consent) return
    if (file.size > 10 * 1024 * 1024) {
      setError('WAV attachments are limited to 10 MiB. Keep the original separately.')
      return
    }
    const bytes = new Uint8Array(await file.arrayBuffer())
    if (
      new TextDecoder().decode(bytes.slice(0, 4)) !== 'RIFF' ||
      new TextDecoder().decode(bytes.slice(8, 12)) !== 'WAVE'
    ) {
      setError('Choose a WAV recording.')
      return
    }
    setWav(Array.from(bytes))
    setError('')
  }
  function makePreview() {
    if (!run) return
    const result = preview(run, omit) as Record<string, Json>
    result.recording = {
      capturedByApp: false,
      participantConsent: consent,
      included: !!wav && consent && includeRecording,
      bytes: !!wav && consent && includeRecording ? wav.length : 0,
      provenance: 'Optional externally recorded WAV; original filename omitted',
    }
    setBundle(result)
    setSaved('')
  }
  async function exportBundle() {
    if (!bundle) return
    setBusy(true)
    setError('')
    const report = bundle as Record<string, Json>
    const bytes = (s: string) => Array.from(new TextEncoder().encode(s))
    const events = (report.events ?? []) as unknown as Run['events']
    const files = [
      { name: 'report.json', bytes: bytes(JSON.stringify(bundle, null, 2)) },
      { name: 'schema.json', bytes: bytes(JSON.stringify(schema, null, 2)) },
      { name: 'diagnostics.log', bytes: bytes(events.map((e) => JSON.stringify(e)).join('\n')) },
      { name: 'notes.txt', bytes: bytes(String(report.notes ?? '')) },
    ]
    if (wav && consent && includeRecording) files.push({ name: 'recording.wav', bytes: wav })
    try {
      setSaved(
        await invokeBridge<string>('export_test_report', { files, consent, includeRecording }),
      )
    } catch (e) {
      setError(message(e))
    } finally {
      setBusy(false)
    }
  }
  return (
    <>
      <button className="testing-launch" onClick={() => setOpen(true)}>
        Guided tests{running ? ' · running' : ''}
      </button>
      {open && (
        <dialog
          ref={dialog}
          className="audio-settings testing-panel"
          aria-labelledby="testing-title"
          onCancel={(e) => {
            e.preventDefault()
            setOpen(false)
          }}
        >
          <header>
            <div>
              <p className="eyebrow">LOCAL TEST EVIDENCE</p>
              <h2 id="testing-title">Guided voice tests</h2>
            </div>
            <button onClick={() => setOpen(false)}>Close</button>
          </header>
          <p>
            Runs stay in memory until you export or quit. Closing this panel keeps an active run
            going (30-minute limit). No microphone recording or automatic upload.
          </p>
          <label>
            Test{' '}
            <select
              value={testId}
              disabled={running || busy}
              onChange={(e) => setTestId(e.target.value as TestId)}
            >
              {Object.entries(definitions).map(([id, d]) => (
                <option key={id} value={id}>
                  {d.title}
                </option>
              ))}
            </select>
          </label>
          {!running && (
            <button disabled={busy} onClick={() => void start()}>
              {run ? 'Start a new run (replaces this run)' : 'Start run'}
            </button>
          )}
          {run && (
            <>
              <p>
                Run {run.runId} · {run.acceptanceTasks.join(', ')} ·{' '}
                {running ? 'Collecting local evidence' : 'Finished'} · {run.events.length} events
                retained · {run.droppedEvents} dropped
              </p>
              <p>
                Automatic observations never mark a step passed. “Not run” is the default; record
                failures honestly.
              </p>
              {run.steps.map((step, i) => (
                <fieldset key={i}>
                  <legend>Step {i + 1}</legend>
                  <p>{step.instruction}</p>
                  <label>
                    Outcome for step {i + 1}{' '}
                    <select
                      value={step.outcome}
                      onChange={(e) =>
                        update((r) => ({
                          ...r,
                          steps: r.steps.map((s, j) =>
                            j === i ? { ...s, outcome: e.target.value as Outcome } : s,
                          ),
                        }))
                      }
                    >
                      {['not-run', 'pass', 'fail'].map((x) => (
                        <option key={x}>{x}</option>
                      ))}
                    </select>
                  </label>
                  <label>
                    Observation for step {i + 1}
                    <textarea
                      maxLength={2000}
                      value={step.observation}
                      onChange={(e) =>
                        update((r) => ({
                          ...r,
                          steps: r.steps.map((s, j) =>
                            j === i ? { ...s, observation: e.target.value } : s,
                          ),
                        }))
                      }
                    />
                  </label>
                </fieldset>
              ))}
              {run.testId === 'listening-trial' && (
                <fieldset>
                  <legend>Controlled trial</legend>
                  {(['presetId', 'conditionId'] as const).map((key) => (
                    <label key={key}>
                      {key}
                      <input
                        maxLength={100}
                        value={run.trial[key]}
                        onChange={(e) =>
                          update((r) => ({ ...r, trial: { ...r.trial, [key]: e.target.value } }))
                        }
                      />
                    </label>
                  ))}
                  {(['quality', 'effort', 'delay'] as const).map((key) => (
                    <label key={key}>
                      {key} · 1 worst, 5 best
                      <select
                        value={run.trial[key] ?? ''}
                        onChange={(e) =>
                          update((r) => ({
                            ...r,
                            trial: {
                              ...r.trial,
                              [key]: e.target.value ? Number(e.target.value) : null,
                            },
                          }))
                        }
                      >
                        <option value="">Not rated</option>
                        {[1, 2, 3, 4, 5].map((n) => (
                          <option key={n}>{n}</option>
                        ))}
                      </select>
                    </label>
                  ))}
                  <p>
                    Apply settings before starting each condition. Preset IDs label a trial; they do
                    not change audio settings. Use a separate run per condition/listener.
                  </p>
                </fieldset>
              )}
              {run.testId === 'acoustic-latency' && (
                <fieldset>
                  <legend>Calibrated acoustic evidence</legend>
                  {(
                    ['sampleRateHz', 'departureSample', 'arrivalSample', 'skewSamples'] as const
                  ).map((key) => (
                    <label key={key}>
                      {key}
                      <input
                        type="number"
                        step="1"
                        value={run.acoustic[key] ?? ''}
                        onChange={(e) =>
                          update((r) => ({
                            ...r,
                            acoustic: {
                              ...r.acoustic,
                              [key]: e.target.value ? Number(e.target.value) : null,
                            },
                          }))
                        }
                      />
                    </label>
                  ))}
                  {(['calibration', 'evidence'] as const).map((key) => (
                    <label key={key}>
                      {key}
                      <textarea
                        maxLength={2000}
                        value={run.acoustic[key]}
                        onChange={(e) =>
                          update((r) => ({
                            ...r,
                            acoustic: { ...r.acoustic, [key]: e.target.value },
                          }))
                        }
                      />
                    </label>
                  ))}
                  <pre>{JSON.stringify(acousticResult(run), null, 2)}</pre>
                </fieldset>
              )}
              <label>
                Run notes (no passwords, tokens, invites or message contents)
                <textarea
                  maxLength={4000}
                  value={run.notes}
                  onChange={(e) => update((r) => ({ ...r, notes: e.target.value }))}
                />
              </label>
              <details>
                <summary>Devices, backend, permissions and settings at start</summary>
                <pre>{JSON.stringify(run.context, null, 2)}</pre>
              </details>
              <details>
                <summary>Voice statistics and recent semantic events</summary>
                <p>
                  Statistics arrive every 500 ms. RTT and processing estimates are not acoustic
                  latency. Counters are cumulative for the current worker/track; they reset on
                  reopen. Offsets use the local monotonic clock at receipt.
                </p>
                <pre>{JSON.stringify(run.events.slice(-12), null, 2)}</pre>
                {!run.events.some((e) => e.kind === 'voice-stats') && (
                  <p>Voice statistics unavailable: no samples observed in this run.</p>
                )}
              </details>
              {running ? (
                <button disabled={busy} onClick={() => void finish()}>
                  Finish run
                </button>
              ) : (
                <fieldset>
                  <legend>Preview & export</legend>
                  <label>
                    <input
                      type="checkbox"
                      checked={omit}
                      onChange={(e) => {
                        setOmit(e.target.checked)
                        setBundle(null)
                      }}
                    />
                    Omit identifying device details (network addresses are never collected)
                  </label>
                  <p>
                    Optional recording attachment: make a WAV externally only with participant
                    consent. This app does not start a recorder. WAV metadata/audio may identify
                    people; inspect it before sharing.
                  </p>
                  <label>
                    <input
                      type="checkbox"
                      checked={consent}
                      onChange={(e) => {
                        setConsent(e.target.checked)
                        setWav(null)
                        setIncludeRecording(false)
                        setBundle(null)
                      }}
                    />
                    All recorded participants consent to attaching this recording
                  </label>
                  {consent && (
                    <label>
                      Attach WAV · maximum 10 MiB
                      <input
                        type="file"
                        accept=".wav,audio/wav"
                        onChange={(e) => void attach(e.target.files?.[0])}
                      />
                    </label>
                  )}
                  {wav && (
                    <label>
                      <input
                        type="checkbox"
                        checked={includeRecording}
                        onChange={(e) => {
                          setIncludeRecording(e.target.checked)
                          setBundle(null)
                        }}
                      />
                      Include the consented WAV in this export ({wav.length} bytes)
                    </label>
                  )}
                  <p>
                    Review every field before sharing. Redaction removes common credential patterns,
                    not every possible secret in free text. ZIP contains report.json, schema.json,
                    diagnostics.log, notes.txt
                    {wav && consent && includeRecording ? ', recording.wav' : ''}.
                  </p>
                  <button onClick={makePreview}>Preview sanitized bundle</button>
                  {bundle && (
                    <>
                      <pre aria-label="Sanitized report preview">
                        {JSON.stringify(bundle, null, 2)}
                      </pre>
                      <button disabled={busy} onClick={() => void exportBundle()}>
                        Export ZIP to Downloads
                      </button>
                    </>
                  )}
                  {saved && (
                    <p role="status">
                      Saved locally: {saved}. Share this file only when you choose.
                    </p>
                  )}
                </fieldset>
              )}
            </>
          )}
          {error && <p role="alert">{error}</p>}
        </dialog>
      )}
    </>
  )
}
