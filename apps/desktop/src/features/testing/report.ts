import type { AppEvent, VoiceStatsPayload } from '../../lib/bindings'
import type { AudioEnvironment } from '../settings/audio/store'
import type { Snapshot } from '../settings/shortcuts/store'

export const definitions = {
  diagnostics: {
    title: 'Diagnostics snapshot',
    tasks: ['T-063'],
    steps: [
      'Review the diagnostics with a listener and record the observed problem; statistics alone do not prove audible quality.',
    ],
  },
  'ptt-focus': {
    title: 'PTT, fullscreen & focus',
    tasks: ['T-048', 'T-049', 'T-050', 'T-058'],
    steps: [
      'Join with a second participant. Confirm the selected PTT binding and talking mode.',
      'Hold and release PTT while another app/game is focused, including fullscreen. Ask the listener to confirm audible start and stop.',
      'Check that the game receives the intended key/mouse action. Toggle mute and deafen; compare the tray.',
      'While PTT is held, change focus, open a password field and cancel binding capture. Confirm transmission stops and recovers safely.',
    ],
  },
  'device-recovery': {
    title: 'Devices, permissions & recovery',
    tasks: ['T-011', 'T-012', 'T-019'],
    steps: [
      'Join voice with a second participant. Check selected devices, permission status and audible input/output.',
      'Unplug and reconnect the selected device. Note fallback, recovery time and what both participants hear.',
      'Deny/revoke microphone permission using system settings, then restore it. Note notices and any rejoin required.',
      'Sleep and wake the computer, then interrupt and restore the network. Confirm reconnect, no stuck transmit and audible recovery.',
    ],
  },
  'listening-trial': {
    title: 'Controlled listening trial',
    tasks: ['T-018', 'T-019'],
    steps: [
      'Agree a preset ID and condition ID with the group. Record room, microphone distance, reference app and noise conditions in notes.',
      'Apply the intended audio settings, then start a run. Use the same passage, devices and volume for each condition.',
      'Each listener independently rates quality, effort and delay from 1 (worst) to 5 (best). Record one report per listener/condition.',
      'Record dropouts, echo, noise and intelligibility. T-018 still requires at least three people and a separate go/no-go decision.',
    ],
  },
  'acoustic-latency': {
    title: 'Acoustic mouth-to-ear measurement',
    tasks: ['T-018'],
    steps: [
      'Use a two-channel recorder with a shared clock. Channel A measures sound at the sending microphone; channel B measures the receiving speaker/headphone output.',
      'Calibrate channel skew using the same impulse on both channels. Record sample rate, skew, routing, distances and recorder details.',
      'Send several distinct impulses through the call. Measure arrival minus departure sample indices on the raw waveform; subtract calibrated skew. Keep trials and failures.',
      'Enter sample indices/rate/skew and evidence reference below. Attach the consented WAV separately if desired. Network RTT is never an acoustic measurement.',
    ],
  },
} as const
export type TestId = keyof typeof definitions
export type Outcome = 'pass' | 'fail' | 'not-run'
export type Json = null | boolean | number | string | Json[] | { [key: string]: Json }
export const missing = (reason: string) => ({ value: null, reason })
export const LIMIT = 500
export interface Entry {
  sequence: number
  offsetMs: number
  kind: string
  data: Json
}
export interface Run {
  schemaVersion: 1
  runId: string
  testId: TestId
  acceptanceTasks: readonly string[]
  startedAt: string
  endedAt: string | null
  build: Json
  context: Json
  finalContext: Json
  events: Entry[]
  droppedEvents: number
  steps: { instruction: string; outcome: Outcome; observation: string }[]
  notes: string
  trial: {
    presetId: string
    conditionId: string
    quality: number | null
    effort: number | null
    delay: number | null
  }
  acoustic: {
    sampleRateHz: number | null
    departureSample: number | null
    arrivalSample: number | null
    skewSamples: number | null
    calibration: string
    evidence: string
  }
}
// An in-memory recorder; no credentials, stores, messages or arbitrary keys enter it.
export class Recorder {
  readonly start: number
  private sequence = 0
  serverId: string | undefined
  run: Run
  constructor(testId: TestId, build: Json, context: Json, clock = performance.now()) {
    this.start = clock
    this.run = {
      schemaVersion: 1,
      runId: crypto.randomUUID(),
      testId,
      acceptanceTasks: definitions[testId].tasks,
      startedAt: new Date().toISOString(),
      endedAt: null,
      build,
      context,
      finalContext: missing('Run not finished'),
      events: [],
      droppedEvents: 0,
      steps: definitions[testId].steps.map((instruction) => ({
        instruction,
        outcome: 'not-run',
        observation: '',
      })),
      notes: '',
      trial: { presetId: '', conditionId: '', quality: null, effort: null, delay: null },
      acoustic: {
        sampleRateHz: null,
        departureSample: null,
        arrivalSample: null,
        skewSamples: null,
        calibration: '',
        evidence: '',
      },
    }
  }
  add(kind: string, data: Json, clock = performance.now()) {
    if (this.run.endedAt) return
    const previous = this.run.events.at(-1)?.offsetMs ?? 0
    this.run.events.push({
      sequence: ++this.sequence,
      offsetMs: Math.max(previous, Math.round(clock - this.start), 0),
      kind,
      data:
        JSON.stringify(data).length > 4096
          ? missing('Event exceeded 4096-character retention limit')
          : data,
    })
    if (this.run.events.length > LIMIT) {
      this.run.events.shift()
      this.run.droppedEvents++
    }
  }
  finish(context: Json) {
    this.run.finalContext = context
    this.run.endedAt = new Date().toISOString()
  }
}
export let recorder: Recorder | null = null
export function begin(testId: TestId, build: Json, context: Json) {
  recorder = new Recorder(testId, build, context)
  return recorder
}
export function record(kind: string, data: Json) {
  recorder?.add(kind, data)
}
export function observe(event: AppEvent, activeServer: string | undefined) {
  if (!recorder || event.serverId !== (recorder.serverId ?? activeServer)) return
  const e = event.event
  if (e.kind === 'voiceStats') record('voice-stats', stats(e.stats))
  if (e.kind === 'connection') record('connection', { state: e.status.kind })
  if (e.kind === 'voiceDevice') {
    // Discard backend error prose: it can contain paths and device identifiers.
    record('device', {
      state: e.state.kind,
      sampleRateHz: e.state.kind === 'lowQualityInput' ? e.state.sample_rate : null,
    })
  }
}
export function stats(s: VoiceStatsPayload): Json {
  return {
    rttMs: s.rttMs,
    encoderBitrateBps: s.encoderBitrate,
    underruns: s.underruns,
    overruns: s.overruns,
    processingLatencyMs: s.processingLatencyMs,
    processingOverBudget: s.processingOverBudget,
    outputRms: s.outputRms,
    omittedSpeakers: Math.max(0, s.speakers.length - 32),
    speakers: s.speakers.slice(0, 32).map((p) => ({
      jitterDelayMs: p.jitterDelayMs,
      lost: p.lost,
      plc: p.plc,
      speaking: p.speaking,
    })),
  }
}
export function context(audio: AudioEnvironment, shortcuts: Snapshot): Json {
  return {
    audio: { ...audio.preferences },
    devices: audio.devices.map((d) => ({ ...d })),
    microphonePermission: audio.microphone,
    backend: audio.preferences.apple ? 'Apple VoiceProcessingIO requested' : 'CPAL requested',
    format: missing(
      'Actual negotiated device format and buffer size are not exposed by this build',
    ),
    shortcuts: {
      backend: shortcuts.backend ?? 'Unavailable',
      preferences: JSON.parse(JSON.stringify(shortcuts.preferences)) as Json,
      running: shortcuts.running,
      suspended: shortcuts.suspended,
      permission: shortcuts.permission,
      systemBinding: shortcuts.systemBinding,
      mouse: shortcuts.mouse,
      limitations: shortcuts.limitations,
    },
    network: missing('Network addresses and credentials are never collected'),
  }
}
export function redact(text: string): string {
  return text
    .slice(0, 16000)
    .replace(
      /-----BEGIN [^-]*PRIVATE KEY-----[\s\S]*?-----END [^-]*PRIVATE KEY-----/g,
      '[redacted private key]',
    )
    .replace(/\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b/gi, '[redacted email]')
    .replace(/\b(?:\d{1,3}\.){3}\d{1,3}(?::\d+)?\b/g, '[redacted address]')
    .replace(/\bBearer\s+[^\s,;]+/gi, 'Bearer [redacted]')
    .replace(
      /\b(password|passwd|token|secret|authorization|cookie|invite|api[_-]?key|session)\b["']?\s*[:=]\s*(?:"[^"]*"|'[^']*'|[^\s,;]+)/gi,
      '$1=[redacted]',
    )
    .replace(/(?:https?|socialspeak):\/\/[^\s<>]+/gi, '[redacted URL]')
    .replace(/\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\b/g, '[redacted token]')
}
function clean(value: Json, omit: boolean): Json {
  if (typeof value === 'string') return redact(value)
  if (Array.isArray(value)) return value.map((v) => clean(v, omit))
  if (value && typeof value === 'object')
    return Object.fromEntries(
      Object.entries(value).map(([k, v]) => [
        k,
        omit && ['devices', 'input', 'output', 'network'].includes(k)
          ? missing('Omitted identifying details')
          : clean(v, omit),
      ]),
    )
  return value
}
export function acousticResult(run: Run): Json {
  const a = run.acoustic
  if (
    !a.calibration.trim() ||
    !a.evidence.trim() ||
    a.sampleRateHz === null ||
    a.sampleRateHz <= 0 ||
    a.departureSample === null ||
    a.arrivalSample === null ||
    a.skewSamples === null
  )
    return missing('No complete calibrated acoustic measurement supplied')
  const ms = ((a.arrivalSample - a.departureSample - a.skewSamples) / a.sampleRateHz) * 1000
  return ms >= 0 && Number.isFinite(ms)
    ? {
        value: ms,
        unit: 'ms',
        origin: 'Human-supplied waveform indices; not independently verified',
      }
    : missing('Invalid measurement indices or calibration')
}
export function preview(run: Run, omit = true): Json {
  return clean(
    JSON.parse(
      JSON.stringify({
        ...run,
        acousticResult: acousticResult(run),
        recording: { capturedByApp: false, included: false },
        measurementNotes: {
          origin: 'Client voice stats, emitted every 500 ms; events timestamped on webview receipt',
          counters:
            'Underruns, overruns, lost, PLC and over-budget counts are cumulative for the current worker/track; reset on reopen',
          rttMs: 'Smoothed QUIC network round-trip, not mouth-to-ear',
          processingLatencyMs: 'Processing-chain estimate, not acoustic latency',
          jitterDelayMs: 'Current per-speaker applied buffer delay',
          outputRms: 'Latest mixed-output linear RMS',
          transmit:
            'Local speaking indicator observed at 40 ms native cadence; not proof that a remote listener heard audio',
          retention: `Last ${LIMIT} events, up to 4096 characters each and 32 speakers per stats sample; omittedSpeakers counts excess speakers. droppedEvents counts discarded oldest entries; suspended webviews may delay delivery`,
          missingStats: run.events.some((e) => e.kind === 'voice-stats')
            ? null
            : 'No voice statistics observed during this run',
        },
        privacy: {
          identifyingDetailsOmitted: omit,
          logs: 'Allowlisted semantic events only; no raw application logs',
          notes:
            'Review free text before sharing; automatic redaction cannot recognize every secret',
        },
      }),
    ) as Json,
    omit,
  )
}
export function validateReport(value: unknown): value is Run {
  if (!value || typeof value !== 'object') return false
  const r = value as Partial<Run>
  return (
    r.schemaVersion === 1 &&
    typeof r.runId === 'string' &&
    !!r.testId &&
    r.testId in definitions &&
    Array.isArray(r.events) &&
    r.events.length <= LIMIT &&
    Array.isArray(r.steps) &&
    r.steps.every(
      (s) => s && typeof s === 'object' && ['pass', 'fail', 'not-run'].includes(s.outcome),
    )
  )
}
