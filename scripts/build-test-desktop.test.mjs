import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { deflateRawSync } from 'node:zlib'
import { access, mkdir, mkdtemp, readdir, readFile, stat, symlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import {
  assertNoSigningSecrets, buildEnvironment, checkAppArchive, checkExtractedApp, createUpdaterArchive, listZip, makeIdentity,
  notarizationKey, parseOptions, parseUpdaterPublicKey, signMacos, updateConfiguration, updaterPayloads, writeManifest,
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

// Minimal stored-entry ZIP writer, so crafted archives need no external tool.
const crcTable = Array.from({ length: 256 }, (_, n) => {
  let c = n
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1
  return c >>> 0
})
const crc32 = (bytes) => { let c = 0xffffffff; for (const b of bytes) c = crcTable[(c ^ b) & 0xff] ^ (c >>> 8); return (c ^ 0xffffffff) >>> 0 }

// entries: [name, kind ('dir' | 'file' | 'link'), content or link target, options]
// options: deflate, descriptor (bit 3 with a data descriptor, as ditto writes files),
// size (declared uncompressed size), patch(local, central) to forge header fields.
function zip(entries) {
  const locals = [], centrals = []
  let offset = 0
  for (const [name, kind, content = '', options = {}] of entries) {
    const nameBytes = Buffer.from(name), data = Buffer.from(kind === 'dir' ? '' : content)
    const stored = options.deflate ? deflateRawSync(data) : data
    const method = options.deflate ? 8 : 0, flags = 0x800 | (options.descriptor ? 0x8 : 0)
    const crc = crc32(data), size = options.size ?? data.length
    const mode = kind === 'dir' ? 0o40755 : kind === 'link' ? 0o120755 : 0o100644
    const local = Buffer.alloc(30)
    local.writeUInt32LE(0x04034b50, 0); local.writeUInt16LE(20, 4); local.writeUInt16LE(flags, 6); local.writeUInt16LE(method, 8)
    if (!options.descriptor) { local.writeUInt32LE(crc, 14); local.writeUInt32LE(stored.length, 18); local.writeUInt32LE(size, 22) }
    local.writeUInt16LE(nameBytes.length, 26)
    const central = Buffer.alloc(46)
    central.writeUInt32LE(0x02014b50, 0); central.writeUInt16LE(0x0314, 4); central.writeUInt16LE(20, 6)
    central.writeUInt16LE(flags, 8); central.writeUInt16LE(method, 10)
    central.writeUInt32LE(crc, 16); central.writeUInt32LE(stored.length, 20); central.writeUInt32LE(size, 24)
    central.writeUInt16LE(nameBytes.length, 28); central.writeUInt32LE(((mode << 16) | (kind === 'dir' ? 0x10 : 0)) >>> 0, 38)
    central.writeUInt32LE(offset, 42)
    const descriptor = Buffer.alloc(options.descriptor ? 16 : 0)
    if (options.descriptor) {
      descriptor.writeUInt32LE(0x08074b50, 0); descriptor.writeUInt32LE(crc, 4)
      descriptor.writeUInt32LE(stored.length, 8); descriptor.writeUInt32LE(size, 12)
    }
    options.patch?.(local, central, descriptor)
    locals.push(local, nameBytes, stored, descriptor)
    centrals.push(central, nameBytes)
    offset += local.length + nameBytes.length + stored.length + descriptor.length
  }
  const directory = Buffer.concat(centrals), end = Buffer.alloc(22)
  end.writeUInt32LE(0x06054b50, 0); end.writeUInt16LE(entries.length, 8); end.writeUInt16LE(entries.length, 10)
  end.writeUInt32LE(directory.length, 12); end.writeUInt32LE(offset, 16)
  return Buffer.concat([...locals, directory, end])
}

const appBundle = [
  ['Sideband.app/', 'dir'], ['Sideband.app/Contents/', 'dir'], ['Sideband.app/Contents/MacOS/', 'dir'],
  ['Sideband.app/Contents/MacOS/sideband', 'file', 'binary', { deflate: true, descriptor: true }],
  ['Sideband.app/Contents/Frameworks/', 'dir'], ['Sideband.app/Contents/Frameworks/Kit.framework/', 'dir'],
  ['Sideband.app/Contents/Frameworks/Kit.framework/Versions/', 'dir'], ['Sideband.app/Contents/Frameworks/Kit.framework/Versions/A/', 'dir'],
  ['Sideband.app/Contents/Frameworks/Kit.framework/Versions/A/Kit', 'file', 'lib'],
  ['Sideband.app/Contents/Frameworks/Kit.framework/Versions/Current', 'link', 'A'],
  ['Sideband.app/Contents/Frameworks/Kit.framework/Kit', 'link', 'Versions/Current/Kit'],
  ['Sideband.app/Contents/Frameworks/Kit.framework/Resources', 'link', 'Versions/Current/Resources'],
  ['Sideband.app/Contents/Frameworks/Kit.framework/Versions/A/Resources/', 'dir'],
]

test('the app ZIP is listed and accepted only as one .app with internal symlinks', () => {
  const entries = listZip(zip(appBundle))
  assert.deepEqual(entries.find((entry) => entry.name.endsWith('Versions/Current')), { name: 'Sideband.app/Contents/Frameworks/Kit.framework/Versions/Current', type: 'symlink', mode: 0o755, target: 'A' })
  assert.equal(checkAppArchive(entries), 'Sideband.app')
  const refuse = (extra, pattern) => assert.throws(() => checkAppArchive(listZip(zip([...appBundle, ...extra]))), pattern)
  refuse([['/etc/passwd', 'file', 'x']], /absolute/)
  refuse([['Sideband.app/../escape', 'file', 'x']], /\.\./)
  refuse([['../escape', 'file', 'x']], /\.\./)
  // A symlink to / or $HOME followed by a file written through it.
  refuse([['Sideband.app/Contents/root', 'link', '/'], ['Sideband.app/Contents/root/tmp/owned', 'file', 'x']], /absolute/)
  refuse([['Sideband.app/Contents/home', 'link', process.env.HOME ?? '/Users/runner'], ['Sideband.app/Contents/home/.zshrc', 'file', 'x']], /absolute/)
  refuse([['Sideband.app/Contents/up', 'link', '../../..']], /outside the bundle/)
  refuse([['Sideband.app/Contents/sibling', 'link', '../../Other.app']], /outside the bundle/)
  // Even an internal link may not be written through.
  refuse([['Sideband.app/Contents/Frameworks/Kit.framework/Versions/Current/extra', 'file', 'x']], /through a symlink/)
  refuse([['Sideband.app/Contents/frameworks/Kit.framework/versions/current/extra', 'file', 'x']], /through a symlink/)
  refuse([['Other.app/', 'dir']], /one top-level/)
  refuse([['README', 'file', 'x']], /\.app/)
  refuse([['Sideband.app/Contents/MacOS/sideband', 'file', 'again']], /twice/)
  refuse([['Sideband.app/Contents/odd\nname', 'file', 'x']], /control/)
  assert.throws(() => checkAppArchive(listZip(zip([['Sideband.app', 'link', '/Applications/Sideband.app'], ['Sideband.app/Contents/x', 'file', 'x']]))), /must be a directory/)
  assert.throws(() => checkAppArchive(listZip(zip([['Sideband.app/Contents/', 'dir']]))), /top-level/)
  assert.throws(() => listZip(Buffer.from('not a zip')), /ZIP/)
})

test('ZIP sidecars and link chains cannot reach outside the bundle', () => {
  const refuse = (extra, pattern) => assert.throws(() => checkAppArchive(listZip(zip([...appBundle, ...extra]))), pattern)
  // ditto would apply AppleDouble data through Versions/Current; sidecars are never produced.
  refuse([['__MACOSX/Sideband.app/Contents/Frameworks/Kit.framework/Versions/Current/._x', 'file', 'appledouble']], /__MACOSX/)
  refuse([['__MACOSX/', 'dir']], /__MACOSX/)
  // Each link is inside on its own; the filesystem follows `up` before the trailing `..`.
  refuse([['Sideband.app/Contents/d1/', 'dir'], ['Sideband.app/Contents/d1/d2/', 'dir'],
    ['Sideband.app/Contents/d1/d2/up', 'link', '../..'], ['Sideband.app/Contents/x', 'link', 'd1/d2/up/../..']], /outside the bundle/)
  refuse([['Sideband.app/y', 'link', 'Contents/../..'], ['Sideband.app/Contents/z', 'link', '../y/Contents']], /outside the bundle/)
  refuse([['Sideband.app/Contents/a', 'link', 'b'], ['Sideband.app/Contents/b', 'link', 'a']], /loop/)
})

test('local headers must agree with the central directory, and link data is bounded', () => {
  const link = (options) => [...appBundle, ['Sideband.app/Contents/l', 'link', 'MacOS/sideband', options]]
  assert.equal(listZip(zip(link())).at(-1).target, 'MacOS/sideband')
  const refuse = (entries, pattern) => assert.throws(() => listZip(zip(entries)), pattern)
  refuse(link({ patch: (local) => local.writeUInt16LE(8, 8) }), /method/)
  refuse(link({ patch: (local) => local.writeUInt32LE(99, 22) }), /size/)
  refuse(link({ patch: (local) => local.writeUInt32LE(99, 18) }), /size/)
  refuse(link({ patch: (local) => local.writeUInt32LE(1, 14) }), /CRC/)
  refuse(link({ patch: (local) => local.writeUInt16LE(0x802, 6) }), /flags/)
  refuse(link({ patch: (local, central) => { local.writeUInt32LE(1, 14); central.writeUInt32LE(1, 16) } }), /CRC/)
  refuse(link({ descriptor: true }), /data descriptor/)
  refuse(link({ patch: (local, central) => { local.writeUInt16LE(12, 8); central.writeUInt16LE(12, 10) } }), /compression/)
  // A file's data descriptor must agree too.
  refuse([...appBundle, ['Sideband.app/Contents/f', 'file', 'abc', { deflate: true, descriptor: true, patch: (l, c, d) => d.writeUInt32LE(7, 12) }]], /descriptor/)
  refuse([['Sideband.app/', 'dir'], ['Sideband.app/l', 'link', 'x'.repeat(5000), { deflate: true }]], /link target/i)
  refuse([['Sideband.app/', 'dir'], ['Sideband.app/l', 'link', 'x'.repeat(100000), { deflate: true, size: 10 }]], /link target/i)
})

test('a crafted ZIP is refused before anything is extracted', async () => {
  const directory = await macosArtifacts([['Sideband.app/Contents/root', 'link', '/'], ['Sideband.app/Contents/root/tmp/owned', 'file', 'x']])
  const scratch = await mkdtemp(path.join(tmpdir(), 'sideband-scratch-test-'))
  const calls = []
  await assert.rejects(signMacos(directory, { APPLE_SIGNING_IDENTITY: 'X', APPLE_KEYCHAIN: '/tmp/k' }, { run: fakeTools(calls), entitlements: '/e', scratch }), /absolute/)
  assert.deepEqual(calls, [])
  assert.deepEqual(await readdir(scratch), [])
})

test('the extracted app must be a real directory whose symlinks stay inside it', async () => {
  const make = async () => {
    const root = await mkdtemp(path.join(tmpdir(), 'sideband-extract-test-'))
    const kit = path.join(root, 'Sideband.app', 'Contents', 'Frameworks', 'Kit.framework')
    await mkdir(path.join(kit, 'Versions', 'A'), { recursive: true })
    await writeFile(path.join(kit, 'Versions', 'A', 'Kit'), 'lib')
    await symlink('A', path.join(kit, 'Versions', 'Current'))
    await symlink('Versions/Current/Kit', path.join(kit, 'Kit'))
    return root
  }
  const good = await make()
  await checkExtractedApp(good, 'Sideband.app')
  const outside = await make()
  await symlink(tmpdir(), path.join(outside, 'Sideband.app', 'Contents', 'tmp'))
  await assert.rejects(checkExtractedApp(outside, 'Sideband.app'), /outside the bundle/)
  const dangling = await make()
  await symlink('missing', path.join(dangling, 'Sideband.app', 'Contents', 'gone'))
  await assert.rejects(checkExtractedApp(dangling, 'Sideband.app'), /does not resolve/)
  const linked = await mkdtemp(path.join(tmpdir(), 'sideband-extract-test-'))
  await symlink(good, path.join(linked, 'Sideband.app'))
  await assert.rejects(checkExtractedApp(linked, 'Sideband.app'), /real directory/)
  const extra = await make()
  await writeFile(path.join(extra, 'stray'), 'x')
  await assert.rejects(checkExtractedApp(extra, 'Sideband.app'), /only Sideband\.app/)
})

test('a real ditto ZIP of a bundle with framework symlinks passes', { skip: process.platform !== 'darwin' }, async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'sideband-ditto-test-'))
  const kit = path.join(root, 'Sideband.app', 'Contents', 'Frameworks', 'Kit.framework')
  await mkdir(path.join(kit, 'Versions', 'A'), { recursive: true })
  await writeFile(path.join(kit, 'Versions', 'A', 'Kit'), 'lib')
  await symlink('A', path.join(kit, 'Versions', 'Current'))
  await symlink('Versions/Current/Kit', path.join(kit, 'Kit'))
  // With --sequesterRsrc this extended attribute would produce __MACOSX sidecars.
  execFileSync('xattr', ['-w', 'com.example.detritus', 'x', path.join(kit, 'Versions', 'A', 'Kit')])
  const archive = path.join(root, 'app.zip')
  execFileSync('/usr/bin/ditto', ['-c', '-k', '--norsrc', '--noextattr', '--noqtn', '--noacl', '--keepParent', path.join(root, 'Sideband.app'), archive])
  const entries = listZip(await readFile(archive))
  assert(!entries.some((entry) => entry.name.startsWith('__MACOSX')))
  assert.equal(checkAppArchive(entries), 'Sideband.app')
  const listed = execFileSync('/usr/bin/zipinfo', ['-1', archive], { encoding: 'utf8' }).trim().split('\n').sort()
  assert.deepEqual(entries.map((entry) => entry.name).sort(), listed)
  assert.equal(entries.find((entry) => entry.name.endsWith('Kit.framework/Kit')).target, 'Versions/Current/Kit')
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

async function macosArtifacts(extra = []) {
  const directory = await mkdtemp(path.join(tmpdir(), 'sideband-macos-sign-test-'))
  const bundle = [...appBundle, ...extra]
  for (const [file, body] of [['Sideband.dmg', 'adhoc-dmg'], ['Sideband-test-0.1.0.app.zip', zip(bundle)], ['Sideband-test-0.1.0.app.tar.gz', 'adhoc-tar'], ['INSTALL.md', 'install']]) {
    await writeFile(path.join(directory, file), body)
  }
  await writeManifest(directory, { version: '0.1.0', target: 'aarch64-apple-darwin' }, ['Sideband.dmg', 'Sideband-test-0.1.0.app.zip', 'Sideband-test-0.1.0.app.tar.gz'], ['Sideband-test-0.1.0.app.tar.gz'])
  return directory
}

function fakeTools(calls, { status = 'Accepted' } = {}) {
  return async (command, args) => {
    calls.push([command, ...args])
    const tool = path.basename(command)
    if (tool === 'zipinfo') return listZip(await readFile(args.at(-1))).map((entry) => entry.name).join('\n') + '\n'
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
  const corrupted = await macosArtifacts()
  await writeFile(path.join(corrupted, 'Sideband-test-0.1.0.app.zip'), 'changed in transit')
  // Catches corruption between jobs; the manifest comes from the same build job.
  await assert.rejects(signMacos(corrupted, env, { run: fakeTools([]), entitlements: '/repo/Entitlements.plist' }), /does not match build-info/)
  await assert.rejects(signMacos(await macosArtifacts(), { ...env, APPLE_KEYCHAIN: '' }, { run: fakeTools([]), entitlements: '/repo/Entitlements.plist' }), /APPLE_KEYCHAIN/)
})
