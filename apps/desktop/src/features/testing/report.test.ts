import { expect, it } from 'vitest'
import {
  acousticResult,
  begin,
  LIMIT,
  missing,
  observe,
  preview,
  Recorder,
  redact,
  validateReport,
} from './report'
import schema from './report.schema.json'
it('versions reports and defaults to human not-run outcomes', () => {
  const r = new Recorder('ptt-focus', { commit: 'full-sha' }, missing('No devices'))
  expect(validateReport(r.run)).toBe(true)
  expect(validateReport({ ...r.run, schemaVersion: 2 })).toBe(false)
  expect(validateReport({ ...r.run, steps: [{ outcome: 'automatic-pass' }] })).toBe(false)
  expect(r.run.steps.every((s) => s.outcome === 'not-run')).toBe(true)
  const report = preview(r.run) as Record<string, unknown>
  expect(schema.required.every((key) => key in report)).toBe(true)
  expect(report.acousticResult).toEqual(
    missing('No complete calibrated acoustic measurement supplied'),
  )
  expect(report.recording).toEqual({ capturedByApp: false, included: false })
})
it('bounds and orders retention and freezes finished runs', () => {
  const r = new Recorder('device-recovery', {}, {}, 100)
  for (let i = 0; i < LIMIT + 20; i++)
    r.add('connection', { state: 'connected' }, i % 2 ? 110 : 105)
  expect(r.run.events).toHaveLength(LIMIT)
  expect(r.run.droppedEvents).toBe(20)
  expect(r.run.events[0]?.sequence).toBe(21)
  expect(r.run.events.every((e, i) => !i || e.offsetMs >= r.run.events[i - 1]!.offsetMs)).toBe(true)
  r.finish(missing('No bridge'))
  r.add('ignored', {})
  expect(r.run.events.at(-1)?.kind).toBe('connection')
})
it('redacts seeded secrets and optional identifiers without mutating the source', () => {
  const secrets = [
    'seed-password',
    'seed-token',
    'seed-invite',
    'seed-auth',
    'seed-cookie',
    'seed-api',
    'seed-session',
  ]
  const r = new Recorder(
    'ptt-focus',
    {},
    {
      devices: [{ name: 'Private headset' }],
      audio: { input: 'private-id', output: 'private-out' },
    },
  )
  r.run.notes =
    'password=seed-password token=seed-token invite="seed-invite" Bearer seed-auth cookie=seed-cookie api_key=seed-api session=seed-session https://host/?token=secret 192.168.1.50 me@example.test'
  const output = JSON.stringify(preview(r.run))
  for (const value of [
    ...secrets,
    'Private headset',
    'private-id',
    'private-out',
    '192.168.1.50',
    'me@example.test',
    'https://host',
  ])
    expect(output).not.toContain(value)
  expect(r.run.notes).toContain('seed-password')
  expect(JSON.stringify(preview(r.run, false))).toContain('Private headset')
  expect(redact('-----BEGIN PRIVATE KEY-----\nseed\n-----END PRIVATE KEY-----')).not.toContain(
    'seed',
  )
})
it('allows only semantic events for the selected server', () => {
  const r = begin('device-recovery', {}, {})
  r.serverId = 'test'
  observe({ serverId: 'test', event: { kind: 'notice', message: 'SECRET' } } as never, 'test')
  observe(
    { serverId: 'test', event: { kind: 'messagesChanged', channel: 'SECRET' } } as never,
    'test',
  )
  observe(
    { serverId: 'other', event: { kind: 'connection', status: { kind: 'connected' } } } as never,
    'test',
  )
  observe(
    {
      serverId: 'test',
      event: { kind: 'voiceDevice', state: { kind: 'unavailable', message: 'SECRET' } },
    } as never,
    undefined,
  )
  expect(r.run.events).toHaveLength(1)
  expect(JSON.stringify(r.run)).not.toContain('SECRET')
})
it('requires calibrated evidence for acoustic delay', () => {
  const r = new Recorder('acoustic-latency', {}, {}).run
  r.acoustic = {
    sampleRateHz: 48000,
    departureSample: 0,
    arrivalSample: 4800,
    skewSamples: 48,
    calibration: '48 sample skew',
    evidence: 'trial-01.wav channels 1 and 2',
  }
  expect(acousticResult(r)).toMatchObject({ value: 99, unit: 'ms' })
  r.acoustic.evidence = ''
  expect(acousticResult(r)).toHaveProperty('value', null)
})

it('bounds individual payloads rather than retaining oversized diagnostics', () => {
  const r = new Recorder('ptt-focus', {}, {})
  r.add('diagnostic', { text: 'x'.repeat(5000) })
  expect(r.run.events[0]?.data).toEqual(missing('Event exceeded 4096-character retention limit'))
})
