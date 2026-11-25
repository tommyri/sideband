import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { deflateRawSync, gzipSync } from 'node:zlib'
import { access, mkdir, mkdtemp, readdir, readFile, rm, stat, symlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import {
  assertNoBundleHooks, assertNoSigningSecrets, buildEnvironment, bundleWindows, checkAppArchive, checkExtractedApp, checkUnbundledWindows,
  checkWindowsExecutable, compareTree, createUpdaterArchive, installTauriCli, listZip, mainBinaryName, makeIdentity,
  notarizationKey, parseOptions, parseUpdaterPublicKey, prepareWindows, readTar, signMacos, signUnpackedMacos,
  tauriCliPackages, artifactDirectories, unpackMacos, updateConfiguration, updaterPayloads, verifyTauriCli, windowsSigningConfig,
  windowsTarget, writeManifest, writeUnbundledManifest,
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
    if (tool === 'ditto' && args[0] === '-x') {
      // A faithful extractor for the listing, so the tree checks see real files.
      for (const entry of listZip(await readFile(args.at(-2)))) {
        const target = path.join(args.at(-1), entry.name)
        await mkdir(entry.type === 'dir' ? target : path.dirname(target), { recursive: true })
        if (entry.type === 'file') await writeFile(target, 'x')
        if (entry.type === 'symlink') await symlink(entry.target, target)
      }
    }
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

test('macOS build output is found whether download-artifact nested it or not', async () => {
  const single = await macosArtifacts()
  assert.deepEqual(await artifactDirectories(single), [single])
  const several = await mkdtemp(path.join(tmpdir(), 'sideband-several-test-'))
  await mkdir(path.join(several, 'sideband-test-aarch64-apple-darwin-abc'))
  await mkdir(path.join(several, 'sideband-test-x86_64-apple-darwin-abc'))
  await writeFile(path.join(several, 'stray.txt'), 'x')
  assert.deepEqual((await artifactDirectories(several)).map((dir) => path.basename(dir)).sort(),
    ['sideband-test-aarch64-apple-darwin-abc', 'sideband-test-x86_64-apple-darwin-abc'])
})

test('unpacking needs no secrets and runs no signing tool; the tree must equal the listing', async () => {
  const directory = await macosArtifacts()
  const scratch = await mkdtemp(path.join(tmpdir(), 'sideband-scratch-test-'))
  const calls = []
  const unpacked = await unpackMacos(directory, { run: fakeTools(calls), scratch })
  assert.equal(unpacked.appName, 'Sideband.app')
  assert.deepEqual(calls.map(([command]) => path.basename(command)), ['zipinfo', 'ditto'])
  assert(unpacked.unpacked.startsWith(path.join(scratch, 'sideband-unpack-')))
  const entries = listZip(await readFile(path.join(directory, 'Sideband-test-0.1.0.app.zip')))
  await compareTree(unpacked.unpacked, entries)
  // Anything the listing does not account for is refused before signing.
  const app = path.join(unpacked.unpacked, 'Sideband.app')
  await writeFile(path.join(app, 'Contents', 'planted'), 'x')
  await assert.rejects(compareTree(unpacked.unpacked, entries), /not in the archive listing.*planted/i)
  const env = { APPLE_SIGNING_IDENTITY: 'Developer ID Application: Example (TEAM)', APPLE_KEYCHAIN: '/tmp/k.keychain-db' }
  await assert.rejects(signUnpackedMacos(directory, unpacked, env, { run: fakeTools([]), entitlements: '/e', scratch }), /planted/)
})

test('the on-disk tree must match the listing in names, types and link targets', async () => {
  const entries = listZip(zip(appBundle))
  const archive = path.join(await mkdtemp(path.join(tmpdir(), 'sideband-tree-zip-')), 'app.zip')
  await writeFile(archive, zip(appBundle))
  const extract = async () => {
    const root = await mkdtemp(path.join(tmpdir(), 'sideband-tree-test-'))
    await fakeTools([])('/usr/bin/ditto', ['-x', '-k', archive, root])
    return root
  }
  await compareTree(await extract(), entries)
  const missing = await extract()
  await rm(path.join(missing, 'Sideband.app', 'Contents', 'MacOS', 'sideband'))
  await assert.rejects(compareTree(missing, entries), /missing/)
  const retyped = await extract()
  await rm(path.join(retyped, 'Sideband.app', 'Contents', 'Frameworks', 'Kit.framework', 'Kit'))
  await writeFile(path.join(retyped, 'Sideband.app', 'Contents', 'Frameworks', 'Kit.framework', 'Kit'), 'x')
  await assert.rejects(compareTree(retyped, entries), /type differs/i)
  const relinked = await extract()
  const current = path.join(relinked, 'Sideband.app', 'Contents', 'Frameworks', 'Kit.framework', 'Versions', 'Current')
  await rm(current)
  await symlink('A/Resources', current)
  await assert.rejects(compareTree(relinked, entries), /target/)
})

// One job of a workflow, from its key to the next job at the same indentation.
function jobText(workflow, name) {
  const start = workflow.indexOf(`\n  ${name}:\n`)
  assert(start >= 0, name)
  const next = workflow.slice(start + 1).search(/\n  [a-z][\w-]*:\n/)
  return next === -1 ? workflow.slice(start) : workflow.slice(start, start + 1 + next)
}
const withoutComments = (text) => text.split('\n').filter((line) => !/^\s*#/.test(line)).join('\n')

test('desktop releases omit package tests only for targets in the same-run workspace test matrix', async () => {
  const [release, platforms, standalone, packaging] = await Promise.all(
    ['release-desktop', 'check-platforms', 'test-desktop', 'package-desktop'].map((name) =>
      readFile(new URL(`../.github/workflows/${name}.yml`, import.meta.url), 'utf8')),
  )
  const workspace = jobText(platforms, 'check')
  const targets = [...workspace.matchAll(/^\s+target: ([\w-]+)$/gm)].map((match) => match[1])
  assert(targets.length > 0)
  assert.match(workspace, /run: cargo test --locked --workspace/)
  assert.match(jobText(release, 'ci'), /uses: .\/\.github\/workflows\/check-platforms.yml/)
  const covered = jobText(release, 'build').match(/workspace_test_targets: '([^']+)'/)
  assert(covered, 'release packaging must declare the targets already tested')
  assert.deepEqual(JSON.parse(covered[1]).sort(), targets.sort())
  assert(!jobText(standalone, 'build').includes('workspace_test_targets'))
  assert.match(packaging, /workspace_test_targets:\n(?: {8}[^\n]+\n)*? {8}default: '\[\]'/)
  assert(jobText(packaging, 'package').includes("!contains(fromJSON(inputs.workspace_test_targets), matrix.target)"))
})

test('no step of the macOS signing job before the keychain import can see a secret', async () => {
  const workflow = await readFile(new URL('../.github/workflows/package-desktop.yml', import.meta.url), 'utf8')
  const job = jobText(workflow, 'macos-signing')
  const steps = job.split('\n      - ').slice(1)
  const at = (pattern) => steps.findIndex((step) => pattern.test(step))
  const keychain = at(/name: Import signing certificate/)
  assert(keychain > 0)
  for (const step of steps.slice(0, keychain)) assert(!step.includes('secrets.'), step.split('\n')[0])
  assert(steps[keychain].includes('secrets.APPLE_CERTIFICATE'))
  const order = [/Refuse refs other than main/, /actions\/checkout@/, /release-gate\.mjs environment/, /actions\/download-artifact@/,
    /unpack-macos/, /git status --porcelain/, /Import signing certificate/, /sign-macos/].map(at)
  assert(order.every((index, i) => index >= 0 && (i === 0 || index > order[i - 1])), JSON.stringify(order))
  // The job is the only one referencing the notarization key.
  assert.equal(workflow.split('secrets.APPLE_API_PRIVATE_KEY').length - 1, job.split('secrets.APPLE_API_PRIVATE_KEY').length - 1)
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

// --- Windows: no-bundle build, checks before the certificate, `tauri bundle` after ---

test('--no-bundle is a Windows release option only', () => {
  assert.deepEqual(parseOptions(['--no-bundle', '--updates', '--version', '0.2.0'], windowsTarget, 'win32'),
    { target: windowsTarget, bundles: [], debug: false, noBundle: true, version: '0.2.0', updates: true })
  assert.throws(() => parseOptions(['--no-bundle'], 'aarch64-apple-darwin', 'darwin'), /only for Windows/)
  assert.throws(() => parseOptions(['--no-bundle', '--bundles', 'nsis'], windowsTarget, 'win32'), /only for Windows/)
  assert.throws(() => parseOptions(['--no-bundle', '--debug'], windowsTarget, 'win32'), /only for Windows/)
})

test('the build never gets Windows signing configuration; only `tauri bundle` does', () => {
  const env = { GITHUB_REPOSITORY: 'owner/repo', TAURI_UPDATER_PUBLIC_KEY: fixturePublicKey, WINDOWS_CERTIFICATE_THUMBPRINT: 'A'.repeat(40) }
  assert.equal(updateConfiguration(env, windowsTarget).bundle.windows, undefined)
  assert.equal(buildEnvironment(env, { updates: false }, {}, '/target').WINDOWS_CERTIFICATE_THUMBPRINT, undefined)
  assert.deepEqual(windowsSigningConfig(''), {})
  assert.deepEqual(windowsSigningConfig('0123456789ABCDEF0123456789ABCDEF01234567').bundle.windows,
    { certificateThumbprint: '0123456789ABCDEF0123456789ABCDEF01234567', digestAlgorithm: 'sha256', timestampUrl: 'http://timestamp.digicert.com' })
  assert.throws(() => windowsSigningConfig('abc" /p secret'), /thumbprint/)
})

test('no Tauri configuration runs a package script or custom signer during `tauri bundle`', async () => {
  const configs = {}
  for (const name of ['tauri.conf.json', 'tauri.test.conf.json']) {
    configs[name] = JSON.parse(await readFile(new URL(`../apps/desktop/src-tauri/${name}`, import.meta.url), 'utf8'))
  }
  assertNoBundleHooks(configs)
  assert.throws(() => assertNoBundleHooks({ 'tauri.windows.conf.json': { build: { beforeBundleCommand: 'pnpm x' } } }), /beforeBundleCommand/)
  assert.throws(() => assertNoBundleHooks({ 'tauri.conf.json': { bundle: { windows: { signCommand: 'x %1' } } } }), /signCommand/)
})

test('the bundled executable is the Cargo package binary', async () => {
  const cargo = await readFile(new URL('../apps/desktop/src-tauri/Cargo.toml', import.meta.url), 'utf8')
  const config = JSON.parse(await readFile(new URL('../apps/desktop/src-tauri/tauri.conf.json', import.meta.url), 'utf8'))
  assert.equal(mainBinaryName(cargo, config), 'socialspeak-desktop.exe')
  assert.equal(mainBinaryName(cargo, { mainBinaryName: 'Sideband' }), 'Sideband.exe')
  assert.throws(() => mainBinaryName(cargo, { mainBinaryName: '../x' }), /binary name/)
})

// A minimal PE32+ image: MZ stub, PE header, optional header with 16 data directories.
function peImage({ machine = 0x8664, characteristics = 0x0022, magic = 0x20b, certificate = [0, 0], marker = true } = {}) {
  const bytes = Buffer.alloc(0x400)
  bytes.write('MZ', 0, 'latin1')
  bytes.writeUInt32LE(0x80, 0x3c)
  bytes.writeUInt32LE(0x00004550, 0x80)
  bytes.writeUInt16LE(machine, 0x84)
  bytes.writeUInt16LE(240, 0x80 + 20)
  bytes.writeUInt16LE(characteristics, 0x80 + 22)
  const optional = 0x80 + 24
  bytes.writeUInt16LE(magic, optional)
  bytes.writeUInt32LE(16, optional + 108)
  bytes.writeUInt32LE(certificate[0], optional + 112 + 32)
  bytes.writeUInt32LE(certificate[1], optional + 112 + 36)
  if (marker) bytes.write('__TAURI_BUNDLE_TYPE_VAR_UNK', 0x300, 'latin1')
  return bytes
}

test('only an unsigned x64 Tauri executable is accepted for bundling', () => {
  checkWindowsExecutable(peImage())
  assert.throws(() => checkWindowsExecutable(peImage({ certificate: [0x380, 0x40] })), /already signed/)
  assert.throws(() => checkWindowsExecutable(peImage({ marker: false })), /bundle-type marker/)
  assert.throws(() => checkWindowsExecutable(peImage({ machine: 0x14c })), /x64/)
  assert.throws(() => checkWindowsExecutable(peImage({ characteristics: 0x2022 })), /executable image/)
  assert.throws(() => checkWindowsExecutable(peImage({ magic: 0x10b })), /PE32\+/)
  assert.throws(() => checkWindowsExecutable(Buffer.concat([Buffer.from('ZM'), peImage().subarray(2)])), /MZ/)
  assert.throws(() => checkWindowsExecutable(Buffer.from('MZ')), /size/)
})

const windowsIdentity = { schemaVersion: 1, channel: 'test', version: '0.2.0', commit: 'c'.repeat(40), dirty: false,
  target: windowsTarget, origin: 'github-actions', profile: 'release' }
const windowsExpected = { version: '0.2.0', commit: 'c'.repeat(40), origin: 'github-actions' }
const binary = 'socialspeak-desktop.exe'

async function unbundledOutput({ identity = windowsIdentity, image = peImage() } = {}) {
  const directory = await mkdtemp(path.join(tmpdir(), 'sideband-unbundled-test-'))
  await writeFile(path.join(directory, binary), image)
  await writeUnbundledManifest(directory, identity, binary)
  return directory
}

test('the uploaded Windows build is exactly the executable, its manifest and checksums', async () => {
  const good = await unbundledOutput()
  const { identity, sha256 } = await checkUnbundledWindows(good, binary, windowsExpected)
  assert.deepEqual(identity, windowsIdentity)
  assert.equal(sha256, createHash('sha256').update(peImage()).digest('hex'))
  const refuse = async (change, pattern) => {
    const directory = await unbundledOutput()
    await change(directory)
    await assert.rejects(checkUnbundledWindows(directory, binary, windowsExpected), pattern)
  }
  await refuse((dir) => writeFile(path.join(dir, 'extra.dll'), 'x'), /exactly/)
  await refuse((dir) => mkdir(path.join(dir, 'nested')), /exactly/)
  await refuse((dir) => rm(path.join(dir, 'SHA256SUMS')), /exactly/)
  await refuse(async (dir) => { await rm(path.join(dir, binary)); await symlink(path.join(good, binary), path.join(dir, binary)) }, /regular file/)
  await refuse((dir) => writeFile(path.join(dir, binary), peImage({ marker: false })), /does not match build-info/)
  await refuse(async (dir) => writeFile(path.join(dir, 'SHA256SUMS'), (await readFile(path.join(dir, 'SHA256SUMS'), 'utf8')) + 'x  extra\n'), /SHA256SUMS/)
  // A manifest that lists the executable under another name, or more than one file.
  await refuse(async (dir) => {
    const info = JSON.parse(await readFile(path.join(dir, 'build-info.json'), 'utf8'))
    await writeFile(path.join(dir, 'SHA256SUMS'), '')
    await writeFile(path.join(dir, 'build-info.json'), JSON.stringify({ ...info, artifacts: [...info.artifacts, { ...info.artifacts[0], file: 'SHA256SUMS' }] }))
  }, /does not match|must list only/)
  for (const [key, value] of [['commit', 'd'.repeat(40)], ['version', '0.1.0'], ['dirty', true], ['target', 'aarch64-apple-darwin'], ['profile', 'debug'], ['origin', 'local']]) {
    const directory = await unbundledOutput({ identity: { ...windowsIdentity, [key]: value } })
    await assert.rejects(checkUnbundledWindows(directory, binary, windowsExpected), new RegExp(key))
  }
  const signed = await unbundledOutput({ image: peImage({ certificate: [0x380, 0x40] }) })
  await assert.rejects(checkUnbundledWindows(signed, binary, windowsExpected), /already signed/)
})

test('the locked Tauri CLI is read from pnpm-lock.yaml with its integrity', async () => {
  const lock = await readFile(new URL('../apps/desktop/pnpm-lock.yaml', import.meta.url), 'utf8')
  const packages = tauriCliPackages(lock)
  assert.deepEqual(packages.map((item) => item.name), ['@tauri-apps/cli', '@tauri-apps/cli-win32-x64-msvc'])
  const version = packages[0].version
  assert.match(version, /^2\.\d+\.\d+$/)
  assert.deepEqual(packages.map((item) => item.url), [
    `https://registry.npmjs.org/@tauri-apps/cli/-/cli-${version}.tgz`,
    `https://registry.npmjs.org/@tauri-apps/cli-win32-x64-msvc/-/cli-win32-x64-msvc-${version}.tgz`])
  for (const item of packages) assert(lock.includes(`integrity: ${item.integrity}`), item.name)
  // A resolution with a tarball URL (or anything besides the integrity) is not accepted.
  const integrity = `integrity: ${packages[1].integrity}`
  const redirected = lock.replace(integrity, `${integrity}, tarball: https://example.com/x.tgz`)
  assert.throws(() => tauriCliPackages(redirected), /plain registry entry/)
})

// A minimal ustar writer. entries: [name, type ('0' file, '5' dir, '2' symlink, 'x' pax), content, linkname]
function tarball(entries, { corrupt = false } = {}) {
  const blocks = []
  for (const [name, type = '0', content = '', link = ''] of entries) {
    const data = Buffer.from(content), header = Buffer.alloc(512)
    header.write(name, 0, 100, 'utf8')
    header.write('0000644\0', 100); header.write('0000000\0', 108); header.write('0000000\0', 116)
    header.write(data.length.toString(8).padStart(11, '0') + '\0', 124); header.write('00000000000\0', 136)
    header.write(type, 156); header.write(link, 157, 100); header.write('ustar\0', 257, 'latin1'); header.write('00', 263)
    header.fill(0x20, 148, 156)
    let sum = 0
    for (const byte of header) sum += byte
    header.write(sum.toString(8).padStart(6, '0') + '\0 ', 148)
    if (corrupt) header[0] ^= 1
    blocks.push(header, data, Buffer.alloc((512 - (data.length % 512)) % 512))
  }
  return gzipSync(Buffer.concat([...blocks, Buffer.alloc(1024)]))
}
const paxRecord = (key, value) => {
  const body = ` ${key}=${value}\n`
  let length = body.length + 1
  while (String(length).length + body.length !== length) length++
  return length + body
}

test('npm tarballs are read as plain files and directories under package/ only', () => {
  const entries = readTar(tarball([['package/', '5'], ['package/package.json', '0', '{}'], ['package/lib/a.js', '0', 'a'],
    ['', 'x', paxRecord('path', 'package/' + 'long/'.repeat(30) + 'b.js')], ['ignored-name', '0', 'b']]))
  assert.deepEqual(entries.map((entry) => [entry.path.join('/'), entry.type]),
    [['', 'dir'], ['package.json', 'file'], ['lib/a.js', 'file'], ['long/'.repeat(30) + 'b.js', 'file']])
  assert.equal(entries[2].data.toString(), 'a')
  const refuse = (list, pattern) => assert.throws(() => readTar(tarball(list)), pattern)
  refuse([['package/link', '2', '', '../../../evil']], /not a file or directory/)
  refuse([['package/hard', '1', '', 'package/a']], /not a file or directory/)
  refuse([['', 'x', paxRecord('linkpath', '/etc/passwd')], ['package/l', '2']], /links/)
  refuse([['package/../escape.js', '0', 'x']], /Refusing/)
  refuse([['other/escape.js', '0', 'x']], /Refusing/)
  refuse([['/package/abs.js', '0', 'x']], /Refusing/)
  refuse([['package/a\\b.js', '0', 'x']], /Refusing/)
  refuse([['package/C:x', '0', 'x']], /Refusing/)
  refuse([['package/A.js', '0', 'x'], ['package/a.js', '0', 'y']], /twice/)
  assert.throws(() => readTar(tarball([['package/a.js', '0', 'x']], { corrupt: true })), /checksum/)
})

// A lockfile with the same shape as pnpm's for a made-up CLI version.
function cliLock(cli, bindingTarball, version = '9.9.9') {
  const sri = (bytes) => 'sha512-' + createHash('sha512').update(bytes).digest('base64')
  return `importers:\n  .:\n    devDependencies:\n      '@tauri-apps/cli':\n        specifier: ^${version}\n        version: ${version}\n\npackages:\n` +
    `  '@tauri-apps/cli-win32-x64-msvc@${version}':\n    resolution:\n      {\n        integrity: ${sri(bindingTarball)},\n      }\n    os: [win32]\n\n` +
    `  '@tauri-apps/cli@${version}':\n    resolution:\n      {\n        integrity: ${sri(cli)},\n      }\n    hasBin: true\n\nsnapshots:\n` +
    `  '@tauri-apps/cli@${version}':\n    optionalDependencies:\n      '@tauri-apps/cli-darwin-arm64': ${version}\n      '@tauri-apps/cli-win32-x64-msvc': ${version}\n`
}
const cliTarball = (name, version = '9.9.9', extra = []) => tarball([['package/package.json', '0', JSON.stringify({ name, version })], ...extra])
const fakeRegistry = (files, requests = []) => async (url) => {
  requests.push(url)
  const body = files[url]
  return body ? { ok: true, status: 200, arrayBuffer: async () => body } : { ok: false, status: 404 }
}
const registryFiles = (cli, binding, version = '9.9.9') => ({
  [`https://registry.npmjs.org/@tauri-apps/cli/-/cli-${version}.tgz`]: cli,
  [`https://registry.npmjs.org/@tauri-apps/cli-win32-x64-msvc/-/cli-win32-x64-msvc-${version}.tgz`]: binding,
})

test('the Tauri CLI is installed from two integrity-checked tarballs and rechecked before it runs', async () => {
  const cli = cliTarball('@tauri-apps/cli', '9.9.9', [['package/tauri.js', '0', 'cli'], ['package/lib/', '5']])
  const binding = cliTarball('@tauri-apps/cli-win32-x64-msvc', '9.9.9', [['package/cli.win32-x64-msvc.node', '0', 'native']])
  const lock = cliLock(cli, binding)
  const destination = await mkdtemp(path.join(tmpdir(), 'sideband-cli-test-'))
  const requests = []
  const entry = await installTauriCli(lock, destination, { fetch: fakeRegistry(registryFiles(cli, binding), requests) })
  assert.equal(entry, path.join(destination, 'node_modules', '@tauri-apps', 'cli', 'tauri.js'))
  assert.equal(requests.length, 2)
  assert.equal(await readFile(path.join(destination, 'node_modules', '@tauri-apps', 'cli-win32-x64-msvc', 'cli.win32-x64-msvc.node'), 'utf8'), 'native')
  assert.equal(await verifyTauriCli(lock, destination), entry)
  await writeFile(path.join(destination, 'node_modules', '@tauri-apps', 'cli', 'planted.js'), 'x')
  await assert.rejects(verifyTauriCli(lock, destination), /Not part of the Tauri CLI.*planted/)
  await rm(path.join(destination, 'node_modules', '@tauri-apps', 'cli', 'planted.js'))
  await writeFile(entry, 'changed')
  await assert.rejects(verifyTauriCli(lock, destination), /changed: node_modules\/@tauri-apps\/cli\/tauri\.js/)
  await rm(entry)
  await assert.rejects(verifyTauriCli(lock, destination), /missing/)
  // A tarball that is not the locked one is refused before anything is extracted.
  const swapped = await mkdtemp(path.join(tmpdir(), 'sideband-cli-test-'))
  await assert.rejects(installTauriCli(lock, swapped, { fetch: fakeRegistry(registryFiles(cliTarball('@tauri-apps/cli', '9.9.9', [['package/tauri.js', '0', 'evil']]), binding)) }), /integrity/)
  assert.deepEqual(await readdir(swapped), ['tarballs'])
  // A locked tarball that carries dependencies or another package is refused too.
  const withDeps = tarball([['package/package.json', '0', JSON.stringify({ name: '@tauri-apps/cli', version: '9.9.9', dependencies: { x: '1' } })]])
  await assert.rejects(installTauriCli(cliLock(withDeps, binding), await mkdtemp(path.join(tmpdir(), 'sideband-cli-test-')), { fetch: fakeRegistry(registryFiles(withDeps, binding)) }), /dependencies/)
  const wrongName = cliTarball('left-pad')
  await assert.rejects(installTauriCli(cliLock(wrongName, binding), await mkdtemp(path.join(tmpdir(), 'sideband-cli-test-')), { fetch: fakeRegistry(registryFiles(wrongName, binding)) }), /holds left-pad/)
})

async function windowsSigningFixture() {
  const cli = cliTarball('@tauri-apps/cli', '9.9.9', [['package/tauri.js', '0', 'cli']])
  const binding = cliTarball('@tauri-apps/cli-win32-x64-msvc', '9.9.9', [['package/cli.win32-x64-msvc.node', '0', 'native']])
  const lockText = cliLock(cli, binding)
  const scratch = await mkdtemp(path.join(tmpdir(), 'sideband-windows-scratch-'))
  const input = await unbundledOutput()
  const state = await prepareWindows(input, { binary, expected: windowsExpected, lockText, scratch, fetch: fakeRegistry(registryFiles(cli, binding)) })
  return { lockText, scratch, input, state }
}

// Stands in for `node tauri.js bundle`: records the call and writes the two installers.
function fakeBundler(calls) {
  return async (command, args, options) => {
    calls.push({ command, args, env: options.env })
    const release = path.join(options.env.CARGO_TARGET_DIR, 'release', 'bundle')
    await mkdir(path.join(release, 'nsis'), { recursive: true })
    await mkdir(path.join(release, 'msi'), { recursive: true })
    await writeFile(path.join(release, 'nsis', 'Sideband_0.2.0_x64-setup.exe'), 'nsis')
    await writeFile(path.join(release, 'msi', 'Sideband_0.2.0_x64_en-US.msi'), 'msi')
  }
}

const releaseEnv = { PATH: '/bin', GITHUB_REPOSITORY: 'owner/repo', TAURI_UPDATER_PUBLIC_KEY: fixturePublicKey }

test('Windows bundling runs only `tauri bundle` from the checked CLI, with the thumbprint, and lists both payloads', async () => {
  const { lockText, scratch, state } = await windowsSigningFixture()
  assert.deepEqual(await readdir(path.join(state.targetDir, 'release')), [binary])
  const calls = []
  const output = path.join(scratch, 'windows-output')
  const thumbprint = '0123456789ABCDEF0123456789ABCDEF01234567'
  const info = await bundleWindows(state, { ...releaseEnv, WINDOWS_CERTIFICATE_THUMBPRINT: thumbprint }, { output, lockText, run: fakeBundler(calls) })
  assert.equal(calls.length, 1)
  const [{ command, args, env }] = calls
  assert.equal(command, process.execPath)
  assert.equal(args[0], path.join(state.cli, 'node_modules', '@tauri-apps', 'cli', 'tauri.js'))
  assert.deepEqual(args.slice(1, 5), ['bundle', '--ci', '--bundles', 'nsis,msi'])
  assert(!args.includes('build') && !args.includes('--no-sign'))
  const configs = args.filter((_, i) => args[i - 1] === '--config')
  assert.equal(configs[0], 'src-tauri/tauri.test.conf.json')
  assert.equal(JSON.parse(configs[1]).version, '0.2.0')
  const config = JSON.parse(configs[2])
  assert.equal(config.bundle.windows.certificateThumbprint, thumbprint)
  assert.equal(config.bundle.createUpdaterArtifacts, false)
  assert.equal(config.plugins.updater.pubkey, fixturePublicKey)
  assert.equal(env.CARGO_TARGET_DIR, state.targetDir)
  assert.equal(env.WINDOWS_CERTIFICATE_THUMBPRINT, undefined)
  assert.deepEqual((await readdir(output)).sort(), ['INSTALL.md', 'SHA256SUMS', 'Sideband_0.2.0_x64-setup.exe', 'Sideband_0.2.0_x64_en-US.msi', 'build-info.json'])
  assert.deepEqual(info.updater, [{ file: 'Sideband_0.2.0_x64-setup.exe' }, { file: 'Sideband_0.2.0_x64_en-US.msi' }])
  const written = JSON.parse(await readFile(path.join(output, 'build-info.json'), 'utf8'))
  assert.deepEqual({ ...written, artifacts: undefined, updater: undefined }, { ...windowsIdentity, artifacts: undefined, updater: undefined })
  assert.equal(written.unbundled, undefined)
})

test('Windows bundling without a certificate stays unsigned; a secret, a changed executable or a changed CLI stops it', async () => {
  const unsigned = await windowsSigningFixture()
  const calls = []
  await bundleWindows(unsigned.state, releaseEnv, { output: path.join(unsigned.scratch, 'out'), lockText: unsigned.lockText, run: fakeBundler(calls) })
  assert.equal(JSON.parse(calls[0].args.at(-1)).bundle.windows, undefined)
  const refuse = async (change, env, pattern) => {
    const fixture = await windowsSigningFixture()
    await change(fixture)
    const calls = []
    await assert.rejects(bundleWindows(fixture.state, env, { output: path.join(fixture.scratch, 'out'), lockText: fixture.lockText, run: fakeBundler(calls) }), pattern)
    assert.deepEqual(calls, [])
  }
  await refuse(async () => {}, { ...releaseEnv, WINDOWS_CERTIFICATE: 'pfx' }, /WINDOWS_CERTIFICATE/)
  await refuse(async () => {}, { ...releaseEnv, WINDOWS_CERTIFICATE_THUMBPRINT: 'not-hex' }, /thumbprint/)
  await refuse(({ state }) => writeFile(path.join(state.targetDir, 'release', binary), peImage({ marker: false })), releaseEnv, /staged executable changed/)
  await refuse(({ state }) => writeFile(path.join(state.targetDir, 'release', 'WebView2Loader.dll'), 'x'), releaseEnv, /only the staged executable/)
  await refuse(({ state }) => writeFile(path.join(state.cli, 'node_modules', '@tauri-apps', 'cli', 'tauri.js'), 'changed'), releaseEnv, /Tauri CLI file changed/)
  await refuse(({ input }) => writeFile(path.join(input, 'extra'), 'x'), releaseEnv, /exactly/)
})

test('a bad Windows upload leaves nothing behind and fetches nothing', async () => {
  const scratch = await mkdtemp(path.join(tmpdir(), 'sideband-windows-scratch-'))
  const requests = []
  const input = await unbundledOutput({ identity: { ...windowsIdentity, dirty: true } })
  await assert.rejects(prepareWindows(input, { binary, expected: windowsExpected, lockText: '', scratch, fetch: fakeRegistry({}, requests) }), /dirty/)
  assert.deepEqual(requests, [])
  assert.deepEqual(await readdir(scratch), [])
})

test('the Windows certificate is only in a job that runs no install, build or dependency script', async () => {
  const workflow = await readFile(new URL('../.github/workflows/package-desktop.yml', import.meta.url), 'utf8')
  const job = jobText(workflow, 'windows-signing')
  const steps = job.split('\n      - ').slice(1)
  const at = (pattern) => steps.findIndex((step) => pattern.test(step))
  const certificate = at(/name: Import Windows signing certificate/)
  assert(certificate > 0)
  for (const step of steps.slice(0, certificate)) assert(!step.includes('secrets.'), step.split('\n')[0])
  assert(steps[certificate].includes('secrets.WINDOWS_CERTIFICATE'))
  const order = [/Refuse refs other than main/, /actions\/checkout@/, /release-gate\.mjs environment/, /actions\/download-artifact@/,
    /prepare-windows/, /git status --porcelain/, /Import Windows signing certificate/, /bundle-windows/,
    /Remove the signing certificate/, /signtool/, /actions\/upload-artifact@/].map(at)
  assert(order.every((index, i) => index >= 0 && (i === 0 || index > order[i - 1])), JSON.stringify(order))
  // Nothing in the job installs dependencies, builds, restores a cache or runs a package script.
  const code = withoutComments(job)
  for (const pattern of [/\bpnpm\b/, /\bnpm\b/, /\byarn\b/, /\bnpx\b/, /\bcargo\s+(build|test|run|install|check|clippy)\b/, /tauri\.js\b.*\bbuild\b/,
    /\btauri\s+build\b/, /rust-cache/, /^\s+cache:/m, /cache-dependency-path/, /build-test-desktop\.mjs\s+--target/]) {
    assert(!pattern.test(code), String(pattern))
  }
  assert.match(job, /environment: \$\{\{ inputs\.environment \}\}/)
  assert.equal(workflow.split('secrets.WINDOWS_CERTIFICATE').length - 1, job.split('secrets.WINDOWS_CERTIFICATE').length - 1)
  // The build job holds no environment or secret, and skips bundling exactly when this job bundles.
  const build = jobText(workflow, 'package')
  assert(!build.includes('secrets.') && !/^\s+environment:/m.test(build))
  assert.match(build, /--no-bundle/)
  assert.match(job, /\n    if: inputs\.environment != '' && inputs\.updates && contains\(inputs\.matrix, 'x86_64-pc-windows-msvc'\)\n/)
  assert(build.includes("UNBUNDLED: ${{ matrix.target == 'x86_64-pc-windows-msvc' && inputs.environment != '' && inputs.updates }}"))
  assert.match(job, /name: sideband-unbundled-x86_64-pc-windows-msvc-\$\{\{ github\.sha \}\}/)
  assert.match(job, /name: sideband-test-x86_64-pc-windows-msvc-\$\{\{ github\.sha \}\}/)
})
