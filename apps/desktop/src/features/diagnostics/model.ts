import { Recorder, preview, type Json } from '../testing/report'
export interface Snapshot {
  transport: {
    rttMs: number
    lostPackets: number
    sentPackets: number
    congestionWindowBytes: number
    pathMtuBytes: number
    maxDatagramBytes: number | null
  } | null
  voice: {
    rttMs: number
    encoderBitrate: number
    underruns: number
    overruns: number
    processingLatencyMs: number
    processingOverBudget: number
    outputRms: number
    speakers: {
      track: number
      jitterDelayMs: number
      lost: number
      plc: number
      speaking: boolean
    }[]
  } | null
  details: {
    backend: string | null
    inputRateHz: number | null
    outputRateHz: number | null
    encoderFec: boolean | null
    speakers: { track: number; jitterDelayMs: number; lost: number; late: number; plc: number }[]
    processingStages: {
      stage: string
      backend: string
      frames: number
      lastMs: number
      meanMs: number
      maxMs: number
    }[]
  } | null
  unavailable: Record<string, string>
}
export interface Sample {
  at: number
  snapshot: Snapshot
}
export function retain(samples: Sample[], sample: Sample) {
  return [...samples.filter((s) => s.at > sample.at - 60_000), sample].slice(-120)
}
export const metrics = [
  { name: 'Network RTT', unit: 'ms', read: (s: Snapshot) => s.transport?.rttMs ?? null },
  {
    name: 'Encoder bitrate',
    unit: 'kbps',
    read: (s: Snapshot) => (s.voice ? s.voice.encoderBitrate / 1000 : null),
  },
  {
    name: 'Maximum speaker buffer delay',
    unit: 'ms',
    read: (s: Snapshot) =>
      s.details?.speakers.length
        ? Math.max(...s.details.speakers.map((p) => p.jitterDelayMs))
        : null,
  },
] as const
// Break paths across unavailable samples and delayed polls; never bridge a disconnect.
export function graphPaths(samples: Sample[], read: (s: Snapshot) => number | null, now: number) {
  const maximum = Math.max(1, ...samples.map((s) => read(s.snapshot) ?? 0))
  const paths: string[] = []
  let path = ''
  let previous = -Infinity
  for (const sample of samples) {
    const value = read(sample.snapshot)
    if (value === null || !Number.isFinite(value)) {
      if (path) paths.push(path)
      path = ''
      continue
    }
    if (sample.at - previous > 1500 && path) {
      paths.push(path)
      path = ''
    }
    const x = Math.max(0, Math.min(600, (sample.at - (now - 60000)) / 100))
    const y = 100 - (value / maximum) * 90
    path += `${path ? ' L' : 'M'}${x.toFixed(1)},${y.toFixed(1)}`
    previous = sample.at
  }
  if (path) paths.push(path)
  return { paths, maximum }
}
export function report(
  build: Json,
  context: Json,
  samples: Sample[],
  notes: string,
  now = performance.now(),
) {
  const r = new Recorder('diagnostics', build, context, samples[0]?.at ?? now)
  r.run.startedAt = new Date(Date.now() - (now - (samples[0]?.at ?? now))).toISOString()
  for (const { at, snapshot } of samples) {
    // Keep useful aggregate timelines within the T-076 per-event size cap.
    r.add(
      'diagnostics-sample',
      {
        transport: snapshot.transport,
        voice: snapshot.voice
          ? { ...snapshot.voice, speakers: snapshot.voice.speakers.slice(0, 16) }
          : null,
      } as Json,
      at,
    )
  }
  r.run.notes = notes
  r.finish({
    latest: samples.at(-1)?.snapshot ?? null,
    windowMs: 60000,
    sampleIntervalMs: 500,
    origin:
      'Transport from live Quinn path; voice from shared core stats; collection only while diagnostics is open',
    loss: 'Cumulative packet/frame counters, not a percentage; resets on reconnect or stream reopen',
    stages:
      'Measured software processing time per 10 ms frame, not total process CPU; Apple system stage CPU unavailable',
  } as Json)
  const result = preview(r.run) as Record<string, Json>
  result.measurementNotes = {
    ...(result.measurementNotes as Record<string, Json>),
    origin:
      'On-demand live Quinn path and shared voice-worker snapshots, polled about every 500 ms',
    missingStats: samples.some((s) => s.snapshot.voice !== null)
      ? null
      : 'No voice statistics observed during this diagnostics window',
  }
  return result
}

export const openDiagnostics = () => window.dispatchEvent(new Event('ss:open-diagnostics'))
