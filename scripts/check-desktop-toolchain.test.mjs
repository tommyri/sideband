import assert from 'node:assert/strict'
import test from 'node:test'
import { toolchainProblems } from './check-desktop-toolchain.mjs'

const manifest = { packageManager: 'pnpm@12.18.0', engines: { node: '>=26.13.0 <27' } }

test('the pinned Node and pnpm pass', () => {
  assert.deepEqual(toolchainProblems(manifest, { node: 'v26.13.0', pnpm: '12.18.0' }), [])
  assert.deepEqual(toolchainProblems(manifest, { node: 'v26.20.1', pnpm: '12.18.0' }), [])
})

test('Node outside the engine range fails with both versions named', () => {
  for (const node of ['v24.22.0', 'v26.12.9', 'v27.0.0', 'v22.1.0']) {
    const [problem, ...rest] = toolchainProblems(manifest, { node, pnpm: '12.18.0' })
    assert.equal(rest.length, 0)
    assert.match(problem, new RegExp(`Node ${node.slice(1).replaceAll('.', '\\.')}.*>=26\\.13\\.0 <27`))
  }
})

test('any pnpm other than packageManager fails, because it rewrites the lockfile', () => {
  for (const pnpm of ['11.9.0', '12.18.1', '12.17.0', '13.0.0']) {
    const [problem] = toolchainProblems(manifest, { node: 'v26.13.0', pnpm })
    assert.match(problem, new RegExp(`pnpm ${pnpm.replaceAll('.', '\\.')}.*12\\.18\\.0.*pnpm-lock\\.yaml`))
  }
})

test('a missing pnpm is reported, not thrown', () => {
  const [problem] = toolchainProblems(manifest, { node: 'v26.13.0', pnpm: null })
  assert.match(problem, /pnpm was not found.*12\.18\.0/)
})

test('both problems are reported together', () => {
  assert.equal(toolchainProblems(manifest, { node: 'v24.22.0', pnpm: '11.9.0' }).length, 2)
})

test('a manifest without the pins is an error in the manifest', () => {
  assert.throws(() => toolchainProblems({}, { node: 'v26.13.0', pnpm: '12.18.0' }), /packageManager/)
  assert.throws(() => toolchainProblems({ packageManager: 'pnpm@12.18.0', engines: { node: '^26' } }, { node: 'v26.13.0', pnpm: '12.18.0' }), /engines\.node/)
})
