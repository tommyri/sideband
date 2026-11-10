import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { mkdir, mkdtemp, readFile, symlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import {
  assertNoSigningSecrets, buildEnvironment, createUpdaterArchive, makeIdentity, notarize, parseOptions,
  parseUpdaterPublicKey, updateConfiguration, updaterPayloads, writeManifest,
} from './build-test-desktop.mjs'

// A real `tauri signer generate` public key (disposable; its private key was discarded).
const fixturePublicKey = 'dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDE0Qzk4NkMyN0VEMjU4REIKUldUYldOSit3b2JKRlBiaHJ2R3lMdkNSSVloUlZnc0Q3Q0ZWd2Rsb1NQQzc4UVpkQlBOeEo2TWMK'

test('bundle formats and targets cannot silently cross operating systems', () => {
  assert.deepEqual(parseOptions([], 'x86_64-pc-windows-msvc', 'win32').bundles, ['nsis', 'msi'])
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


test('updater configuration needs only the public key and never asks Tauri to sign', () => {
  const env = { GITHUB_REPOSITORY: 'owner/repo', TAURI_UPDATER_PUBLIC_KEY: fixturePublicKey }
  assert.throws(() => updateConfiguration({}, 'aarch64-apple-darwin'), /TAURI_UPDATER_PUBLIC_KEY/)
  assert.throws(() => updateConfiguration({ ...env, TAURI_UPDATER_PUBLIC_KEY: 'not a key' }, 'aarch64-apple-darwin'), /public key/)
  assert.throws(() => updateConfiguration({ ...env, GITHUB_REPOSITORY: '../bad' }, 'aarch64-apple-darwin'))
  const config = updateConfiguration(env, 'aarch64-apple-darwin')
  // Updater artifacts would make `tauri build` demand TAURI_SIGNING_PRIVATE_KEY.
  assert.equal(config.bundle.createUpdaterArtifacts, false)
  assert.equal(config.bundle.macOS.signingIdentity, '-')
  assert.equal(config.plugins.updater.pubkey, fixturePublicKey)
  assert.equal(config.plugins.updater.endpoints[0], 'https://github.com/owner/repo/releases/download/desktop-updates/aarch64-apple-darwin.json')
  assert.equal(parseOptions(['--updates'], 'aarch64-apple-darwin', 'darwin').updates, true)
  assert.equal(parseUpdaterPublicKey(fixturePublicKey).keyId.toString('hex'), 'db58d27ec286c914')
})

test('the build step refuses signing secrets and never forwards them', () => {
  assertNoSigningSecrets({ PATH: '/bin', APPLE_SIGNING_IDENTITY: 'Developer ID Application: X', TAURI_SIGNING_PRIVATE_KEY: '' })
  for (const key of ['TAURI_SIGNING_PRIVATE_KEY', 'TAURI_SIGNING_PRIVATE_KEY_PASSWORD', 'TAURI_PRIVATE_KEY', 'APPLE_PASSWORD', 'APPLE_API_KEY', 'APPLE_CERTIFICATE_PASSWORD', 'WINDOWS_CERTIFICATE']) {
    assert.throws(() => assertNoSigningSecrets({ [key]: 'secret' }), new RegExp(key))
  }
  const env = { PATH: '/bin', APPLE_SIGNING_IDENTITY: 'Developer ID Application: X', CARGO_BUILD_TARGET: 'x', SS_UPDATE_ENDPOINT: 'stale', TAURI_UPDATER_PUBLIC_KEY: fixturePublicKey, GITHUB_REPOSITORY: 'owner/repo' }
  const identity = { version: '0.1.0' }
  const manual = buildEnvironment(env, { updates: false }, identity, '/target')
  assert.equal(manual.APPLE_SIGNING_IDENTITY, undefined)
  assert.equal(manual.SS_UPDATE_ENDPOINT, undefined)
  assert.equal(manual.CARGO_BUILD_TARGET, undefined)
  const release = buildEnvironment(env, { updates: true }, identity, '/target', updateConfiguration(env, 'aarch64-apple-darwin'))
  assert.equal(release.APPLE_SIGNING_IDENTITY, env.APPLE_SIGNING_IDENTITY)
  assert.equal(release.SS_UPDATE_PUBLIC_KEY, fixturePublicKey)
  assert.match(release.SS_UPDATE_ENDPOINT, /aarch64-apple-darwin\.json$/)
  assert.equal(JSON.parse(release.SS_BUILD_IDENTITY).version, '0.1.0')
})

test('each platform has exactly the updater payloads the feed needs', () => {
  assert.deepEqual(updaterPayloads(['a.dmg', 'a.app.zip', 'a.app.tar.gz'], 'darwin'), ['a.app.tar.gz'])
  assert.deepEqual(updaterPayloads(['a.msi', 'a-setup.exe'], 'win32'), ['a-setup.exe', 'a.msi'])
  assert.deepEqual(updaterPayloads(['a.deb', 'a.AppImage'], 'linux'), ['a.AppImage'])
  assert.throws(() => updaterPayloads(['a.dmg'], 'darwin'), /exactly one/)
  assert.throws(() => updaterPayloads(['a.AppImage', 'b.AppImage'], 'linux'), /exactly one/)
})

test('manifest lists unsigned updater payloads with checksums', async () => {
  const output = await mkdtemp(path.join(tmpdir(), 'sideband-manifest-test-'))
  await writeFile(path.join(output, 'a.AppImage'), 'appimage')
  await writeFile(path.join(output, 'a.deb'), 'deb')
  await writeFile(path.join(output, 'INSTALL.md'), 'install')
  const info = await writeManifest(output, { version: '0.1.0' }, ['a.deb', 'a.AppImage'], ['a.AppImage'])
  assert.deepEqual(info.updater, [{ file: 'a.AppImage' }])
  assert.equal(info.artifacts.find(a => a.file === 'a.deb').bytes, 3)
  const sums = await readFile(path.join(output, 'SHA256SUMS'), 'utf8')
  assert.match(sums, /  a\.AppImage\n/)
  assert.match(sums, /  build-info\.json\n/)
})

test('macOS updater archive has the app as its only root, without AppleDouble files', { skip: process.platform !== 'darwin' }, async () => {
  const dir = await mkdtemp(path.join(tmpdir(), 'sideband-archive-test-'))
  const app = path.join(dir, 'Sideband.app')
  await mkdir(path.join(app, 'Contents', 'MacOS'), { recursive: true })
  await writeFile(path.join(app, 'Contents', 'MacOS', 'sideband'), 'binary', { mode: 0o755 })
  await symlink('MacOS/sideband', path.join(app, 'Contents', 'link'))
  execFileSync('xattr', ['-w', 'com.example.detritus', 'x', path.join(app, 'Contents', 'MacOS', 'sideband')])
  const archive = path.join(dir, 'Sideband.app.tar.gz')
  createUpdaterArchive(app, archive)
  const entries = execFileSync('tar', ['-tzf', archive], { encoding: 'utf8' }).trim().split('\n')
  assert(entries.every(entry => entry.startsWith('Sideband.app/')))
  assert(!entries.some(entry => entry.includes('._')))
  assert(entries.includes('Sideband.app/Contents/link'))
})

test('notarization submits and staples the DMG, then refreshes checksums', async () => {
  const output = await mkdtemp(path.join(tmpdir(), 'sideband-notarize-test-'))
  await writeFile(path.join(output, 'a.dmg'), 'dmg')
  await writeFile(path.join(output, 'a.app.zip'), 'zip')
  await writeFile(path.join(output, 'INSTALL.md'), 'install')
  await writeManifest(output, { version: '0.1.0' }, ['a.dmg', 'a.app.zip'])
  const env = { APPLE_ID: 'id@example.com', APPLE_PASSWORD: 'app-password', APPLE_TEAM_ID: 'TEAM' }
  const calls = []
  const run = async (command, args) => {
    calls.push([command, ...args])
    if (args[0] === 'stapler') await writeFile(path.join(output, 'a.dmg'), 'dmg+ticket')
    return args[0] === 'notarytool' ? JSON.stringify({ status: 'Accepted', id: '1' }) : ''
  }
  await notarize(output, env, run)
  assert.deepEqual(calls[0].slice(0, 4), ['xcrun', 'notarytool', 'submit', path.join(output, 'a.dmg')])
  assert(calls[0].includes('--wait'))
  assert.deepEqual(calls[1], ['xcrun', 'stapler', 'staple', path.join(output, 'a.dmg')])
  const info = JSON.parse(await readFile(path.join(output, 'build-info.json'), 'utf8'))
  assert.equal(info.artifacts.find(a => a.file === 'a.dmg').bytes, 'dmg+ticket'.length)
  await assert.rejects(notarize(output, { ...env, APPLE_PASSWORD: '' }, run), /APPLE_PASSWORD/)
  await assert.rejects(notarize(output, env, async () => JSON.stringify({ status: 'Invalid', id: '2' })), /Invalid/)
})
