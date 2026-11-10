import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { access, mkdir, mkdtemp, readFile, stat, symlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import {
  assertNoSigningSecrets, buildEnvironment, createUpdaterArchive, makeIdentity, notarizationKey, parseOptions,
  parseUpdaterPublicKey, signMacos, updateConfiguration, updaterPayloads, writeManifest,
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
  for (const key of ['TAURI_SIGNING_PRIVATE_KEY', 'TAURI_SIGNING_PRIVATE_KEY_PASSWORD', 'TAURI_PRIVATE_KEY', 'APPLE_PASSWORD', 'APPLE_API_KEY',
    'APPLE_API_PRIVATE_KEY', 'APPLE_API_KEY_ID', 'APPLE_API_ISSUER', 'APPLE_CERTIFICATE', 'APPLE_CERTIFICATE_PASSWORD', 'WINDOWS_CERTIFICATE']) {
    assert.throws(() => assertNoSigningSecrets({ [key]: 'secret' }), new RegExp(key))
  }
  const env = { PATH: '/bin', APPLE_SIGNING_IDENTITY: 'Developer ID Application: X', CARGO_BUILD_TARGET: 'x', SS_UPDATE_ENDPOINT: 'stale', TAURI_UPDATER_PUBLIC_KEY: fixturePublicKey, GITHUB_REPOSITORY: 'owner/repo' }
  const identity = { version: '0.1.0' }
  const manual = buildEnvironment(env, { updates: false }, identity, '/target')
  assert.equal(manual.APPLE_SIGNING_IDENTITY, undefined)
  assert.equal(manual.SS_UPDATE_ENDPOINT, undefined)
  assert.equal(manual.CARGO_BUILD_TARGET, undefined)
  const release = buildEnvironment(env, { updates: true }, identity, '/target', updateConfiguration(env, 'aarch64-apple-darwin'))
  // macOS builds are always ad-hoc; the Developer ID signature is added in a separate job.
  assert.equal(release.APPLE_SIGNING_IDENTITY, undefined)
  assert.equal(updateConfiguration(env, 'aarch64-apple-darwin').bundle.macOS.signingIdentity, '-')
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

const apiKey = {
  APPLE_API_KEY_ID: 'ABCDE12345', APPLE_API_ISSUER: '69a6de7e-0000-47e3-e053-5b8c7c11a4d1',
  APPLE_API_PRIVATE_KEY: '-----BEGIN PRIVATE KEY-----\nTUlHVEFnRUFNQk1HQnlxR1NNNDlBZ0VHQ0NxR1NNNDlBd0VIQkhrd2R3SUJBUVFn\n-----END PRIVATE KEY-----\n',
}

test('notarization uses an API key; partial or malformed credentials fail', () => {
  assert.equal(notarizationKey({}), null)
  assert.deepEqual(notarizationKey(apiKey), { id: apiKey.APPLE_API_KEY_ID, issuer: apiKey.APPLE_API_ISSUER, key: apiKey.APPLE_API_PRIVATE_KEY })
  assert.throws(() => notarizationKey({ ...apiKey, APPLE_API_ISSUER: '' }), /all of/)
  assert.throws(() => notarizationKey({ ...apiKey, APPLE_API_KEY_ID: '../x' }), /key ID/)
  assert.throws(() => notarizationKey({ ...apiKey, APPLE_API_PRIVATE_KEY: 'password' }), /\.p8/)
  // The Apple ID path needs the app password on notarytool's command line.
  assert.throws(() => notarizationKey({ APPLE_ID: 'a@example.com', APPLE_PASSWORD: 'pw', APPLE_TEAM_ID: 'T' }), /API key/)
})

async function macosArtifacts() {
  const directory = await mkdtemp(path.join(tmpdir(), 'sideband-macos-sign-test-'))
  for (const [file, body] of [['Sideband.dmg', 'adhoc-dmg'], ['Sideband-test-0.1.0.app.zip', 'adhoc-zip'], ['Sideband-test-0.1.0.app.tar.gz', 'adhoc-tar'], ['INSTALL.md', 'install']]) {
    await writeFile(path.join(directory, file), body)
  }
  await writeManifest(directory, { version: '0.1.0', target: 'aarch64-apple-darwin' }, ['Sideband.dmg', 'Sideband-test-0.1.0.app.zip', 'Sideband-test-0.1.0.app.tar.gz'], ['Sideband-test-0.1.0.app.tar.gz'])
  return directory
}

function fakeTools(calls, { status = 'Accepted' } = {}) {
  return async (command, args) => {
    calls.push([command, ...args])
    const tool = path.basename(command)
    if (tool === 'ditto' && args[0] === '-x') await mkdir(path.join(args.at(-1), 'Sideband.app', 'Contents'), { recursive: true })
    else if (tool === 'ditto' && args[0] === '-c') await writeFile(args.at(-1), 'signed-zip')
    else if (tool === 'tar') await writeFile(args[args.indexOf('-czf') + 1], 'signed-tar')
    else if (tool === 'hdiutil') await writeFile(args.at(-1), 'signed-dmg')
    else if (tool === 'xcrun' && args[0] === 'notarytool') {
      const keyFile = args[args.indexOf('--key') + 1]
      assert.equal(await readFile(keyFile, 'utf8'), apiKey.APPLE_API_PRIVATE_KEY)
      assert.equal((await stat(keyFile)).mode & 0o777, 0o600)
      return JSON.stringify({ status, id: 'submission' })
    } else if (tool === 'xcrun' && args[0] === 'stapler') await writeFile(args.at(-1), 'signed-dmg+ticket')
    return ''
  }
}

test('macOS signing re-signs the app, rebuilds every package and notarizes with a key file, never argv secrets', async () => {
  const directory = await macosArtifacts()
  const env = { APPLE_SIGNING_IDENTITY: 'Developer ID Application: Example (TEAM)', APPLE_KEYCHAIN: '/tmp/k.keychain-db', ...apiKey }
  const calls = []
  await signMacos(directory, env, { run: fakeTools(calls), entitlements: '/repo/Entitlements.plist' })
  // System tools by absolute path: a PATH shim cannot stand in for them.
  assert(calls.every(([command]) => command.startsWith('/usr/bin/')))
  const signApp = calls.find(([command, ...args]) => command.endsWith('codesign') && args.at(-1).endsWith('Sideband.app') && args.includes('--sign'))
  for (const flag of ['--options', 'runtime', '--timestamp', '--entitlements', '/repo/Entitlements.plist', '--keychain', env.APPLE_KEYCHAIN, env.APPLE_SIGNING_IDENTITY]) assert(signApp.includes(flag), flag)
  const submit = calls.find(([, first]) => first === 'notarytool')
  assert.deepEqual(submit.slice(submit.indexOf('--key-id'), submit.indexOf('--key-id') + 4), ['--key-id', apiKey.APPLE_API_KEY_ID, '--issuer', apiKey.APPLE_API_ISSUER])
  const argv = calls.flat().join('\n')
  assert(!argv.includes('BEGIN PRIVATE KEY') && !argv.includes('--password') && !argv.includes('--apple-id'))
  await assert.rejects(access(submit[submit.indexOf('--key') + 1]))
  assert.deepEqual(calls.at(-1).slice(1), ['stapler', 'staple', path.join(directory, 'Sideband.dmg')])
  const info = JSON.parse(await readFile(path.join(directory, 'build-info.json'), 'utf8'))
  assert.equal(info.artifacts.find(a => a.file === 'Sideband.dmg').bytes, 'signed-dmg+ticket'.length)
  assert.equal(info.artifacts.find(a => a.file.endsWith('.app.tar.gz')).bytes, 'signed-tar'.length)
  assert.deepEqual(info.updater, [{ file: 'Sideband-test-0.1.0.app.tar.gz' }])
  assert.match(await readFile(path.join(directory, 'SHA256SUMS'), 'utf8'), /Sideband\.dmg/)
})

test('macOS signing without notarization keys skips notarytool; a rejected submission fails and still removes the key', async () => {
  const env = { APPLE_SIGNING_IDENTITY: 'Developer ID Application: Example (TEAM)', APPLE_KEYCHAIN: '/tmp/k.keychain-db' }
  const calls = []
  await signMacos(await macosArtifacts(), env, { run: fakeTools(calls), entitlements: '/repo/Entitlements.plist' })
  assert(!calls.some(([command]) => command.endsWith('xcrun')))
  const rejected = []
  await assert.rejects(signMacos(await macosArtifacts(), { ...env, ...apiKey }, { run: fakeTools(rejected, { status: 'Invalid' }), entitlements: '/repo/Entitlements.plist' }), /Invalid/)
  const submit = rejected.find(([, first]) => first === 'notarytool')
  await assert.rejects(access(submit[submit.indexOf('--key') + 1]))
  const tampered = await macosArtifacts()
  await writeFile(path.join(tampered, 'Sideband-test-0.1.0.app.zip'), 'changed in transit')
  await assert.rejects(signMacos(tampered, env, { run: fakeTools([]), entitlements: '/repo/Entitlements.plist' }), /checksum/)
  await assert.rejects(signMacos(await macosArtifacts(), { ...env, APPLE_KEYCHAIN: '' }, { run: fakeTools([]), entitlements: '/repo/Entitlements.plist' }), /APPLE_KEYCHAIN/)
})
