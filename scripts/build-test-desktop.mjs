import { execFileSync, spawnSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { inflateRawSync } from 'node:zlib'
import { createReadStream } from 'node:fs'
import { appendFile, cp, lstat, mkdir, mkdtemp, readFile, readdir, realpath, rm, stat, symlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const desktop = path.join(root, 'apps', 'desktop')
const platforms = {
  'aarch64-apple-darwin': { os: 'darwin', bundles: ['app', 'dmg'] },
  'x86_64-apple-darwin': { os: 'darwin', bundles: ['app', 'dmg'] },
  'x86_64-pc-windows-msvc': { os: 'win32', bundles: ['nsis', 'msi'] },
  'x86_64-unknown-linux-gnu': { os: 'linux', bundles: ['deb', 'appimage'] },
}

export function parseOptions(args, host, os) {
  let target = host
  let bundles
  let debug = false
  let version
  let updates = false
  for (let i = 0; i < args.length; i++) {
    switch (args[i]) {
      case '--target': target = args[++i]; break
      case '--bundles':
        if (!args[i + 1]) throw new Error('--bundles requires a format list')
        bundles = args[++i].split(',')
        break
      case '--version': version = validateVersion(args[++i]); break
      case '--updates': updates = true; break
      case '--debug': debug = true; break
      default: throw new Error('Usage: node scripts/build-test-desktop.mjs [--target TRIPLE] [--bundles app,dmg|nsis,msi|deb,appimage] [--debug] [--version X.Y.Z] [--updates]')
    }
  }
  const platform = platforms[target]
  if (!platform || platform.os !== os) throw new Error('Build on the target operating system; unsupported target')
  bundles ??= platform.bundles
  if (!bundles.length || bundles.some((bundle) => !platform.bundles.includes(bundle))) {
    throw new Error('Unsupported bundle format for target')
  }
  return { target, bundles: [...new Set(bundles)], debug, ...(version ? { version } : {}), ...(updates ? { updates } : {}) }
}

export function validateVersion(version) {
  if (typeof version !== 'string' || !/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(version) ||
      version.split('.').some((part, index) => Number(part) > (index < 2 ? 255 : 65535))) {
    throw new Error('Version must be X.Y.Z; major/minor must be 0–255 and patch 0–65535 for MSI')
  }
  return version
}

export function makeIdentity({ version, commit, status, target, debug, ci }) {
  if (!/^[a-f0-9]{40}$/.test(commit)) throw new Error('A full git commit is required')
  return {
    schemaVersion: 1, channel: 'test', version, commit,
    dirty: status.length > 0, target,
    origin: ci ? 'github-actions' : 'local',
    profile: debug ? 'debug' : 'release',
  }
}

// TAURI_UPDATER_PUBLIC_KEY is base64 of the minisign public key file `tauri signer generate` writes.
export function parseUpdaterPublicKey(text) {
  const lines = Buffer.from(String(text ?? '').trim(), 'base64').toString('utf8').trim().split('\n')
  const key = lines.length === 2 && lines[0].startsWith('untrusted comment:') ? Buffer.from(lines[1], 'base64') : Buffer.alloc(0)
  if (key.length !== 42 || key.subarray(0, 2).toString() !== 'Ed') {
    throw new Error('TAURI_UPDATER_PUBLIC_KEY is not a Tauri updater public key (the complete .pub file contents)')
  }
  return { keyId: key.subarray(2, 10), key: key.subarray(10) }
}

// Credentials that must never be visible to `tauri build`, its build scripts or proc
// macros. The updater key is used only by the release workflow's signing step; the
// Apple certificate and notarization key only by the separate macOS signing job.
// WINDOWS_CERTIFICATE* are imported by an earlier step of the Windows build job, which
// is a recorded limitation (docs/release.md), but never passed to the build itself.
export const signingSecrets = [
  'TAURI_SIGNING_PRIVATE_KEY', 'TAURI_SIGNING_PRIVATE_KEY_PASSWORD', 'TAURI_SIGNING_PRIVATE_KEY_PATH',
  'TAURI_PRIVATE_KEY', 'TAURI_PRIVATE_KEY_PASSWORD', 'TAURI_PRIVATE_KEY_PATH',
  'APPLE_ID', 'APPLE_PASSWORD', 'APPLE_TEAM_ID', 'APPLE_API_ISSUER', 'APPLE_API_KEY', 'APPLE_API_KEY_PATH',
  'APPLE_API_KEY_ID', 'APPLE_API_PRIVATE_KEY', 'APPLE_CERTIFICATE', 'APPLE_CERTIFICATE_PASSWORD',
  'WINDOWS_CERTIFICATE', 'WINDOWS_CERTIFICATE_PASSWORD',
]

export function assertNoSigningSecrets(env) {
  const present = signingSecrets.filter((key) => env[key])
  if (present.length) {
    throw new Error(`Refusing to build with ${present.join(', ')} in the environment; signing is a separate step (docs/release.md)`)
  }
}

export function buildEnvironment(env, options, identity, targetDir, updaterConfig) {
  const result = { ...env, CI: 'true', CARGO_TARGET_DIR: targetDir, SS_BUILD_IDENTITY: JSON.stringify(identity) }
  // The build is always ad-hoc signed on macOS; signMacos adds the Developer ID later.
  for (const key of [...signingSecrets, 'APPLE_SIGNING_IDENTITY', 'CARGO_BUILD_TARGET', 'SS_UPDATE_PUBLIC_KEY', 'SS_UPDATE_ENDPOINT']) delete result[key]
  if (options.updates) {
    result.SS_UPDATE_PUBLIC_KEY = env.TAURI_UPDATER_PUBLIC_KEY.trim()
    result.SS_UPDATE_ENDPOINT = updaterConfig.plugins.updater.endpoints[0]
  }
  return result
}

export function updateConfiguration(env, target) {
  if (!env.TAURI_UPDATER_PUBLIC_KEY) throw new Error('Signed updates require TAURI_UPDATER_PUBLIC_KEY')
  parseUpdaterPublicKey(env.TAURI_UPDATER_PUBLIC_KEY)
  const repo = env.GITHUB_REPOSITORY
  if (!/^[A-Za-z0-9][\w.-]*\/[A-Za-z0-9][\w.-]*$/.test(repo ?? '')) throw new Error('Invalid update repository')
  if (!platforms[target]) throw new Error('Invalid update target')
  const windows = env.WINDOWS_CERTIFICATE_THUMBPRINT ? { windows: {
    certificateThumbprint: env.WINDOWS_CERTIFICATE_THUMBPRINT, digestAlgorithm: 'sha256',
    timestampUrl: 'http://timestamp.digicert.com',
  } } : {}
  return {
    // Tauri would sign updater artifacts inside `tauri build`, which needs the private key
    // there. The installers are the payloads on Windows and Linux; the macOS archive is
    // made by createUpdaterArchive. The release workflow signs them in a separate job.
    bundle: { createUpdaterArtifacts: false, macOS: { signingIdentity: '-' }, ...windows },
    plugins: { updater: { requireSignedVersion: true, pubkey: env.TAURI_UPDATER_PUBLIC_KEY.trim(),
      endpoints: [`https://github.com/${repo}/releases/download/desktop-updates/${target}.json`],
      windows: { installMode: 'passive' } } },
  }
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, { cwd: desktop, stdio: 'inherit', ...options })
  if (result.error) throw result.error
  if (result.status !== 0) throw new Error(command + ' failed with status ' + result.status)
}

async function filesBelow(directory) {
  const result = []
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const full = path.join(directory, entry.name)
    if (entry.isDirectory()) result.push(...await filesBelow(full))
    else if (entry.isFile()) result.push(full)
  }
  return result
}

async function checksum(file) {
  const hash = createHash('sha256')
  for await (const chunk of createReadStream(file)) hash.update(chunk)
  return hash.digest('hex')
}

const updaterSuffixes = { darwin: ['.app.tar.gz'], win32: ['.exe', '.msi'], linux: ['.AppImage'] }

export function updaterPayloads(collected, os) {
  return updaterSuffixes[os].map((suffix) => {
    const matches = collected.filter((file) => file.endsWith(suffix))
    if (matches.length !== 1) throw new Error(`Expected exactly one ${suffix} updater payload`)
    return matches[0]
  })
}

// Same layout as Tauri's own updater bundle: the .app directory is the only root entry,
// which the updater strips when extracting. No AppleDouble or extended-attribute entries.
const archiveArgs = (app, destination) => ['--no-mac-metadata', '--no-xattrs', '--no-acls', '-czf', destination,
  '-C', path.dirname(app), path.basename(app)]

export function createUpdaterArchive(app, destination) {
  execFileSync('/usr/bin/tar', archiveArgs(app, destination), { env: { ...process.env, COPYFILE_DISABLE: '1' }, stdio: 'inherit' })
}

// Updater payloads are listed unsigned; the release workflow adds `<file>.sig`.
export async function writeManifest(output, identity, collected, updater) {
  const artifacts = await Promise.all([...collected].sort().map(async (file) => ({
    file, bytes: (await stat(path.join(output, file))).size,
    sha256: await checksum(path.join(output, file)),
  })))
  const info = { ...identity, artifacts, ...(updater ? { updater: updater.map((file) => ({ file })) } : {}) }
  await writeFile(path.join(output, 'build-info.json'), JSON.stringify(info, null, 2) + '\n')
  const checksummed = [...collected, 'build-info.json', 'INSTALL.md'].sort()
  const lines = await Promise.all(checksummed.map(async (file) => await checksum(path.join(output, file)) + '  ' + file))
  await writeFile(path.join(output, 'SHA256SUMS'), lines.join('\n') + '\n')
  return info
}

const dittoPlain = ['--norsrc', '--noextattr', '--noqtn', '--noacl']

const capture = async (command, args, options = {}) =>
  execFileSync(command, args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'], ...options })
const tool = (name) => `/usr/bin/${name}`

// App Store Connect API key. notarytool reads it from a file, so nothing secret goes
// on a command line. The Apple ID path would put the app password in argv.
export function notarizationKey(env) {
  const names = ['APPLE_API_KEY_ID', 'APPLE_API_ISSUER', 'APPLE_API_PRIVATE_KEY']
  const present = names.filter((name) => env[name])
  if (!present.length) {
    if (env.APPLE_ID || env.APPLE_PASSWORD) throw new Error('Notarization uses an App Store Connect API key (APPLE_API_*), not APPLE_ID/APPLE_PASSWORD')
    return null
  }
  if (present.length !== names.length) throw new Error(`Notarization needs all of ${names.join(', ')}`)
  if (!/^[A-Z0-9]{8,12}$/.test(env.APPLE_API_KEY_ID)) throw new Error('APPLE_API_KEY_ID is not an App Store Connect key ID')
  if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(env.APPLE_API_ISSUER)) throw new Error('APPLE_API_ISSUER is not an issuer ID')
  if (!env.APPLE_API_PRIVATE_KEY.includes('-----BEGIN PRIVATE KEY-----')) throw new Error('APPLE_API_PRIVATE_KEY must be the contents of the AuthKey .p8 file')
  return { id: env.APPLE_API_KEY_ID, issuer: env.APPLE_API_ISSUER, key: env.APPLE_API_PRIVATE_KEY }
}

async function notarize(file, credentials, work, run) {
  const keyFile = path.join(work, `AuthKey_${credentials.id}.p8`)
  await writeFile(keyFile, credentials.key, { mode: 0o600, flag: 'wx' })
  try {
    const result = JSON.parse(await run(tool('xcrun'), ['notarytool', 'submit', file, '--key', keyFile,
      '--key-id', credentials.id, '--issuer', credentials.issuer, '--wait', '--output-format', 'json']))
    if (result.status !== 'Accepted') throw new Error(`Notarization ${result.id ?? ''} finished as ${result.status}; see xcrun notarytool log`)
  } finally {
    await rm(keyFile, { force: true })
  }
}

// build-info.json and its checksums come from the build job itself, so this check
// catches corruption between jobs, not tampering: the signing job signs whatever the
// build produced. The trust boundary is the build's inputs, a reviewed commit and a
// locked dependency graph. Every file is also required to be a regular file.
async function readBuildManifest(directory) {
  const info = JSON.parse(await readFile(path.join(directory, 'build-info.json'), 'utf8'))
  for (const artifact of info.artifacts) {
    if (path.basename(artifact.file) !== artifact.file) throw new Error('Invalid artifact path')
    const file = path.join(directory, artifact.file)
    if (!(await lstat(file)).isFile()) throw new Error(`Artifact is not a regular file: ${artifact.file}`)
    if ((await stat(file)).size !== artifact.bytes || await checksum(file) !== artifact.sha256) {
      throw new Error(`Artifact does not match build-info.json: ${artifact.file}`)
    }
  }
  return info
}

const crcTable = Array.from({ length: 256 }, (_, n) => {
  let c = n
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1
  return c >>> 0
})
function crc32(bytes) {
  let c = 0xffffffff
  for (const byte of bytes) c = crcTable[(c ^ byte) & 0xff] ^ (c >>> 8)
  return (c ^ 0xffffffff) >>> 0
}

const maxLinkTarget = 4096

// Reads a ZIP's central directory without extracting anything. Every entry's local
// header (and data descriptor, if any) must agree with the central directory, so an
// extractor reading either sees the same archive. ZIP64, multi-disk, encrypted,
// overlapping and non-UTF-8 entries are refused, as are methods other than store and
// deflate. Link targets are read with a size bound and their CRC checked.
export function listZip(bytes) {
  const minimum = Math.max(0, bytes.length - 0xffff - 22)
  let end = -1
  for (let i = bytes.length - 22; i >= minimum; i--) if (bytes.readUInt32LE(i) === 0x06054b50) { end = i; break }
  if (end === -1) throw new Error('Not a ZIP archive')
  const count = bytes.readUInt16LE(end + 10), size = bytes.readUInt32LE(end + 12), start = bytes.readUInt32LE(end + 16)
  if (bytes.readUInt16LE(end + 4) || bytes.readUInt16LE(end + 6) || count === 0xffff || start === 0xffffffff || start + size > end) {
    throw new Error('Unsupported ZIP layout (multi-disk or ZIP64)')
  }
  const entries = [], ranges = []
  let offset = start
  for (let n = 0; n < count; n++) {
    if (offset + 46 > end || bytes.readUInt32LE(offset) !== 0x02014b50) throw new Error('Corrupt ZIP central directory')
    const madeBy = bytes.readUInt16LE(offset + 4) >> 8, flags = bytes.readUInt16LE(offset + 8), method = bytes.readUInt16LE(offset + 10)
    const crc = bytes.readUInt32LE(offset + 16), compressed = bytes.readUInt32LE(offset + 20), uncompressed = bytes.readUInt32LE(offset + 24)
    const nameLength = bytes.readUInt16LE(offset + 28), extraLength = bytes.readUInt16LE(offset + 30)
    const commentLength = bytes.readUInt16LE(offset + 32), external = bytes.readUInt32LE(offset + 38), local = bytes.readUInt32LE(offset + 42)
    const raw = bytes.subarray(offset + 46, offset + 46 + nameLength)
    const name = raw.toString('utf8'), label = JSON.stringify(name)
    if (flags & 0x1) throw new Error('Encrypted ZIP entries are not allowed')
    if (!(flags & 0x800) && raw.some((byte) => byte > 0x7f)) throw new Error('ZIP entry names must be UTF-8')
    if (method !== 0 && method !== 8) throw new Error(`Unsupported compression method ${method} for ${label}`)
    if (compressed === 0xffffffff || uncompressed === 0xffffffff || local === 0xffffffff || local + 30 > start) {
      throw new Error('Unsupported ZIP layout (ZIP64)')
    }
    if (method === 0 && compressed !== uncompressed) throw new Error(`Stored entry ${label} has two different sizes`)
    if (bytes.readUInt32LE(local) !== 0x04034b50) throw new Error(`Missing local header for ${label}`)
    const localLength = bytes.readUInt16LE(local + 26), localName = local + 30
    if (!bytes.subarray(localName, localName + localLength).equals(raw)) throw new Error(`ZIP local header name differs for ${label}`)
    if (bytes.readUInt16LE(local + 6) !== flags) throw new Error(`ZIP local header flags differ for ${label}`)
    if (bytes.readUInt16LE(local + 8) !== method) throw new Error(`ZIP local header compression method differs for ${label}`)
    const dataStart = localName + localLength + bytes.readUInt16LE(local + 28)
    let dataEnd = dataStart + compressed
    const unixMode = madeBy === 3 ? external >>> 16 : 0
    const kind = unixMode & 0o170000
    const type = kind === 0o120000 ? 'symlink'
      : kind === 0o040000 || (!kind && name.endsWith('/')) ? 'dir'
        : kind === 0o100000 || !kind ? 'file' : 'other'
    const sized = [['CRC', 14, crc], ['compressed size', 18, compressed], ['uncompressed size', 22, uncompressed]]
    if (flags & 0x8) {
      // ditto writes files this way: zero sizes locally, the real ones after the data.
      if (type === 'symlink') throw new Error(`Link ${label} uses a data descriptor`)
      for (const [field, at, value] of sized) {
        const seen = bytes.readUInt32LE(local + at)
        if (seen !== 0 && seen !== value) throw new Error(`ZIP local header ${field} differs for ${label}`)
      }
      if (dataEnd + 12 > start) throw new Error(`Truncated data descriptor for ${label}`)
      const at = bytes.readUInt32LE(dataEnd) === 0x08074b50 ? dataEnd + 4 : dataEnd
      if (bytes.readUInt32LE(at) !== crc || bytes.readUInt32LE(at + 4) !== compressed || bytes.readUInt32LE(at + 8) !== uncompressed) {
        throw new Error(`ZIP data descriptor disagrees with the directory for ${label}`)
      }
      dataEnd = at + 12
    } else {
      for (const [field, at, value] of sized) {
        if (bytes.readUInt32LE(local + at) !== value) throw new Error(`ZIP local header ${field} differs for ${label}`)
      }
    }
    if (dataEnd > start) throw new Error(`ZIP entry ${label} runs into the central directory`)
    ranges.push([local, dataEnd, label])
    const entry = { name, type, mode: unixMode & 0o7777 }
    if (type === 'symlink') {
      if (uncompressed > maxLinkTarget) throw new Error(`Link target for ${label} is too long`)
      const data = bytes.subarray(dataStart, dataEnd)
      let target
      try { target = method === 8 ? inflateRawSync(data, { maxOutputLength: maxLinkTarget }) : data } catch {
        throw new Error(`Link target for ${label} does not inflate within ${maxLinkTarget} bytes`)
      }
      if (target.length !== uncompressed) throw new Error(`Link target for ${label} is not its declared size`)
      if (crc32(target) !== crc) throw new Error(`Link target for ${label} fails its CRC check`)
      entry.target = target.toString('utf8')
    }
    entries.push(entry)
    offset += 46 + nameLength + extraLength + commentLength
  }
  ranges.sort((left, right) => left[0] - right[0])
  for (let i = 1; i < ranges.length; i++) if (ranges[i][0] < ranges[i - 1][1]) throw new Error(`ZIP entries ${ranges[i - 1][2]} and ${ranges[i][2]} overlap`)
  return entries
}

const archiveKey = (parts) => parts.join('/').normalize('NFD').toLowerCase()
const maxLinkHops = 40

// Follows a link through the links already in the archive, component by component as
// the filesystem would, and returns the path it reaches, or null if that leaves the
// bundle (parts[0]).
function resolveLink(parts, links) {
  const current = parts.slice(0, -1), queue = links.get(archiveKey(parts)).split('/')
  let hops = 0
  while (queue.length) {
    const component = queue.shift()
    if (component === '' || component === '.') continue
    if (component === '..') {
      if (current.length <= 1) return null
      current.pop()
      continue
    }
    current.push(component)
    const target = links.get(archiveKey(current))
    if (target !== undefined) {
      if (++hops > maxLinkHops) throw new Error(`Link loop at ${JSON.stringify(parts.join('/'))}`)
      current.pop()
      queue.unshift(...target.split('/'))
    }
  }
  return current
}

// Refuses anything an extractor could write outside one top-level <Name>.app: absolute
// paths, `..`, links that are absolute or, followed through other links, leave the
// bundle, and entries written through any link (keys are case- and
// normalization-folded, as on APFS). __MACOSX sidecars are refused: the build zips with
// --norsrc --noextattr, and ditto would apply them through links. Returns the app's name.
export function checkAppArchive(entries) {
  const refuse = (entry, reason) => { throw new Error(`Refusing app ZIP entry ${JSON.stringify(entry.name)}: ${reason}`) }
  let app
  const seen = new Set(), links = new Map(), parsed = []
  for (const entry of entries) {
    if (!entry.name || /[\u0000-\u001f\u007f\\]/.test(entry.name)) refuse(entry, 'control character or backslash in the name')
    if (entry.name.startsWith('/')) refuse(entry, 'absolute path')
    const parts = entry.name.replace(/\/$/, '').split('/')
    if (parts.some((part) => part === '' || part === '.' || part === '..')) refuse(entry, 'empty, . or .. path component')
    if (parts[0].normalize('NFD').toLowerCase() === '__macosx') refuse(entry, '__MACOSX sidecars are not accepted')
    if (entry.type === 'other') refuse(entry, 'not a file, directory or symlink')
    if (entry.mode & 0o7000) refuse(entry, 'setuid, setgid or sticky bit')
    if (!/^[^/]+\.app$/.test(parts[0])) refuse(entry, 'everything must be inside one <Name>.app/')
    if (app && parts[0] !== app) refuse(entry, 'more than one top-level .app')
    app ??= parts[0]
    if (parts.length === 1 && entry.type !== 'dir') refuse(entry, 'the top-level .app must be a directory')
    const key = archiveKey(parts)
    if (seen.has(key)) refuse(entry, 'the same path appears twice')
    seen.add(key)
    if (entry.type === 'symlink') {
      const target = entry.target ?? ''
      if (!target || target.includes('\u0000')) refuse(entry, 'empty symlink target')
      if (target.startsWith('/')) refuse(entry, `absolute symlink target ${JSON.stringify(target)}`)
      links.set(key, target)
    }
    parsed.push({ entry, parts })
  }
  for (const { entry, parts } of parsed) {
    for (let i = 1; i < parts.length; i++) if (links.has(archiveKey(parts.slice(0, i)))) refuse(entry, 'written through a symlink')
    if (entry.type === 'symlink' && !resolveLink(parts, links)) refuse(entry, `symlink target ${JSON.stringify(entry.target)} is outside the bundle`)
  }
  if (!app || !parsed.some(({ entry, parts }) => parts.length === 1 && entry.type === 'dir')) {
    throw new Error('The app ZIP needs a top-level <Name>.app/ directory entry')
  }
  return app
}

// After extraction: the extractor's view can differ from the listing, so check the
// tree itself. The app must be a real directory and every link must resolve inside it.
export async function checkExtractedApp(directory, appName) {
  const top = await readdir(directory)
  if (top.length !== 1 || top[0] !== appName) throw new Error(`Extraction must produce only ${appName}`)
  const app = path.join(directory, appName)
  if (!(await lstat(app)).isDirectory()) throw new Error(`${appName} is not a real directory`)
  const root = await realpath(app)
  const walk = async (dir) => {
    for (const name of await readdir(dir)) {
      const file = path.join(dir, name), info = await lstat(file)
      if (info.mode & 0o7000) throw new Error(`setuid, setgid or sticky bit on ${file}`)
      if (info.isSymbolicLink()) {
        let resolved
        try { resolved = await realpath(file) } catch { throw new Error(`Symlink ${file} does not resolve`) }
        if (resolved !== root && !resolved.startsWith(root + path.sep)) throw new Error(`Symlink ${file} points outside the bundle`)
      } else if (info.isDirectory()) await walk(file)
      else if (!info.isFile()) throw new Error(`Unexpected file type at ${file}`)
    }
  }
  await walk(app)
  return app
}

// Runs in its own job on a fresh macOS runner, after the ad-hoc build: re-signs the
// app with the Developer ID, rebuilds the ZIP, updater archive and DMG from it, signs
// the DMG, notarizes and staples it, and rewrites build-info.json and SHA256SUMS. Only
// system tools by absolute path run here. It signs whatever the build produced; what it
// prevents is the build's output reaching the keys or files outside its temporary
// directories. The ZIP is the only thing unpacked: it is listed and checked before
// extraction and the result is checked again. Nothing is mounted: the DMG is created
// anew, with the app and an Applications link but no window layout.
export async function signMacos(directory, env, { run = capture, entitlements, scratch = tmpdir() } = {}) {
  const identity = env.APPLE_SIGNING_IDENTITY, keychain = env.APPLE_KEYCHAIN
  if (!identity || !keychain || !entitlements) throw new Error('macOS signing needs APPLE_SIGNING_IDENTITY, APPLE_KEYCHAIN and the entitlements file')
  const credentials = notarizationKey(env)
  const { artifacts, updater, ...info } = await readBuildManifest(directory)
  const only = (suffix, required = true) => {
    const matches = artifacts.filter((artifact) => artifact.file.endsWith(suffix))
    if (matches.length > 1 || (required && !matches.length)) throw new Error(`Expected exactly one ${suffix} artifact`)
    return matches[0] && path.join(directory, matches[0].file)
  }
  const zip = only('.app.zip'), dmg = only('.dmg'), archive = only('.app.tar.gz', false)
  const entries = listZip(await readFile(zip))
  const appName = checkAppArchive(entries)
  // A second, independent listing: refuse if Info-ZIP sees different entries.
  const listed = (await run(tool('zipinfo'), ['-1', zip])).split('\n').filter(Boolean).sort()
  if (listed.join('\n') !== entries.map((entry) => entry.name).sort().join('\n')) {
    throw new Error('zipinfo lists different entries than the ZIP directory')
  }
  // Separate private directories, none shared with the keychain or the checkout.
  const unpacked = await mkdtemp(path.join(scratch, 'sideband-unpack-'))
  const staging = await mkdtemp(path.join(scratch, 'sideband-dmg-'))
  const secrets = await mkdtemp(path.join(scratch, 'sideband-notary-'))
  try {
    await run(tool('ditto'), ['-x', '-k', ...dittoPlain, zip, unpacked])
    const app = await checkExtractedApp(unpacked, appName)
    // Tauri clears extended attributes before signing; do the same for the unpacked copy.
    await run(tool('xattr'), ['-cr', app])
    const timestamp = identity === '-' ? [] : ['--timestamp']
    await run(tool('codesign'), ['--force', '--options', 'runtime', ...timestamp, '--entitlements', entitlements,
      '--keychain', keychain, '--sign', identity, app])
    await run(tool('codesign'), ['--verify', '--deep', '--strict', '--verbose=2', app])
    await rm(zip)
    await run(tool('ditto'), ['-c', '-k', ...dittoPlain, '--keepParent', app, zip])
    if (archive) {
      await rm(archive)
      await run(tool('tar'), archiveArgs(app, archive), { env: { PATH: '/usr/bin:/bin', COPYFILE_DISABLE: '1' } })
    }
    const root = path.join(staging, 'root')
    await mkdir(root)
    await run(tool('ditto'), [app, path.join(root, appName)])
    await symlink('/Applications', path.join(root, 'Applications'))
    await rm(dmg)
    await run(tool('hdiutil'), ['create', '-volname', appName.slice(0, -'.app'.length), '-srcfolder', root,
      '-fs', 'HFS+', '-format', 'UDZO', '-ov', dmg])
    await run(tool('codesign'), ['--force', ...timestamp, '--keychain', keychain, '--sign', identity, dmg])
    if (credentials) {
      // The submission covers the app inside the DMG, so the ZIP and updater archive
      // are notarized too; only the DMG carries a stapled ticket.
      await notarize(dmg, credentials, secrets, run)
      await run(tool('xcrun'), ['stapler', 'staple', dmg])
    }
  } finally {
    for (const dir of [unpacked, staging, secrets]) await rm(dir, { recursive: true, force: true })
  }
  return writeManifest(directory, info, artifacts.map((artifact) => artifact.file), updater?.map((item) => item.file))
}

async function main() {
  if (process.argv[2] === 'sign-macos') {
    // INPUT holds one downloaded artifact directory per macOS target.
    const input = process.argv[3]
    if (!input) throw new Error('Usage: node scripts/build-test-desktop.mjs sign-macos INPUT_DIRECTORY')
    const entitlements = path.join(desktop, 'src-tauri', 'Entitlements.plist')
    let signed = 0
    for (const entry of await readdir(input, { withFileTypes: true })) {
      if (!entry.isDirectory()) continue
      await signMacos(path.join(input, entry.name), process.env, { entitlements, scratch: process.env.RUNNER_TEMP || tmpdir() })
      console.log('Signed ' + entry.name + (notarizationKey(process.env) ? ' and notarized its DMG' : ' (not notarized: no APPLE_API_* key)'))
      signed++
    }
    if (!signed) throw new Error('No macOS artifacts to sign')
    return
  }
  assertNoSigningSecrets(process.env)
  const rust = execFileSync('rustc', ['-vV'], { cwd: root, encoding: 'utf8' })
  const host = /^host: (.+)$/m.exec(rust)?.[1]
  const options = parseOptions(process.argv.slice(2), host, process.platform)
  const config = JSON.parse(await readFile(path.join(desktop, 'src-tauri', 'tauri.conf.json'), 'utf8'))
  const git = (...args) => execFileSync('git', args, { cwd: root, encoding: 'utf8' }).trim()
  const identity = makeIdentity({
    version: options.version ?? config.version, commit: git('rev-parse', 'HEAD'),
    status: git('status', '--porcelain'), target: options.target,
    debug: options.debug, ci: process.env.GITHUB_ACTIONS === 'true',
  })
  const targetDir = path.join(root, 'target')
  const outputs = path.join(targetDir, 'test-builds')
  await mkdir(outputs, { recursive: true })
  const output = await mkdtemp(path.join(outputs, options.target + '-'))
  const args = ['build', '--ci', '--config', 'src-tauri/tauri.test.conf.json',
    '--bundles', options.bundles.join(',')]
  // Native builds reuse Cargo's normal cache; cross-architecture builds have a target subdirectory.
  if (options.target !== host) args.push('--target', options.target)
  if (options.version) args.push('--config', JSON.stringify({ version: options.version }))
  if (options.debug) args.push('--debug')
  const updaterConfig = options.updates ? updateConfiguration(process.env, options.target) : undefined
  if (updaterConfig) args.push('--config', JSON.stringify(updaterConfig))
  args.push('--', '--locked')
  const env = buildEnvironment(process.env, options, identity, targetDir, updaterConfig)
  const started = Date.now()
  run(process.execPath, [path.join(desktop, 'node_modules', '@tauri-apps', 'cli', 'tauri.js'), ...args], { env })
  const bundleDir = path.join(targetDir, ...(options.target === host ? [] : [options.target]),
    identity.profile, 'bundle')
  const app = path.join(bundleDir, 'macos', config.productName + '.app')
  const collected = []
  for (const bundle of options.bundles) {
    if (bundle === 'app') {
      // ditto keeps symlinks and permissions. No resource forks, extended attributes,
      // quarantine or ACLs: no __MACOSX sidecars, which the signing job refuses.
      const name = config.productName + '-test-' + identity.version + '.app.zip'
      run('/usr/bin/ditto', ['-c', '-k', ...dittoPlain, '--keepParent', app, path.join(output, name)])
      collected.push(name)
      continue
    }
    const suffix = { dmg: '.dmg', nsis: '.exe', msi: '.msi', deb: '.deb', appimage: '.AppImage' }[bundle]
    const directory = path.join(bundleDir, bundle)
    const candidates = (await filesBelow(directory)).filter((file) => file.endsWith(suffix))
    let count = 0
    for (const file of candidates) {
      if ((await stat(file)).mtimeMs < started - 1000) continue
      const name = path.basename(file)
      await cp(file, path.join(output, name), { errorOnExist: true, force: false })
      collected.push(name)
      count++
    }
    if (!count) throw new Error('No newly generated ' + bundle + ' artifact; refusing stale output')
  }
  if (options.updates && process.platform === 'darwin') {
    if ((await stat(app)).mtimeMs < started - 1000) throw new Error('No newly generated app bundle; refusing stale output')
    const name = config.productName + '-test-' + identity.version + '.app.tar.gz'
    createUpdaterArchive(app, path.join(output, name))
    collected.push(name)
  }
  const updater = options.updates ? updaterPayloads(collected, process.platform) : undefined
  await cp(path.join(root, 'docs', 'testing-app.md'), path.join(output, 'INSTALL.md'))
  await writeManifest(output, identity, collected, updater)
  if (process.env.GITHUB_ACTIONS === 'true' && process.env.GITHUB_OUTPUT) {
    await appendFile(process.env.GITHUB_OUTPUT, 'artifact_directory=' + output + '\n')
  }
  console.log('Test artifacts: ' + output)
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => { console.error(error.message); process.exitCode = 1 })
}
