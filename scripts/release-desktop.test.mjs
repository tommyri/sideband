import assert from 'node:assert/strict'
import { createHash, generateKeyPairSync, randomBytes, sign } from 'node:crypto'
import { mkdtemp, mkdir, readFile, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { validateVersion } from './build-test-desktop.mjs'
import {
  prepareAssets, publishRelease, selectPlatforms, selectRelease, publishFeeds, signUpdaters, verifyUpdaterSignature,
} from './release-desktop.mjs'

const commit = 'a'.repeat(40)
const b64 = (value) => Buffer.from(value).toString('base64')

// Real `tauri signer` 2.12.4 output for the bytes "payload bytes" (disposable key).
const cli = {
  publicKey: 'dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDE0Qzk4NkMyN0VEMjU4REIKUldUYldOSit3b2JKRlBiaHJ2R3lMdkNSSVloUlZnc0Q3Q0ZWd2Rsb1NQQzc4UVpkQlBOeEo2TWMK',
  signature: 'dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVUYldOSit3b2JKRkJmN285M0dNZUhzUkhqR2FYdTNYcXBsOWs2UlRNNUN5NDk3TENzanRqTzJoVmdQWDkvYTBqNklpTUJjU1VRUzlNSlREU2p4UEdET05KUVJSYU1nb3djPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzk0MzQyMzI0CWZpbGU6cGF5bG9hZC5iaW4JdmVyc2lvbjoxLjIuMwpWMll0MG9LeDRaazhOd0tlVEdVdmRNdmpMUEl3ZXhIL3hxRTRNQzBWMXljclJteXYvejdEaWNCV2JablVPd2JmTXUzUjE1MG5WYkl0eGFGUEVKR3hBQT09Cg==',
  unversioned: 'dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVUYldOSit3b2JKRkJEZGlrMm1aWEJiNjRiOWxJMUVObDFsclVsbE11Nms5bTB2QU1TSW5la2pnUkhnZGhZU3hlTldFTEdHcWFCRnBsYnNTRVZ5Ymh0VDVIelJFSzFtZVFvPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzk0MzQyMDc1CWZpbGU6cGF5bG9hZC5iaW4KbzVaSDZEWEVxWkd2Sm5BUUJkUXhwWHdCZWMycU0zMURMVmJra00vaDcreTE2ZytTdFBSbTFYTG44VFlLTVFLY0owVmlVeEVNOVR0UjMzVjU3SHBvRGc9PQo=',
}

// The same minisign format, from a key generated per test run.
function signer() {
  const { publicKey, privateKey } = generateKeyPairSync('ed25519')
  const id = randomBytes(8), raw = Buffer.from(publicKey.export({ format: 'jwk' }).x, 'base64url')
  return {
    publicKey: b64(`untrusted comment: minisign public key: test\n${b64(Buffer.concat([Buffer.from('Ed'), id, raw]))}\n`),
    sign(bytes, version) {
      const signature = sign(null, createHash('blake2b512').update(bytes).digest(), privateKey)
      const trusted = `timestamp:1\tfile:payload\tversion:${version}`
      const global = sign(null, Buffer.concat([signature, Buffer.from(trusted)]), privateKey)
      return b64(`untrusted comment: signature from tauri secret key\n${b64(Buffer.concat([Buffer.from('ED'), id, signature]))}\ntrusted comment: ${trusted}\n${b64(global)}\n`)
    },
  }
}

test('updater signatures are checked against the public key and the signed version', () => {
  const bytes = Buffer.from('payload bytes')
  verifyUpdaterSignature({ bytes, signature: cli.signature, publicKey: cli.publicKey, version: '1.2.3' })
  assert.throws(() => verifyUpdaterSignature({ bytes, signature: cli.signature, publicKey: cli.publicKey, version: '1.2.4' }), /signed for version 1\.2\.3/)
  assert.throws(() => verifyUpdaterSignature({ bytes: Buffer.from('payload bytez'), signature: cli.signature, publicKey: cli.publicKey, version: '1.2.3' }), /does not verify/)
  assert.throws(() => verifyUpdaterSignature({ bytes, signature: cli.unversioned, publicKey: cli.publicKey, version: '1.2.3' }), /no signed version/)
  const other = signer()
  assert.throws(() => verifyUpdaterSignature({ bytes, signature: cli.signature, publicKey: other.publicKey, version: '1.2.3' }), /different key/)
  verifyUpdaterSignature({ bytes, signature: other.sign(bytes, '0.1.0'), publicKey: other.publicKey, version: '0.1.0' })
  assert.throws(() => verifyUpdaterSignature({ bytes, signature: b64('garbage'), publicKey: other.publicKey, version: '0.1.0' }), /signature/)
})

test('release selection validates the public updater configuration without signing secrets', () => {
  const env = { RELEASE_VERSION: '0.1.0', RELEASE_MACOS: 'true', RELEASE_UPDATES: 'true', GITHUB_REPOSITORY: 'owner/repo', TAURI_UPDATER_PUBLIC_KEY: cli.publicKey }
  assert.deepEqual(selectRelease(env).include.map(x => x.target), ['aarch64-apple-darwin'])
  assert.throws(() => selectRelease({ ...env, TAURI_UPDATER_PUBLIC_KEY: 'x' }), /public key/)
  assert.equal(selectRelease({ ...env, RELEASE_UPDATES: 'false', TAURI_UPDATER_PUBLIC_KEY: '' }).include.length, 1)
})

test('signing runs only the Tauri signer with the key, for checksummed payloads of this version', async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'sideband-sign-test-'))
  const pkg = path.join(root, 'linux')
  await mkdir(pkg)
  const bytes = Buffer.from('appimage'), sha256 = createHash('sha256').update(bytes).digest('hex')
  await writeFile(path.join(pkg, 'a.AppImage'), bytes)
  const identity = { version: '0.1.0', target: 'x86_64-unknown-linux-gnu', artifacts: [{ file: 'a.AppImage', bytes: bytes.length, sha256 }], updater: [{ file: 'a.AppImage' }] }
  await writeFile(path.join(pkg, 'build-info.json'), JSON.stringify(identity))
  const key = signer(), calls = []
  const run = async (command, args, options) => {
    calls.push({ command, args, env: options.env })
    await writeFile(args.at(-1) + '.sig', key.sign(await readFile(args.at(-1)), args[args.indexOf('--app-version') + 1]))
  }
  const env = { PATH: '/bin', GH_TOKEN: 'token', TAURI_SIGNING_PRIVATE_KEY: 'private', TAURI_SIGNING_PRIVATE_KEY_PASSWORD: 'password', TAURI_UPDATER_PUBLIC_KEY: key.publicKey }
  await signUpdaters({ input: root, version: '0.1.0', env, cli: '/cli/tauri.js', run })
  assert.equal(calls.length, 1)
  assert.deepEqual(calls[0].args, ['/cli/tauri.js', 'signer', 'sign', '--app-version', '0.1.0', path.join(pkg, 'a.AppImage')])
  assert.deepEqual(Object.keys(calls[0].env).sort(), ['CI', 'PATH', 'TAURI_SIGNING_PRIVATE_KEY', 'TAURI_SIGNING_PRIVATE_KEY_PASSWORD'])
  await assert.rejects(signUpdaters({ input: root, version: '0.1.0', env: { ...env, TAURI_UPDATER_PUBLIC_KEY: signer().publicKey }, cli: '/cli/tauri.js', run }), /different key/)
  await assert.rejects(signUpdaters({ input: root, version: '0.1.1', env, cli: '/cli/tauri.js', run }), /version/)
  await assert.rejects(signUpdaters({ input: root, version: '0.1.0', env: { ...env, TAURI_SIGNING_PRIVATE_KEY: '' }, cli: '/cli/tauri.js', run }), /TAURI_SIGNING_PRIVATE_KEY/)
  await writeFile(path.join(pkg, 'a.AppImage'), 'corrupted')
  await assert.rejects(signUpdaters({ input: root, version: '0.1.0', env, cli: '/cli/tauri.js', run }), /checksum/)
})
test('selection schedules only requested operating systems and rejects empty selection', () => {
  assert.deepEqual(selectPlatforms({ windows: true }).include.map(x => x.target), ['x86_64-pc-windows-msvc'])
  assert.equal(selectPlatforms({ windows: true, macos: true, linux: true }).include.length, 3)
  assert.throws(() => selectPlatforms({}))
  assert.equal(validateVersion('0.0.1'), '0.0.1')
  for (const value of ['v0.0.1', '0.01.1', '1.2', '../1.0.0', '1.2.65536', '256.0.0', '1.256.0', '1.2.3;echo bad', undefined]) assert.throws(() => validateVersion(value))
})

test('a one-platform release finds its build output without a subdirectory', async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'sideband-release-single-'))
  const input = path.join(root, 'input'), output = path.join(root, 'output')
  await mkdir(input, { recursive: true })
  const bytes = Buffer.from('fixture installer'), sha256 = createHash('sha256').update(bytes).digest('hex')
  const identity = { version: '0.0.1', commit, target: 'x86_64-pc-windows-msvc', dirty: false, profile: 'release', origin: 'github-actions',
    artifacts: ['installer.exe', 'installer.msi'].map(file => ({ file, bytes: bytes.length, sha256 })) }
  await writeFile(path.join(input, 'installer.exe'), bytes)
  await writeFile(path.join(input, 'installer.msi'), bytes)
  await writeFile(path.join(input, 'INSTALL.md'), 'Instructions')
  await writeFile(path.join(input, 'build-info.json'), JSON.stringify(identity))
  const files = await prepareAssets(input, output, '0.0.1', commit)
  assert(files.some(file => file.endsWith('Sideband-0.0.1-windows-x64-setup.exe')))
})

test('assets have stable platform-specific names and reject corrupt or mismatched builds', async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'sideband-release-test-'))
  const input = path.join(root, 'input'), pkg = path.join(input, 'windows'), output = path.join(root, 'output')
  await mkdir(pkg, { recursive: true })
  const bytes = Buffer.from('fixture installer'), sha256 = createHash('sha256').update(bytes).digest('hex')
  const identity = { version: '0.0.1', commit, target: 'x86_64-pc-windows-msvc', dirty: false, profile: 'release', origin: 'github-actions', artifacts: ['installer.exe', 'installer.msi'].map(file => ({ file, bytes: bytes.length, sha256 })) }
  await writeFile(path.join(pkg, 'installer.exe'), bytes)
  await writeFile(path.join(pkg, 'installer.msi'), bytes)
  await writeFile(path.join(pkg, 'INSTALL.md'), 'Instructions')
  await writeFile(path.join(pkg, 'build-info.json'), JSON.stringify(identity))
  const files = await prepareAssets(input, output, '0.0.1', commit)
  assert(files.some(file => file.endsWith('Sideband-0.0.1-windows-x64-setup.exe')))
  assert.match(await readFile(path.join(output, 'Sideband-0.0.1-windows-x64-SHA256SUMS'), 'utf8'), new RegExp(sha256))
  identity.updater = ['installer.exe', 'installer.msi'].map(file => ({ file }))
  await writeFile(path.join(pkg, 'build-info.json'), JSON.stringify(identity))
  const key = signer()
  await assert.rejects(prepareAssets(input, output, '0.0.1', commit, 'owner/repo', undefined, key.publicKey), /signature/)
  for (const file of ['installer.exe', 'installer.msi']) await writeFile(path.join(pkg, file + '.sig'), key.sign(bytes, '0.0.1'))
  await assert.rejects(prepareAssets(input, output, '0.0.1', commit, 'owner/repo', undefined, signer().publicKey), /different key/)
  await assert.rejects(prepareAssets(input, output, '0.0.1', commit, 'owner/repo'), /public key/)
  await prepareAssets(input, output, '0.0.1', commit, 'owner/repo', undefined, key.publicKey)
  const feed = JSON.parse(await readFile(path.join(output, identity.target + '.json'), 'utf8'))
  assert.equal(feed.platforms['windows-x86_64-nsis'].signature, (await readFile(path.join(pkg, 'installer.exe.sig'), 'utf8')).trim())
  assert(feed.platforms['windows-x86_64-nsis'].url.includes(commit))
  assert.equal(Object.keys(feed.platforms).length, 2)
  await assert.rejects(prepareAssets(input, output, '0.0.2', commit), /identity/)
  await writeFile(path.join(pkg, 'installer.exe'), 'corrupt')
  await assert.rejects(prepareAssets(input, output, '0.0.1', commit), /checksum/)
})

test('same-version publication moves the tag, edits the prerelease and clobbers only supplied assets', () => {
  const calls = []
  const run = args => {
    calls.push(args)
    if (args[0] === 'api' && args[1].includes('/git/ref/')) return JSON.stringify({ object: { sha: 'b'.repeat(40) } })
    if (args[0] === 'api' && args[1].includes('/releases/tags/')) return '{"id":1}'
    return ''
  }
  publishRelease({ repo: 'owner/sideband', commit, version: '0.0.1', files: ['windows.exe'], notes: 'notes.md', run })
  assert(calls.some(args => args.includes('force=true')))
  assert(calls.some(args => args[0] === 'release' && args[1] === 'edit'))
  assert.deepEqual(calls.at(-1), ['release', 'upload', 'v0.0.1', 'windows.exe', '--repo', 'owner/sideband', '--clobber'])
})

test('first publication creates a tag and prerelease; authentication errors abort', () => {
  const calls = []
  const run = args => {
    calls.push(args)
    if (args[0] === 'api' && args[1].startsWith('repos/')) {
      const error = new Error('Not found'); error.stderr = 'gh: Not Found (HTTP 404)'; throw error
    }
    return ''
  }
  publishRelease({ repo: 'owner/sideband', commit, version: '0.0.1', files: ['windows.exe'], notes: 'notes.md', run })
  assert(calls.some(args => args.includes('POST') && args.includes('ref=refs/tags/v0.0.1')))
  assert(calls.some(args => args[0] === 'release' && args[1] === 'create'))
  assert.throws(() => publishRelease({ repo: 'owner/sideband', commit, version: '0.0.1', files: [], notes: 'notes.md', run: () => { throw new Error('Unauthorized') } }), /Unauthorized/)
})


test('platform feeds never regress and other platforms are not overwritten', async () => {
  const dir = await mkdtemp(path.join(tmpdir(), 'sideband-feed-test-'))
  const file = path.join(dir, 'x86_64-pc-windows-msvc.json')
  const calls = []
  const run = args => {
    calls.push(args)
    if (args[0] === 'api') return JSON.stringify({ assets: [{ name: path.basename(file) }] })
    if (args[1] === 'download') return JSON.stringify({ version: '0.0.2' })
    return ''
  }
  await writeFile(file, JSON.stringify({ version: '0.0.1' }))
  await publishFeeds({ repo: 'owner/repo', commit, feeds: [file], run })
  assert(!calls.some(args => args[1] === 'upload'))
  await writeFile(file, JSON.stringify({ version: '0.0.2' }))
  await publishFeeds({ repo: 'owner/repo', commit, feeds: [file], run })
  assert.deepEqual(calls.at(-1), ['release', 'upload', 'desktop-updates', file, '--repo', 'owner/repo', '--clobber'])
})

test('alpha version, signed updater endpoint and Windows product version advance together', async () => {
  const { nativeVersion, feedTag } = await import('./desktop-version.mjs')
  const { makeIdentity, updateConfiguration } = await import('./build-test-desktop.mjs')
  const env = { GITHUB_REPOSITORY: 'owner/repo', TAURI_UPDATER_PUBLIC_KEY: cli.publicKey }
  const versions = ['0.1.4-alpha.1', '0.1.4-alpha.2', '0.1.4-alpha.10', '0.1.4', '0.1.5-alpha.1']
  assert.deepEqual(versions.map(nativeVersion), ['0.1.400', '0.1.401', '0.1.409', '0.1.499', '0.1.500'])
  assert.equal(nativeVersion('0.1.1'), '0.1.1')
  assert.equal(feedTag('0.1.4'), 'desktop-updates')
  const target = 'x86_64-pc-windows-msvc', version = versions[0]
  assert.equal(makeIdentity({ version, commit, target, status: '' }).channel, 'alpha')
  assert.deepEqual(updateConfiguration(env, target, version).plugins.updater.endpoints,
    ['https://github.com/owner/repo/releases/download/desktop-alpha-updates/x86_64-pc-windows-msvc.json'])
  assert.throws(() => selectRelease({ RELEASE_VERSION: version, RELEASE_WINDOWS: 'true' }), /Publish alpha/)
  for (const invalid of ['0.1.4-alpha.01', '0.1.4-alpha.100', '0.1.4-alpha.0', '0.1.4-beta.1']) assert.throws(() => validateVersion(invalid))
})

test('alpha feeds reject cross-channel updates and compare alpha numbers numerically', async () => {
  const dir = await mkdtemp(path.join(tmpdir(), 'sideband-alpha-feed-'))
  const file = path.join(dir, 'aarch64-apple-darwin.json')
  const calls = []
  const run = args => {
    calls.push(args)
    if (args[0] === 'api') return JSON.stringify({ assets: [{ name: path.basename(file) }] })
    if (args[1] === 'download') return JSON.stringify({ version: '0.1.4-alpha.10' })
    return ''
  }
  const input = { repo: 'owner/repo', commit, feeds: [file], run }
  await writeFile(file, JSON.stringify({ version: '0.1.4-alpha.2' }))
  await assert.rejects(publishFeeds(input), /different feed/)
  await publishFeeds({ ...input, tag: 'desktop-alpha-updates' })
  assert(!calls.some(args => args[1] === 'upload'))
  await writeFile(file, JSON.stringify({ version: '0.1.4-alpha.11' }))
  await publishFeeds({ ...input, tag: 'desktop-alpha-updates' })
  assert.deepEqual(calls.at(-1).slice(0, 3), ['release', 'upload', 'desktop-alpha-updates'])
})

test('a published alpha retry restores its original manifests and refuses a moved tag', async () => {
  const { publishAlpha } = await import('./release-desktop.mjs')
  const dir = await mkdtemp(path.join(tmpdir(), 'sideband-alpha-retry-'))
  const manifests = ['aarch64-apple-darwin.json', 'x86_64-pc-windows-msvc.json'].map(name => path.join(dir, name))
  const version = '0.1.4-alpha.1', original = JSON.stringify({ version, notes: 'original' })
  const calls = []
  const run = args => {
    calls.push(args)
    if (args[1].includes('/git/ref/')) return JSON.stringify({ object: { sha: commit } })
    if (args[0] === 'api') return JSON.stringify({ draft: false })
    if (args[1] === 'download') return original
    throw new Error('Unexpected mutation')
  }
  const input = { version, commit, repo: 'owner/repo', manifests, files: [], notes: '', run }
  await publishAlpha(input)
  for (const file of manifests) assert.equal(await readFile(file, 'utf8'), original)
  await assert.rejects(publishAlpha({ ...input, commit: 'b'.repeat(40) }), /another commit/)
})
