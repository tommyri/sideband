import { ChevronRight, Mic, Square, X } from 'lucide-react'
import { useAppStore } from '../../../store/app-store'
import { useMicrophoneGuidance } from '../../voice/microphone'
import './audio.css'
import { openDiagnostics } from '../../diagnostics/model'
import { ShortcutSettings } from '../shortcuts/ShortcutSettings'
import { ProfileSettings } from '../../profile/ProfileSettings'
import { useEffect, useLayoutEffect, useRef, useState } from 'react'
import { useVoiceStore, refreshVoice } from '../../voice/store'
import { memberAction } from '../../voice/api'
import {
  audioLevel,
  defaults,
  effectiveInput,
  environment,
  inputCount,
  onAudioLevel,
  save,
  stopAudioTest,
  testAudio,
  useAudioStore,
  type AudioEnvironment,
  type AudioPreferences,
} from './store'

/** Inputs offered when the host does not report a device's count; settings accept up to 64. */
const UNKNOWN_INPUTS = 16
function InputChannel({
  env,
  draft,
  change,
}: {
  env: AudioEnvironment
  draft: AudioPreferences
  change: (value: number) => void
}) {
  const count = inputCount(env, draft.input)
  const choice = draft.inputChannel
  const shown = effectiveInput(choice, count)
  return (
    <>
      <label>
        Microphone input
        <select
          aria-label="Microphone input"
          disabled={draft.apple}
          value={shown}
          onChange={(e) => change(Number(e.target.value))}
        >
          {Array.from({ length: count ?? UNKNOWN_INPUTS }, (_, i) => (
            <option key={i + 1} value={i + 1}>
              Input {i + 1}
            </option>
          ))}
          {count === null && choice > UNKNOWN_INPUTS && (
            <option value={choice}>Input {choice}</option>
          )}
          {(count === null || count > 1) && <option value={0}>Average of all inputs</option>}
        </select>
      </label>
      {!draft.apple && shown !== choice && (
        <p className="audio-hint">
          {count === 1
            ? 'This device has one input; it uses Input 1.'
            : `This device has no Input ${choice}; it uses Input 1.`}
        </p>
      )}
    </>
  )
}
function message(error: unknown) {
  return error && typeof error === 'object' && 'message' in error
    ? String(error.message)
    : String(error)
}
function InputMeter({ testing, active }: { testing: boolean; active: boolean }) {
  const [level, setLevel] = useState(-96)
  useEffect(() => {
    let live = true
    if (!active && !testing) return
    const report = (value: number) => {
      if (live) setLevel(value)
    }
    const unlisten = testing ? onAudioLevel(report).catch(() => () => {}) : undefined
    const timer = active
      ? setInterval(
          () =>
            void audioLevel()
              .then(report)
              .catch(() => {}),
          100,
        )
      : undefined
    return () => {
      live = false
      clearInterval(timer)
      void unlisten?.then((stop) => stop())
    }
  }, [active, testing])
  return (
    <div className="audio-meter">
      <meter
        aria-label="Microphone input level"
        min={-96}
        max={0}
        value={active || testing ? level : -96}
      />
      <span>{active || testing ? `${Math.round(level)} dBFS` : 'Ready'}</span>
    </div>
  )
}
export type AudioSettingsFocus = 'microphone' | 'talkingMode'

export function AudioSettings({
  close,
  initialFocus,
}: {
  close: () => void
  initialFocus?: AudioSettingsFocus
}) {
  const [shortcuts, setShortcuts] = useState(false)
  const [profileOpen, setProfileOpen] = useState(false)
  const dialog = useRef<HTMLDialogElement>(null)
  const microphone = useRef<HTMLSelectElement>(null)
  const talkingMode = useRef<HTMLSelectElement>(null)
  const processing = useRef<HTMLDetailsElement>(null)
  const appleProcessing = useRef<HTMLInputElement>(null)
  const [env, setEnv] = useState<AudioEnvironment | null>(null)
  const [draft, setDraft] = useState<AudioPreferences>(defaults)
  const [error, setError] = useState('')
  const [status, setStatus] = useState('')
  const [busy, setBusy] = useState(false)
  const [testing, setTesting] = useState<'mic' | 'output' | 'calibrate' | null>(null)
  const localTest = useRef<{ stopping: boolean } | null>(null)
  const [capturing, setCapturing] = useState(false)
  const active = useVoiceStore((s) => s.active)
  const view = useVoiceStore((s) => (active ? s.views[active.serverId] : undefined))
  const stats = useVoiceStore((s) => (active ? s.stats[active.serverId] : undefined))
  const channel = view?.channels.find((c) => c.id === active?.channelId)
  const connection = useAppStore(
    (s) => s.servers.find((s) => s.id === active?.serverId)?.status.kind,
  )
  const mic = useMicrophoneGuidance(
    active ? { server: active.serverId, view, channel, connection } : undefined,
  )
  const saved = useAudioStore((s) => s.preferences)
  const dirty =
    !!env &&
    (Object.keys(defaults) as (keyof AudioPreferences)[]).some((key) => saved[key] !== draft[key])
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null
    const sheet = dialog.current
    sheet?.showModal()
    let live = true
    void environment()
      .then((e) => {
        if (live) {
          setEnv(e)
          setDraft(e.preferences)
        }
      })
      .catch((e) => {
        if (live) setError(message(e))
      })
    return () => {
      live = false
      if (localTest.current) {
        localTest.current = null
        void stopAudioTest().catch(() => {})
      }
      sheet?.close()
      if (previous?.isConnected) previous.focus({ preventScroll: true })
      // Resizing can move the personal settings entry while the sheet is open.
      if (previous instanceof HTMLButtonElement && document.activeElement !== previous) {
        const entries = document.querySelectorAll<HTMLButtonElement>(
          '.personal-settings-sidebar, .personal-settings-compact',
        )
        Array.from(entries)
          .find((entry) => entry.getClientRects().length)
          ?.focus({ preventScroll: true })
      }
    }
  }, [])
  const ready = env !== null
  useLayoutEffect(() => {
    if (!initialFocus || !ready) return
    let target: HTMLElement | null = talkingMode.current
    if (initialFocus === 'microphone') {
      if (microphone.current?.disabled && processing.current) processing.current.open = true
      target = microphone.current?.disabled ? appleProcessing.current : microphone.current
    }
    target?.focus()
    target?.scrollIntoView?.({ block: 'center' })
  }, [initialFocus, ready])
  function change<K extends keyof AudioPreferences>(key: K, value: AudioPreferences[K]) {
    setDraft((d) => ({ ...d, [key]: value }))
    setStatus('')
  }
  async function perform(action: () => Promise<unknown>, success = '') {
    setBusy(true)
    setError('')
    setStatus('')
    try {
      await action()
      setStatus(success)
    } catch (e) {
      setError(message(e))
    } finally {
      setBusy(false)
    }
  }
  async function test(kind: 'mic' | 'output' | 'calibrate') {
    if (active || busy || localTest.current) return
    const run = { stopping: false }
    localTest.current = run
    setTesting(kind)
    setBusy(true)
    setError('')
    setStatus('')
    try {
      const threshold = await testAudio(draft, kind)
      if (localTest.current !== run) return
      if (kind === 'calibrate') change('threshold', Math.round(threshold))
      setStatus(
        kind === 'calibrate'
          ? 'Calibration complete. Apply to save the new threshold.'
          : 'Local audio test complete.',
      )
    } catch (e) {
      if (localTest.current !== run) return
      if (run.stopping && message(e) === 'Audio test stopped')
        setStatus('Local audio test stopped.')
      else setError(message(e))
    } finally {
      if (localTest.current === run) {
        localTest.current = null
        setTesting(null)
        setBusy(false)
      }
    }
  }
  async function stopTest() {
    const run = localTest.current
    if (!run || run.stopping) return
    run.stopping = true
    try {
      await stopAudioTest()
    } catch (e) {
      run.stopping = false
      setError(message(e))
    }
  }
  const defaultMic = env?.devices.find((d) => d.id === env.defaultInput && d.input)?.name
  function slider(key: keyof AudioPreferences, label: string, min: number, max: number, step = 1) {
    return (
      <label className="audio-control">
        {label} <span>{String(draft[key])}</span>
        <input
          aria-label={label}
          type="range"
          min={min}
          max={max}
          step={step}
          value={Number(draft[key])}
          onChange={(e) => change(key, Number(e.target.value))}
        />
      </label>
    )
  }
  return (
    <dialog
      ref={dialog}
      data-binding-capture={capturing}
      className="personal-audio"
      aria-labelledby="audio-title"
      onCancel={(e) => {
        e.preventDefault()
        close()
      }}
      onKeyDown={(e) => {
        if (e.key === 'Escape' && capturing) {
          e.preventDefault()
          setCapturing(false)
          return
        }
        if (e.key !== 'Tab' || (e.target as HTMLElement).closest('dialog') !== e.currentTarget)
          return
        const nodes = Array.from(
          e.currentTarget.querySelectorAll<HTMLElement>('button,input,select,summary'),
        ).filter(
          (n) =>
            !n.matches(':disabled') &&
            (n.tagName === 'SUMMARY' || !n.closest('details:not([open])')) &&
            n.getClientRects().length > 0,
        )
        if (!nodes.length) return
        e.preventDefault()
        const i = nodes.indexOf(document.activeElement as HTMLElement)
        nodes[(i + (e.shiftKey ? -1 : 1) + nodes.length) % nodes.length]?.focus()
      }}
    >
      <header>
        <div>
          <span>Personal settings</span>
          <h2 id="audio-title">Audio</h2>
        </div>
        <button
          type="button"
          className="button secondary audio-profile"
          disabled={busy || testing !== null || capturing}
          onClick={() => setProfileOpen(true)}
        >
          Profile and devices
        </button>
        <button className="audio-close" aria-label="Close audio settings" onClick={close}>
          <X size={19} />
        </button>
      </header>
      {shortcuts && <ShortcutSettings close={() => setShortcuts(false)} />}
      {profileOpen && <ProfileSettings close={() => setProfileOpen(false)} />}
      <div className="personal-audio-body">
        {!env ? (
          !error && <p role="status">Loading audio devices…</p>
        ) : (
          <>
            <fieldset className="audio-basics" disabled={busy}>
              <legend className="sr-only">Audio preferences</legend>
              {(['input', 'output'] as const).map((key) => (
                <label key={key}>
                  {key === 'input' ? 'Microphone' : 'Output device'}
                  <select
                    ref={key === 'input' ? microphone : undefined}
                    aria-label={key === 'input' ? 'Microphone' : 'Output device'}
                    disabled={draft.apple}
                    value={draft[key] ?? ''}
                    onChange={(e) => change(key, e.target.value || null)}
                  >
                    <option value="">
                      System default{key === 'input' && defaultMic ? ` · ${defaultMic}` : ''}
                    </option>
                    {draft[key] && !env.devices.some((d) => d.id === draft[key]) && (
                      <option value={draft[key]}>Unavailable device — choose another</option>
                    )}
                    {env.devices
                      .filter((d) => d[key])
                      .map((d) => (
                        <option key={d.id} value={d.id}>
                          {d.name}
                        </option>
                      ))}
                  </select>
                </label>
              ))}
              <label>
                Talking mode
                <select
                  ref={talkingMode}
                  aria-label="Talking mode"
                  value={draft.mode}
                  onChange={(e) => change('mode', e.target.value)}
                >
                  <option value="ptt">Push-to-talk</option>
                  <option value="vad">Voice activation</option>
                  <option value="open">Open microphone</option>
                </select>
              </label>
            </fieldset>
            <p className="audio-guidance">{mic.guidance}</p>
            {draft.mode !== saved.mode && (
              <p className="audio-pending">
                {draft.mode === 'open'
                  ? 'Applying open microphone will transmit continuously in voice while you are unmuted.'
                  : 'Apply settings to use the selected talking mode.'}
              </p>
            )}
            <section className="audio-check" aria-label="Microphone check">
              <div>
                {testing ? (
                  <button className="button secondary" onClick={() => void stopTest()}>
                    <Square size={14} /> Stop test
                  </button>
                ) : (
                  <button
                    className="button secondary"
                    aria-describedby="mic-check-hint"
                    disabled={busy || !!active}
                    onClick={() => void test('mic')}
                  >
                    <Mic size={17} /> Check mic
                  </button>
                )}
                <InputMeter
                  active={!!active}
                  testing={testing === 'mic' || testing === 'calibrate'}
                />
              </div>
              <p id="mic-check-hint">
                {active
                  ? 'Leave voice to run a local mic check.'
                  : 'Use headphones. This 10-second check plays your mic back locally after a one-second delay.'}
              </p>
              {env.microphone === 'Denied' && (
                <p>Allow Sideband to use the microphone in your system’s privacy settings.</p>
              )}
            </section>
            <details ref={processing} className="audio-disclosure">
              <summary>
                <ChevronRight size={16} /> Advanced processing
              </summary>
              <fieldset disabled={busy}>
                <legend className="sr-only">Microphone processing</legend>
                {env.apple && (
                  <label>
                    <input
                      ref={appleProcessing}
                      type="checkbox"
                      checked={draft.apple}
                      onChange={(e) => {
                        const enabled = e.currentTarget.checked
                        setDraft((d) => ({
                          ...d,
                          apple: enabled,
                          input: null,
                          output: null,
                          echo: false,
                          gain: false,
                          noise: 'off',
                        }))
                      }}
                    />
                    Use Apple voice processing
                  </label>
                )}
                <label>
                  <input
                    type="checkbox"
                    disabled={draft.apple || !env.sonora}
                    checked={draft.echo}
                    onChange={(e) => change('echo', e.target.checked)}
                  />
                  Echo cancellation — reduces speaker echo; adds CPU work (1.5 ms/frame budget).
                </label>
                <label>
                  Noise suppression
                  <select
                    aria-label="Noise suppression"
                    disabled={draft.apple}
                    value={draft.noise}
                    onChange={(e) => change('noise', e.target.value)}
                  >
                    <option value="off">Off</option>
                    {env.sonora && (
                      <option value="sonora">WebRTC · about 7 ms added latency</option>
                    )}
                    {env.rnnoise && <option value="rnnoise">RNNoise · 10 ms added latency</option>}
                  </select>
                </label>
                <label>
                  <input
                    type="checkbox"
                    disabled={draft.apple || !env.sonora}
                    checked={draft.gain}
                    onChange={(e) => change('gain', e.target.checked)}
                  />
                  Automatic gain — steadies microphone volume; adds CPU work (1.5 ms/frame budget).
                </label>
                <p>
                  {draft.apple
                    ? 'Apple processing uses system-default devices and includes echo cancellation, noise suppression and automatic gain; software stages are disabled. Device latency varies.'
                    : 'All processing is off by default. Software chain budget: 3 ms per 10 ms frame.'}{' '}
                  {!env.sonora &&
                    !env.rnnoise &&
                    'Software processing models are not included in this build.'}
                </p>
              </fieldset>
            </details>
            <details className="audio-disclosure">
              <summary>
                <ChevronRight size={16} /> More audio controls
              </summary>
              <fieldset disabled={busy}>
                <legend>Device options</legend>
                <InputChannel
                  env={env}
                  draft={draft}
                  change={(value) => change('inputChannel', value)}
                />
                <p className="audio-hint">
                  For interfaces with several inputs: pick the one your microphone is plugged into.
                  Averaging lowers a single microphone’s level and adds the noise of the unused
                  inputs.
                </p>
                <p>
                  Microphone permission: {env.microphone}. If denied, enable Sideband in your
                  system’s microphone privacy settings.
                </p>
                <button
                  onClick={() =>
                    void perform(async () => {
                      const next = await environment()
                      setEnv(next)
                    }, 'Device list refreshed.')
                  }
                >
                  Refresh devices
                </button>

                <button disabled={!!active} onClick={() => void test('output')}>
                  Test output
                </button>
              </fieldset>
              <fieldset disabled={busy}>
                <legend>Talking controls</legend>
                <button
                  onClick={(event) => {
                    event.currentTarget.focus()
                    setShortcuts(true)
                  }}
                >
                  Global shortcuts
                </button>
                {draft.mode === 'ptt' && (
                  <>
                    {slider('releaseMs', 'Release delay (ms)', 0, 2000, 10)}
                    <button
                      disabled={env.systemBinding}
                      onClick={(event) => {
                        event.currentTarget.focus()
                        setCapturing(true)
                      }}
                      onKeyDown={(e) => {
                        if (!capturing) return
                        e.preventDefault()
                        e.stopPropagation()
                        if (e.key === 'Escape') {
                          setCapturing(false)
                          return
                        }
                        if (['Shift', 'Control', 'Alt', 'Meta', 'Tab'].includes(e.key)) return
                        change('binding', e.code)
                        setCapturing(false)
                      }}
                      onPointerDown={(e) => {
                        if (capturing && env.mouse && e.button > 0) {
                          e.preventDefault()
                          change('binding', `Mouse${e.button}`)
                          setCapturing(false)
                        }
                      }}
                      onContextMenu={(e) => e.preventDefault()}
                    >
                      {capturing
                        ? 'Press a key' +
                          (env.mouse ? ' or mouse button' : '') +
                          ' · Escape cancels'
                        : `Capture in-app binding${draft.binding ? `: ${draft.binding}` : ''}`}
                    </button>
                    <button onClick={() => change('binding', '')}>Clear binding</button>
                    <p>
                      Bindings work while the app is focused, outside text fields and dialogs. Hold
                      to talk remains available. Configure global listening and platform permissions
                      in Global shortcuts.
                    </p>
                    {env.systemBinding && (
                      <p>
                        Set the global binding in your system’s shortcut settings; this backend does
                        not support key capture.
                      </p>
                    )}
                    {env.permission && (
                      <p>
                        Global PTT requires {env.permission}. Use Global shortcuts to configure
                        listening.
                      </p>
                    )}
                    {env.limitations.map((s, i) => (
                      <p key={i}>{s}</p>
                    ))}
                  </>
                )}
                {draft.mode === 'vad' && (
                  <>
                    {slider('threshold', 'Activation threshold (dBFS)', -96, 0)}
                    {slider('hangoverMs', 'Quiet hangover (ms)', 0, 5000, 10)}
                    <button disabled={!!active} onClick={() => void test('calibrate')}>
                      Auto-calibrate · stay quiet for 2 seconds
                    </button>
                    <p>
                      Calibration listens locally to background noise and sets the threshold 12 dB
                      above its median. Leave voice first.
                    </p>
                  </>
                )}
              </fieldset>
              <fieldset disabled={busy}>
                <legend>Output</legend>
                {slider('master', 'Master volume (%)', 0, 200)}
                {slider('cues', 'Cue volume (%)', 0, 100)}
                {slider('attenuation', 'Reduce other voices while speaking (%)', 0, 50)}
              </fieldset>
              {active && view && (
                <fieldset disabled={busy}>
                  <legend>People in this server</legend>
                  {view.members
                    .filter((m) => m.id !== view.ownUserId)
                    .map((m) => (
                      <label key={`${m.id}:${m.volume}`}>
                        @{m.username} · {m.volume}%
                        <input
                          aria-label={`Volume for ${m.username}`}
                          type="range"
                          min="0"
                          max="200"
                          defaultValue={m.volume}
                          onPointerUp={(e) => {
                            const volume = Number(e.currentTarget.value)
                            void perform(async () => {
                              await memberAction({
                                serverId: active.serverId,
                                userId: m.id,
                                action: 'volume',
                                volume,
                                enabled: null,
                                channelId: null,
                                reason: null,
                              })
                              await refreshVoice(active.serverId)
                            })
                          }}
                          onKeyUp={(e) => {
                            const volume = Number(e.currentTarget.value)
                            void perform(async () => {
                              await memberAction({
                                serverId: active.serverId,
                                userId: m.id,
                                action: 'volume',
                                volume,
                                enabled: null,
                                channelId: null,
                                reason: null,
                              })
                              await refreshVoice(active.serverId)
                            })
                          }}
                        />
                      </label>
                    ))}
                  <button
                    onClick={() =>
                      void perform(async () => {
                        for (const m of view.members)
                          if (m.id !== view.ownUserId)
                            await memberAction({
                              serverId: active.serverId,
                              userId: m.id,
                              action: 'volume',
                              volume: 100,
                              enabled: null,
                              channelId: null,
                              reason: null,
                            })
                        await refreshVoice(active.serverId)
                      }, 'Per-user volumes reset.')
                    }
                  >
                    Reset all per-user volumes
                  </button>
                </fieldset>
              )}
              <fieldset disabled={busy}>
                <legend>Network & diagnostics</legend>
                <p>
                  {channel
                    ? `${channel.name} · ${channel.profile} · ${stats ? Math.round(stats.encoderBitrate / 1000) + ' kbps' : 'bitrate pending'}`
                    : 'Join voice to see the channel profile and bitrate.'}
                </p>
                {slider('jitterMin', 'Minimum jitter buffer (ms)', 20, 200, 10)}
                {slider('jitterMax', 'Maximum jitter buffer (ms)', 20, 200, 10)}
                <p>
                  More buffering tolerates network variation but adds delay. Changes take effect
                  during a pause in speech.
                </p>
                <button onClick={openDiagnostics}>Diagnostics · Ctrl+Shift+D</button>
              </fieldset>
            </details>
          </>
        )}
      </div>
      <footer>
        <div>
          {error && <p role="alert">{error}</p>}
          {status && <p role="status">{status}</p>}
        </div>
        {dirty ? (
          <>
            <button disabled={busy} onClick={close}>
              Cancel
            </button>
            <button
              className="button primary"
              disabled={busy || capturing || draft.jitterMin > draft.jitterMax}
              onClick={() => void perform(() => save(draft), 'Applied and saved.')}
            >
              Apply settings
            </button>
          </>
        ) : (
          <button className="button primary" onClick={close}>
            Done
          </button>
        )}
      </footer>
    </dialog>
  )
}
