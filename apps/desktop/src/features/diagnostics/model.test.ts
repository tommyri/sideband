import { expect, it } from 'vitest'
import { graphPaths, metrics, report, retain, type Sample } from './model'
import { fixture } from './fixture'
it('bounds graphs by both time and sample count', () => {
  let samples: Sample[] = []
  for (let i = 0; i < 140; i++) samples = retain(samples, { at: i * 500, snapshot: fixture })
  expect(samples).toHaveLength(120)
  samples = retain(samples, { at: 200000, snapshot: fixture })
  expect(samples).toHaveLength(1)
})
it('breaks graphs at unavailable data and delayed polls', () => {
  const gap = { ...fixture, transport: null }
  const samples = [
    { at: 100, snapshot: fixture },
    { at: 600, snapshot: gap },
    { at: 1100, snapshot: fixture },
    { at: 5000, snapshot: fixture },
  ]
  expect(graphPaths(samples, metrics[0].read, 5000).paths).toHaveLength(3)
  expect(metrics[1].read(fixture)).toBe(32)
  expect(metrics[2].read(fixture)).toBeNull()
})
it('exports the shared schema, redaction, no recordings and truthful missing latency', () => {
  const value = report(
    { commit: 'test' },
    { devices: [{ name: 'Private device' }], audio: { input: 'private' } },
    [{ at: 10, snapshot: fixture }],
    'password=seed-password token=seed-token invite=seed-invite',
    20,
  ) as Record<string, unknown>
  const text = JSON.stringify(value)
  for (const secret of [
    'seed-password',
    'seed-token',
    'seed-invite',
    'Private device',
    '"private"',
  ])
    expect(text).not.toContain(secret)
  expect(value.testId).toBe('diagnostics')
  expect(value.measurementNotes).toMatchObject({ missingStats: null })
  expect(value.acousticResult).toMatchObject({ value: null })
  expect(value.recording).toMatchObject({ included: false, capturedByApp: false })
  expect(value.steps).toEqual([expect.objectContaining({ outcome: 'not-run' })])
})
