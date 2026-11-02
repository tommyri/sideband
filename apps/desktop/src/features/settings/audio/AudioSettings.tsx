import { useEffect, useRef, useState } from 'react'
import { listen } from '@tauri-apps/api/event'
import { invokeBridge } from '../../../lib/tauri'
import { useVoiceStore, refreshVoice } from '../../voice/store'
import { memberAction } from '../../voice/api'
import { defaults, environment, save, type AudioEnvironment, type AudioPreferences } from './store'

function message(error: unknown) {
  return error && typeof error === 'object' && 'message' in error
    ? String(error.message)
    : String(error)
}
function InputMeter({ testing, active }: { testing: boolean; active: boolean }) {
  const [level, setLevel] = useState(-96)
  useEffect(() => {
    let live = true
    const unlisten = listen<number>('ss://audio-level', (event) => {
      if (live) setLevel(event.payload)
    }).catch(() => () => {})
    const timer = setInterval(() => {
      if (active && !testing)
        void invokeBridge<number>('audio_level')
          .then((v) => {
            if (live) setLevel(v)
          })
          .catch(() => {})
    }, 100)
    return () => {
      live = false
      clearInterval(timer)
      void unlisten.then((stop) => stop())
    }
  }, [active, testing])
  return (
    <div className="audio-meter">
      <label htmlFor="input-level">
        Input level{' '}
        {active || testing ? `${Math.round(level)} dBFS` : '— start Test mic to measure'}
      </label>
      <meter id="input-level" min={-96} max={0} value={active || testing ? level : -96} />
    </div>
  )
}
export function AudioSettings({ close }: { close: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null)
  const [env, setEnv] = useState<AudioEnvironment | null>(null)
  const [draft, setDraft] = useState<AudioPreferences>(defaults)
  const [error, setError] = useState('')
  const [status, setStatus] = useState('')
  const [busy, setBusy] = useState(false)
  const [testing, setTesting] = useState(false)
  const [capturing, setCapturing] = useState(false)
  const active = useVoiceStore((s) => s.active)
  const view = useVoiceStore((s) => (active ? s.views[active.serverId] : undefined))
  const stats = useVoiceStore((s) => (active ? s.stats[active.serverId] : undefined))
  const channel = view?.channels.find((c) => c.id === active?.channelId)
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null
    dialog.current?.showModal()
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
      void invokeBridge('stop_audio_test').catch(() => {})
      previous?.focus()
    }
  }, [])
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
  async function test(kind: string) {
    setTesting(true)
    await perform(
      async () => {
        const threshold = await invokeBridge<number>('test_audio', { preferences: draft, kind })
        if (kind === 'calibrate') change('threshold', Math.round(threshold))
      },
      kind === 'calibrate'
        ? 'Calibration complete. Apply to save the new threshold.'
        : 'Local audio test complete.',
    )
    setTesting(false)
  }
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
      className="audio-settings"
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
        if (e.key !== 'Tab') return
        const nodes = Array.from(
          e.currentTarget.querySelectorAll<HTMLElement>('button,input,select,summary'),
        ).filter((n) => !n.matches(':disabled') && n.getClientRects().length > 0)
        if (!nodes.length) return
        e.preventDefault()
        const i = nodes.indexOf(document.activeElement as HTMLElement)
        nodes[(i + (e.shiftKey ? -1 : 1) + nodes.length) % nodes.length]?.focus()
      }}
    >
      <header>
        <div>
          <p className="eyebrow">YOUR AUDIO</p>
          <h2 id="audio-title">Audio & voice</h2>
        </div>
        <button aria-label="Close audio settings" onClick={close}>
          ×
        </button>
      </header>
      <div className="audio-body">
        {error && (
          <p role="alert" className="form-error">
            {error}
          </p>
        )}
        {status && <p role="status">{status}</p>}
        {!env ? (
          <p>Loading audio devices…</p>
        ) : (
          <>
            <fieldset disabled={busy}>
              <legend>Devices</legend>
              {(['input', 'output'] as const).map((key) => (
                <label key={key}>
                  {key === 'input' ? 'Microphone' : 'Output device'}
                  <select
                    aria-label={key === 'input' ? 'Microphone' : 'Output device'}
                    disabled={draft.apple}
                    value={draft[key] ?? ''}
                    onChange={(e) => change(key, e.target.value || null)}
                  >
                    <option value="">System default</option>
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
              <p>
                Microphone permission: {env.microphone}. If denied, enable SocialSpeak in your
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
            </fieldset>
            <InputMeter testing={testing} active={!!active} />
            <div className="audio-test-actions">
              <button disabled={busy || !!active || !env} onClick={() => void test('mic')}>
                Test mic · 10 seconds
              </button>
              <button disabled={busy || !!active} onClick={() => void test('output')}>
                Test output
              </button>
              {testing && (
                <button onClick={() => void invokeBridge('stop_audio_test')}>Stop test</button>
              )}
            </div>
            <p className="audio-hint">
              Tests stay on this computer. Use headphones: Test mic plays raw input after a 1-second
              delay.{' '}
              {active
                ? 'Leave voice to run local tests.'
                : 'Starting a mic test may request microphone permission.'}
            </p>
            <fieldset disabled={busy}>
              <legend>Transmission</legend>
              <label>
                Talking mode
                <select
                  aria-label="Talking mode"
                  value={draft.mode}
                  onChange={(e) => change('mode', e.target.value)}
                >
                  <option value="ptt">Push-to-talk</option>
                  <option value="vad">Voice activation</option>
                  <option value="open">Open microphone</option>
                </select>
              </label>
              {draft.mode === 'open' && (
                <p>
                  Open microphone transmits continuously while connected and unmuted. Apply only
                  when you are ready to broadcast.
                </p>
              )}
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
                      ? 'Press a key' + (env.mouse ? ' or mouse button' : '') + ' · Escape cancels'
                      : `Capture in-app binding${draft.binding ? `: ${draft.binding}` : ''}`}
                  </button>
                  <button onClick={() => change('binding', '')}>Clear binding</button>
                  <p>
                    Bindings work while the app is focused, outside text fields and dialogs. Hold to
                    talk remains available. Global shortcuts and permission/evdev setup are coming
                    with T-058.
                  </p>
                  {env.systemBinding && (
                    <p>
                      Set the global binding in your system’s shortcut settings; this backend does
                      not support key capture.
                    </p>
                  )}
                  {env.permission && (
                    <p>
                      Global PTT requires {env.permission}. No global listener is started by this
                      screen.
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
              <legend>Processing</legend>
              {env.apple && (
                <label>
                  <input
                    type="checkbox"
                    checked={draft.apple}
                    onChange={(e) =>
                      setDraft((d) => ({
                        ...d,
                        apple: e.target.checked,
                        input: null,
                        output: null,
                        echo: false,
                        gain: false,
                        noise: 'off',
                      }))
                    }
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
                  {env.sonora && <option value="sonora">WebRTC · about 7 ms added latency</option>}
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
              <legend>Advanced</legend>
              <p>
                {channel
                  ? `${channel.name} · ${channel.profile} · ${stats ? Math.round(stats.encoderBitrate / 1000) + ' kbps' : 'bitrate pending'}`
                  : 'Join voice to see the channel profile and bitrate.'}
              </p>
              {slider('jitterMin', 'Minimum jitter buffer (ms)', 20, 200, 10)}
              {slider('jitterMax', 'Maximum jitter buffer (ms)', 20, 200, 10)}
              <p>
                More buffering tolerates network variation but adds delay. Changes settle at silence
                boundaries. Frame duration follows the channel profile; a 10 ms override awaits the
                T-018 release decision.
              </p>
              <button disabled title="The diagnostics panel will be added in T-063">
                Diagnostics · coming soon
              </button>
            </fieldset>
            <footer>
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
            </footer>
          </>
        )}
      </div>
    </dialog>
  )
}
