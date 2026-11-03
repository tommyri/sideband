import assert from 'node:assert/strict'
import test from 'node:test'
import { makeIdentity, parseOptions } from './build-test-desktop.mjs'

test('bundle formats and targets cannot silently cross operating systems', () => {
  assert.deepEqual(parseOptions([], 'x86_64-pc-windows-msvc', 'win32').bundles, ['nsis'])
  assert.deepEqual(parseOptions(['--debug', '--bundles', 'app'], 'aarch64-apple-darwin', 'darwin'),
    { target: 'aarch64-apple-darwin', bundles: ['app'], debug: true })
  assert.throws(() => parseOptions(['--target', 'x86_64-pc-windows-msvc'], 'aarch64-apple-darwin', 'darwin'))
  assert.throws(() => parseOptions(['--bundles', 'exe;unexpected'], 'x86_64-pc-windows-msvc', 'win32'))
  assert.throws(() => parseOptions(['--target'], 'aarch64-apple-darwin', 'darwin'))
  assert.throws(() => parseOptions(['--bundles'], 'aarch64-apple-darwin', 'darwin'))
})

test('identity preserves exact source provenance without including status paths', () => {
  const input = { version: '0.1.0', commit: 'a'.repeat(40), target: 'aarch64-apple-darwin',
    status: ' M private-file', debug: true, ci: false }
  const identity = makeIdentity(input)
  assert.equal(identity.dirty, true)
  assert.equal(identity.commit, input.commit)
  assert.equal(identity.profile, 'debug')
  assert.equal(identity.origin, 'local')
  assert.equal(JSON.stringify(identity).includes('private-file'), false)
  assert.equal(makeIdentity({ ...input, status: '', ci: true }).dirty, false)
  assert.throws(() => makeIdentity({ ...input, commit: 'unknown' }))
})

test('installer version override is validated without changing the target selection', () => {
  assert.equal(parseOptions(['--version', '0.0.1'], 'aarch64-apple-darwin', 'darwin').version, '0.0.1')
  assert.throws(() => parseOptions(['--version'], 'aarch64-apple-darwin', 'darwin'))
})
