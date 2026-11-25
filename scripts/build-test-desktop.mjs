import { execFileSync, spawnSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { gunzipSync, inflateRawSync } from 'node:zlib'
import { constants as fsConstants, createReadStream } from 'node:fs'
import { appendFile, copyFile, cp, lstat, mkdir, mkdtemp, readFile, readdir, readlink, realpath, rm, stat, symlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

import { feedTag, nativeVersion, validateVersion } from './desktop-version.mjs'
export { validateVersion } from './desktop-version.mjs'

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
  let noBundle = false
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
      case '--no-bundle': noBundle = true; break
      default: throw new Error('Usage: node scripts/build-test-desktop.mjs [--target TRIPLE] [--bundles app,dmg|nsis,msi|deb,appimage] [--debug] [--version X.Y.Z] [--updates] [--no-bundle]')
    }
  }
  const platform = platforms[target]
  if (!platform || platform.os !== os) throw new Error('Build on the target operating system; unsupported target')
  if (noBundle) {
    // Only Windows bundles in a later job (windows-signing); --bundles would be ignored.
    if (os !== 'win32' || bundles || debug) throw new Error('--no-bundle is only for Windows release builds, without --bundles or --debug')
    return { target, bundles: [], debug, noBundle, ...(version ? { version } : {}), ...(updates ? { updates } : {}) }
  }
  bundles ??= platform.bundles
  if (!bundles.length || bundles.some((bundle) => !platform.bundles.includes(bundle))) {
    throw new Error('Unsupported bundle format for target')
  }
  return { target, bundles: [...new Set(bundles)], debug, ...(version ? { version } : {}), ...(updates ? { updates } : {}) }
}

export function makeIdentity({ version, commit, status, target, debug, ci }) {
  if (!/^[a-f0-9]{40}$/.test(commit)) throw new Error('A full git commit is required')
  return {
    schemaVersion: 1, channel: version.includes('-alpha.') ? 'alpha' : 'test', version, commit,
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
// Apple certificate and notarization key only by the separate macOS signing job; the
// Windows certificate only by the separate Windows signing job, which bundles the
// executable built here with `--no-bundle`.
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
    throw new Error(`Refusing to run with ${present.join(', ')} in the environment; signing secrets belong only to the signing steps (docs/release.md)`)
  }
}

export function buildEnvironment(env, options, identity, targetDir, updaterConfig) {
  const result = { ...env, CI: 'true', CARGO_TARGET_DIR: targetDir, SS_BUILD_IDENTITY: JSON.stringify(identity) }
  // The build is always ad-hoc signed on macOS; signMacos adds the Developer ID later.
  for (const key of [...signingSecrets, 'APPLE_SIGNING_IDENTITY', 'WINDOWS_CERTIFICATE_THUMBPRINT', 'CARGO_BUILD_TARGET', 'SS_UPDATE_PUBLIC_KEY', 'SS_UPDATE_ENDPOINT']) delete result[key]
  if (options.updates) {
    result.SS_UPDATE_PUBLIC_KEY = env.TAURI_UPDATER_PUBLIC_KEY.trim()
    result.SS_UPDATE_ENDPOINT = updaterConfig.plugins.updater.endpoints[0]
  }
  return result
}

export function updateConfiguration(env, target, version) {
  if (!env.TAURI_UPDATER_PUBLIC_KEY) throw new Error('Signed updates require TAURI_UPDATER_PUBLIC_KEY')
  parseUpdaterPublicKey(env.TAURI_UPDATER_PUBLIC_KEY)
  const repo = env.GITHUB_REPOSITORY
  if (!/^[A-Za-z0-9][\w.-]*\/[A-Za-z0-9][\w.-]*$/.test(repo ?? '')) throw new Error('Invalid update repository')
  if (!platforms[target]) throw new Error('Invalid update target')
  return {
    // Tauri would sign updater artifacts inside `tauri build`, which needs the private key
    // there. The installers are the payloads on Windows and Linux; the macOS archive is
    // made by createUpdaterArchive. The release workflow signs them in a separate job.
    // Windows code signing is added by windowsSigningConfig, only in `tauri bundle`.
    bundle: { createUpdaterArtifacts: false, macOS: { signingIdentity: '-' } },
    plugins: { updater: { requireSignedVersion: true, pubkey: env.TAURI_UPDATER_PUBLIC_KEY.trim(),
      endpoints: [`https://github.com/${repo}/releases/download/${feedTag(version)}/${target}.json`],
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

// The extracted tree must be exactly what was validated: every path on disk is a
// listed entry (or a parent directory of one), with the same type and link target,
// and every listed entry exists.
export async function compareTree(root, entries) {
  const expected = new Map(entries.map((entry) => [entry.name.replace(/\/$/, ''), entry]))
  const parents = new Set()
  for (const name of expected.keys()) {
    const parts = name.split('/')
    for (let i = 1; i < parts.length; i++) parents.add(parts.slice(0, i).join('/'))
  }
  const seen = new Set()
  const walk = async (relative) => {
    for (const name of await readdir(path.join(root, relative))) {
      const entryPath = relative ? `${relative}/${name}` : name, file = path.join(root, entryPath)
      const info = await lstat(file)
      const type = info.isSymbolicLink() ? 'symlink' : info.isDirectory() ? 'dir' : info.isFile() ? 'file' : 'other'
      const entry = expected.get(entryPath)
      if (!entry && !(type === 'dir' && parents.has(entryPath))) throw new Error(`Not in the archive listing: ${entryPath}`)
      if (entry && entry.type !== type) throw new Error(`Type differs for ${entryPath}: ${type} on disk, ${entry.type} in the archive`)
      if (type === 'symlink' && await readlink(file) !== entry.target) throw new Error(`Link target differs from the archive: ${entryPath}`)
      seen.add(entryPath)
      if (type === 'dir') await walk(entryPath)
    }
  }
  await walk('')
  for (const name of expected.keys()) if (!seen.has(name)) throw new Error(`In the archive but missing on disk: ${name}`)
}

function onlyArtifact(directory, artifacts, suffix, required = true) {
  const matches = artifacts.filter((artifact) => artifact.file.endsWith(suffix))
  if (matches.length > 1 || (required && !matches.length)) throw new Error(`Expected exactly one ${suffix} artifact`)
  return matches[0] && path.join(directory, matches[0].file)
}

// Phase 1 of the signing job, before any secret is on the runner: list and check the
// app ZIP, compare with zipinfo, extract it into a fresh private directory, then check
// the tree and compare it with the listing. Nothing here signs or needs a credential.
// Downloaded build outputs: actions/download-artifact puts a lone matching artifact
// straight into INPUT and several into one subdirectory each.
export async function artifactDirectories(input) {
  const entries = await readdir(input, { withFileTypes: true })
  if (entries.some((entry) => entry.isFile() && entry.name === 'build-info.json')) return [input]
  return entries.filter((entry) => entry.isDirectory()).map((entry) => path.join(input, entry.name))
}

export async function unpackMacos(directory, { run = capture, scratch = tmpdir() } = {}) {
  const { artifacts } = await readBuildManifest(directory)
  const zip = onlyArtifact(directory, artifacts, '.app.zip')
  const entries = listZip(await readFile(zip))
  const appName = checkAppArchive(entries)
  // A second, independent listing: refuse if Info-ZIP sees different entries.
  const listed = (await run(tool('zipinfo'), ['-1', zip])).split('\n').filter(Boolean).sort()
  if (listed.join('\n') !== entries.map((entry) => entry.name).sort().join('\n')) {
    throw new Error('zipinfo lists different entries than the ZIP directory')
  }
  const unpacked = await mkdtemp(path.join(scratch, 'sideband-unpack-'))
  try {
    await run(tool('ditto'), ['-x', '-k', ...dittoPlain, zip, unpacked])
    await checkExtractedApp(unpacked, appName)
    await compareTree(unpacked, entries)
  } catch (error) {
    await rm(unpacked, { recursive: true, force: true })
    throw error
  }
  return { directory, appName, unpacked }
}

// Phase 2, after the keychain import: re-check the artifacts and the unpacked tree, then
// re-sign the app with the Developer ID, rebuild the ZIP, updater archive and DMG from
// it, sign the DMG, notarize and staple it, and rewrite build-info.json and SHA256SUMS.
// Only system tools by absolute path run here. It signs whatever the build produced;
// what the job prevents is the build's output reaching the keys. Nothing is mounted:
// the DMG is created anew, with the app and an Applications link but no window layout.
export async function signUnpackedMacos(directory, unpacked, env, { run = capture, entitlements, scratch = tmpdir() } = {}) {
  const staging = await mkdtemp(path.join(scratch, 'sideband-dmg-'))
  const secrets = await mkdtemp(path.join(scratch, 'sideband-notary-'))
  try {
    const identity = env.APPLE_SIGNING_IDENTITY, keychain = env.APPLE_KEYCHAIN
    if (!identity || !keychain || !entitlements) throw new Error('macOS signing needs APPLE_SIGNING_IDENTITY, APPLE_KEYCHAIN and the entitlements file')
    const credentials = notarizationKey(env)
    if (path.dirname(unpacked.unpacked) !== path.resolve(scratch) || !path.basename(unpacked.unpacked).startsWith('sideband-unpack-')) {
      throw new Error('Unexpected unpack directory')
    }
    const { artifacts, updater, ...info } = await readBuildManifest(directory)
    const zip = onlyArtifact(directory, artifacts, '.app.zip'), dmg = onlyArtifact(directory, artifacts, '.dmg')
    const archive = onlyArtifact(directory, artifacts, '.app.tar.gz', false)
    const entries = listZip(await readFile(zip))
    if (checkAppArchive(entries) !== unpacked.appName) throw new Error('The app ZIP changed after unpacking')
    const app = await checkExtractedApp(unpacked.unpacked, unpacked.appName)
    await compareTree(unpacked.unpacked, entries)
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
    await run(tool('ditto'), [app, path.join(root, unpacked.appName)])
    await symlink('/Applications', path.join(root, 'Applications'))
    await rm(dmg)
    await run(tool('hdiutil'), ['create', '-volname', unpacked.appName.slice(0, -'.app'.length), '-srcfolder', root,
      '-fs', 'HFS+', '-format', 'UDZO', '-ov', dmg])
    await run(tool('codesign'), ['--force', ...timestamp, '--keychain', keychain, '--sign', identity, dmg])
    if (credentials) {
      // The submission covers the app inside the DMG, so the ZIP and updater archive
      // are notarized too; only the DMG carries a stapled ticket.
      await notarize(dmg, credentials, secrets, run)
      await run(tool('xcrun'), ['stapler', 'staple', dmg])
    }
    return await writeManifest(directory, info, artifacts.map((artifact) => artifact.file), updater?.map((item) => item.file))
  } finally {
    for (const dir of [unpacked.unpacked, staging, secrets]) await rm(dir, { recursive: true, force: true })
  }
}

// Both phases in one call, for local use and tests. The workflow runs them as separate
// steps with the keychain import between them.
export async function signMacos(directory, env, options = {}) {
  return signUnpackedMacos(directory, await unpackMacos(directory, options), env, options)
}

// ---------------------------------------------------------------------------------
// Windows: the build job runs `tauri build --no-bundle` with no certificate and
// uploads the executable. The windows-signing job, on a fresh runner, checks that
// upload and fetches the locked Tauri CLI before any secret (prepare-windows), then,
// after the certificate import, runs `tauri bundle` (bundle-windows). Tauri's bundler
// signs the patched executable, the NSIS plugin DLLs, the uninstaller (makensis runs
// signtool through `!uninstfinalize`), the WiX extension DLLs and both installers.
// `tauri bundle` compiles nothing: it runs `cargo metadata --no-deps`, `rustc -vV`,
// and the NSIS and WiX tools it downloads and checks by hash.

export const windowsTarget = 'x86_64-pc-windows-msvc'
const unbundledFiles = ['SHA256SUMS', 'build-info.json']
const maxExecutable = 1024 * 1024 * 1024

// The executable name Tauri bundles: `mainBinaryName`, else the Cargo package name.
export function mainBinaryName(cargoToml, config) {
  const name = config.mainBinaryName ?? /^\[package\][^[]*?^name\s*=\s*"([^"]+)"/m.exec(cargoToml)?.[1]
  if (!/^[A-Za-z0-9][\w.-]*$/.test(name ?? '')) throw new Error('Cannot determine the main binary name')
  return name + '.exe'
}

// The executable must be an unsigned x64 PE image that still carries Tauri's
// bundle-type marker, which `tauri bundle` patches per installer before signing.
export function checkWindowsExecutable(bytes) {
  const fail = (reason) => { throw new Error(`Not an unsigned x64 Tauri executable: ${reason}`) }
  if (bytes.length < 0x200 || bytes.length > maxExecutable) fail('unexpected size')
  if (bytes.toString('latin1', 0, 2) !== 'MZ') fail('no MZ header')
  const pe = bytes.readUInt32LE(0x3c)
  if (pe < 0x40 || pe + 24 + 240 > bytes.length || bytes.readUInt32LE(pe) !== 0x00004550) fail('no PE header')
  if (bytes.readUInt16LE(pe + 4) !== 0x8664) fail('machine is not x64')
  const characteristics = bytes.readUInt16LE(pe + 22)
  if (!(characteristics & 0x0002) || characteristics & 0x2000) fail('not an executable image')
  const optional = pe + 24
  if (bytes.readUInt16LE(pe + 20) < 240 || bytes.readUInt16LE(optional) !== 0x20b) fail('not PE32+')
  if (bytes.readUInt32LE(optional + 108) < 5) fail('no certificate table entry')
  // Data directory 4 is the Authenticode certificate table: it must be empty.
  if (bytes.readUInt32LE(optional + 112 + 4 * 8) || bytes.readUInt32LE(optional + 112 + 4 * 8 + 4)) fail('it is already signed')
  if (!bytes.includes(Buffer.from('__TAURI_BUNDLE_TYPE_VAR_UNK'))) fail('no unpatched bundle-type marker')
}

// build-info.json for the no-bundle build: the identity, the executable and nothing
// else. SHA256SUMS covers both.
export async function writeUnbundledManifest(output, identity, binary) {
  const file = path.join(output, binary)
  const info = { ...identity, unbundled: binary, artifacts: [{ file: binary, bytes: (await stat(file)).size, sha256: await checksum(file) }] }
  await writeFile(path.join(output, 'build-info.json'), JSON.stringify(info, null, 2) + '\n')
  const lines = await Promise.all([binary, 'build-info.json'].sort().map(async (name) => await checksum(path.join(output, name)) + '  ' + name))
  await writeFile(path.join(output, 'SHA256SUMS'), lines.join('\n') + '\n')
  return info
}

// The downloaded no-bundle output: exactly the executable, build-info.json and
// SHA256SUMS as regular files (no links, junctions or directories), checksums that
// agree, the identity this run expects, and an executable that passes
// checkWindowsExecutable. As on macOS, this catches corruption between jobs, not
// tampering: the build job wrote all three files.
export async function checkUnbundledWindows(directory, binary, expected) {
  if (!(await lstat(directory)).isDirectory()) throw new Error('The Windows build output is not a directory')
  const names = (await readdir(directory)).sort()
  const wanted = [binary, ...unbundledFiles].sort()
  if (names.join('\n') !== wanted.join('\n')) throw new Error(`The Windows build output must hold exactly ${wanted.join(', ')} (has ${names.join(', ') || 'nothing'})`)
  for (const name of names) if (!(await lstat(path.join(directory, name))).isFile()) throw new Error(`Not a regular file: ${name}`)
  const { artifacts, unbundled, ...identity } = await readBuildManifest(directory)
  if (unbundled !== binary || artifacts.length !== 1 || artifacts[0].file !== binary) throw new Error(`build-info.json must list only ${binary}`)
  const sums = await readFile(path.join(directory, 'SHA256SUMS'), 'utf8')
  const lines = await Promise.all(wanted.filter((name) => name !== 'SHA256SUMS').map(async (name) => await checksum(path.join(directory, name)) + '  ' + name))
  if (sums !== lines.join('\n') + '\n') throw new Error('SHA256SUMS does not match the Windows build output')
  const want = { schemaVersion: 1, channel: expected.version.includes('-alpha.') ? 'alpha' : 'test', target: windowsTarget, dirty: false, profile: 'release', ...expected }
  for (const [key, value] of Object.entries(want)) {
    if (identity[key] !== value) throw new Error(`build-info.json ${key} is ${JSON.stringify(identity[key])}, expected ${JSON.stringify(value)}`)
  }
  const extra = Object.keys(identity).filter((key) => !['schemaVersion', 'channel', 'version', 'commit', 'dirty', 'target', 'origin', 'profile'].includes(key))
  if (extra.length) throw new Error(`Unexpected build-info.json fields: ${extra.join(', ')}`)
  checkWindowsExecutable(await readFile(path.join(directory, binary)))
  return { identity, sha256: artifacts[0].sha256 }
}

// The locked Tauri CLI, without a package manager: the two tarballs pnpm-lock.yaml
// pins for @tauri-apps/cli and its Windows binding, checked against the lockfile's
// sha512 integrity. The CLI has no other dependencies and no install script.
const tauriCli = '@tauri-apps/cli'
export const tauriCliBinding = '@tauri-apps/cli-win32-x64-msvc'
const maxTarball = 128 * 1024 * 1024

export function tauriCliPackages(lockText, binding = tauriCliBinding) {
  const importer = [...lockText.matchAll(/\n {6}'@tauri-apps\/cli':\n {8}specifier: [^\n]+\n {8}version: ([^\n]+)\n/g)]
  if (importer.length !== 1 || !/^\d+\.\d+\.\d+$/.test(importer[0][1])) throw new Error('pnpm-lock.yaml must pin exactly one @tauri-apps/cli version')
  const version = importer[0][1]
  const escape = (text) => text.replace(/[.*+?^${}()|[\]\\/]/g, '\\$&')
  const packages = [tauriCli, binding].map((name) => {
    const entry = [...lockText.matchAll(new RegExp(`\\n  '${escape(name)}@${escape(version)}':\\n    resolution:\\s*\\{\\s*integrity: (sha512-[A-Za-z0-9+/]+={0,2}),?\\s*\\}\\n`, 'g'))]
    if (entry.length !== 1) throw new Error(`pnpm-lock.yaml has no plain registry entry for ${name}@${version}`)
    const integrity = entry[0][1]
    if (Buffer.from(integrity.slice('sha512-'.length), 'base64').length !== 64) throw new Error(`Malformed integrity for ${name}`)
    const base = name.split('/')[1]
    return { name, version, integrity, url: `https://registry.npmjs.org/${name}/-/${base}-${version}.tgz` }
  })
  const snapshot = new RegExp(`\\n  '${escape(tauriCli)}@${escape(version)}':\\n    optionalDependencies:\\n(?: {6}[^\\n]+\\n)*? {6}'${escape(binding)}': ${escape(version)}\\n`)
  if (!snapshot.test(lockText)) throw new Error(`pnpm-lock.yaml does not pair ${tauriCli}@${version} with ${binding}@${version}`)
  return packages
}

const integrityOf = (bytes) => 'sha512-' + createHash('sha512').update(bytes).digest('base64')

// A minimal reader for npm package tarballs (ustar, optional pax headers). Only
// regular files and directories under package/ are accepted; links, devices, GNU
// extensions, absolute paths, `..`, backslashes and case-folded duplicates are not.
export function readTar(gzipped) {
  const bytes = gunzipSync(gzipped, { maxOutputLength: 4 * maxTarball })
  const text = (start, length) => { const field = bytes.subarray(start, start + length); const end = field.indexOf(0); return field.subarray(0, end === -1 ? length : end).toString('utf8') }
  const octal = (start, length, label) => {
    if (bytes[start] & 0x80) throw new Error(`Tar ${label} in base-256 is not accepted`)
    const value = text(start, length).trim()
    if (!/^[0-7]*$/.test(value)) throw new Error(`Malformed tar ${label}`)
    return value ? parseInt(value, 8) : 0
  }
  const entries = [], seen = new Set()
  let offset = 0, pax = {}
  for (;;) {
    if (offset + 512 > bytes.length) throw new Error('Truncated tar archive')
    const at = offset, header = bytes.subarray(at, at + 512)
    if (header.every((byte) => byte === 0)) break
    let sum = 0
    for (let i = 0; i < 512; i++) sum += i >= 148 && i < 156 ? 0x20 : header[i]
    if (octal(at + 148, 8, 'checksum') !== sum) throw new Error('Tar header checksum mismatch')
    // POSIX ustar only (npm writes it, with pax headers for long names); not old GNU tar.
    if (header.toString('latin1', 257, 263) !== 'ustar\0') throw new Error('Not a POSIX ustar archive')
    const type = String.fromCharCode(header[156] || 0x30)
    const size = octal(at + 124, 12, 'size')
    const dataStart = at + 512
    if (dataStart + size > bytes.length) throw new Error('Truncated tar entry')
    const data = bytes.subarray(dataStart, dataStart + size)
    offset = dataStart + Math.ceil(size / 512) * 512
    if (type === 'x' || type === 'g') {
      const records = {}
      for (let at = 0; at < data.length;) {
        const space = data.indexOf(0x20, at), length = Number(data.toString('utf8', at, space))
        if (space === -1 || !Number.isInteger(length) || length <= space - at || at + length > data.length) throw new Error('Malformed pax header')
        const record = data.toString('utf8', space + 1, at + length - 1), equals = record.indexOf('=')
        if (equals === -1) throw new Error('Malformed pax record')
        records[record.slice(0, equals)] = record.slice(equals + 1)
        at += length
      }
      if (records.linkpath !== undefined) throw new Error('Tar links are not accepted')
      if (type === 'x') pax = records
      continue
    }
    const prefix = text(at + 345, 155)
    const name = pax.path ?? (prefix ? prefix + '/' : '') + text(at, 100)
    pax = {}
    if (type !== '0' && type !== '5') throw new Error(`Tar entry ${JSON.stringify(name)} is not a file or directory`)
    if (/[\u0000-\u001f\u007f\\:]/.test(name) || name.startsWith('/')) throw new Error(`Refusing tar entry ${JSON.stringify(name)}`)
    const parts = name.replace(/\/$/, '').split('/')
    if (parts[0] !== 'package' || parts.some((part) => part === '' || part === '.' || part === '..')) throw new Error(`Refusing tar entry ${JSON.stringify(name)}`)
    const key = parts.join('/').normalize('NFC').toLowerCase()
    if (seen.has(key)) throw new Error(`Tar entry ${JSON.stringify(name)} appears twice`)
    seen.add(key)
    if (type === '5' && size) throw new Error(`Tar directory ${JSON.stringify(name)} has data`)
    entries.push({ path: parts.slice(1), type: type === '5' ? 'dir' : 'file', data })
  }
  return entries
}

async function fetchBytes(url, fetch) {
  const response = await fetch(url)
  if (!response.ok) throw new Error(`Downloading ${url} failed with HTTP ${response.status}`)
  const bytes = Buffer.from(await response.arrayBuffer())
  if (bytes.length > maxTarball) throw new Error(`${url} is larger than expected`)
  return bytes
}

// Downloads and checks both tarballs, keeps them, and extracts them into
// DESTINATION/node_modules. Returns the CLI entry point. Nothing is run.
export async function installTauriCli(lockText, destination, { fetch = globalThis.fetch, binding = tauriCliBinding } = {}) {
  const packages = tauriCliPackages(lockText, binding)
  await mkdir(path.join(destination, 'tarballs'))
  for (const item of packages) {
    const bytes = await fetchBytes(item.url, fetch)
    if (integrityOf(bytes) !== item.integrity) throw new Error(`${item.name}@${item.version} does not match its pnpm-lock.yaml integrity`)
    await writeFile(path.join(destination, 'tarballs', path.basename(item.url)), bytes, { flag: 'wx' })
    const root = path.join(destination, 'node_modules', ...item.name.split('/'))
    await mkdir(root, { recursive: true })
    for (const entry of readTar(bytes)) {
      const target = path.join(root, ...entry.path)
      if (entry.type === 'dir') await mkdir(target, { recursive: true })
      else {
        await mkdir(path.dirname(target), { recursive: true })
        await writeFile(target, entry.data, { flag: 'wx' })
      }
    }
    const manifest = JSON.parse(await readFile(path.join(root, 'package.json'), 'utf8'))
    if (manifest.name !== item.name || manifest.version !== item.version) throw new Error(`${item.name} tarball holds ${manifest.name}@${manifest.version}`)
    if (Object.keys(manifest.dependencies ?? {}).length) throw new Error(`${item.name} has dependencies; the signing job installs only the CLI`)
  }
  return path.join(destination, 'node_modules', ...tauriCli.split('/'), 'tauri.js')
}

// Before running the CLI: the kept tarballs still match the lockfile, and the
// extracted tree is exactly their contents, byte for byte, with nothing added.
export async function verifyTauriCli(lockText, destination, { binding = tauriCliBinding } = {}) {
  const expected = new Map()
  for (const item of tauriCliPackages(lockText, binding)) {
    const bytes = await readFile(path.join(destination, 'tarballs', path.basename(item.url)))
    if (integrityOf(bytes) !== item.integrity) throw new Error(`${item.name} tarball changed since it was checked`)
    const prefix = ['node_modules', ...item.name.split('/')]
    for (let i = 1; i <= prefix.length; i++) expected.set(prefix.slice(0, i).join('/'), null)
    for (const entry of readTar(bytes)) {
      const parts = [...prefix, ...entry.path]
      for (let i = prefix.length + 1; i < parts.length; i++) if (!expected.has(parts.slice(0, i).join('/'))) expected.set(parts.slice(0, i).join('/'), null)
      expected.set(parts.join('/'), entry.type === 'dir' ? null : entry.data)
    }
  }
  const seen = new Set()
  const walk = async (relative) => {
    for (const name of await readdir(path.join(destination, ...relative))) {
      const parts = [...relative, name], key = parts.join('/'), file = path.join(destination, ...parts)
      if (key === 'tarballs') continue
      const info = await lstat(file)
      if (!expected.has(key)) throw new Error(`Not part of the Tauri CLI packages: ${key}`)
      const data = expected.get(key)
      if (data === null ? !info.isDirectory() : !info.isFile() || !(await readFile(file)).equals(data)) throw new Error(`Tauri CLI file changed: ${key}`)
      seen.add(key)
      if (info.isDirectory()) await walk(parts)
    }
  }
  await walk([])
  for (const key of expected.keys()) if (!seen.has(key)) throw new Error(`Tauri CLI file missing: ${key}`)
  return path.join(destination, 'node_modules', ...tauriCli.split('/'), 'tauri.js')
}

// Phase 1 of the Windows signing job, before any secret is on the runner: check the
// build output, copy the executable into a fresh target directory where `tauri
// bundle` expects it, and fetch and check the locked Tauri CLI.
export async function prepareWindows(input, { binary, expected, lockText, scratch = tmpdir(), host = windowsTarget, fetch, binding } = {}) {
  const { identity, sha256 } = await checkUnbundledWindows(input, binary, expected)
  const targetDir = await mkdtemp(path.join(scratch, 'sideband-bundle-'))
  const cli = await mkdtemp(path.join(scratch, 'sideband-tauri-cli-'))
  try {
    const release = path.join(targetDir, ...(host === windowsTarget ? [] : [windowsTarget]), 'release')
    await mkdir(release, { recursive: true })
    await copyFile(path.join(input, binary), path.join(release, binary), fsConstants.COPYFILE_EXCL)
    if (await checksum(path.join(release, binary)) !== sha256) throw new Error('The staged executable does not match build-info.json')
    await installTauriCli(lockText, cli, { fetch, binding })
  } catch (error) {
    for (const dir of [targetDir, cli]) await rm(dir, { recursive: true, force: true })
    throw error
  }
  return { input, binary, sha256, identity, targetDir, cli }
}

// `tauri bundle` runs build.beforeBundleCommand, a package script, after the
// certificate import. None may be configured, in any file it merges, nor a custom
// sign command.
export function assertNoBundleHooks(configs) {
  for (const [name, config] of Object.entries(configs)) {
    if (config?.build?.beforeBundleCommand !== undefined) throw new Error(`${name} sets build.beforeBundleCommand, which would run after the certificate import`)
    if (config?.bundle?.windows?.signCommand !== undefined) throw new Error(`${name} sets bundle.windows.signCommand`)
  }
}

const thumbprintPattern = /^[0-9A-F]{40}$/

// Tauri signs with signtool from the Windows SDK, selecting the certificate in the
// current user's store by thumbprint. No thumbprint: the installers stay unsigned.
export function windowsSigningConfig(thumbprint) {
  if (!thumbprint) return {}
  if (!thumbprintPattern.test(thumbprint)) throw new Error('WINDOWS_CERTIFICATE_THUMBPRINT is not a SHA-1 thumbprint')
  return { bundle: { windows: { certificateThumbprint: thumbprint, digestAlgorithm: 'sha256', timestampUrl: 'http://timestamp.digicert.com' } } }
}

const mergeConfig = (left, right) => {
  const result = { ...left }
  for (const [key, value] of Object.entries(right)) {
    result[key] = value && typeof value === 'object' && !Array.isArray(value) && left[key] && typeof left[key] === 'object' ? mergeConfig(left[key], value) : value
  }
  return result
}

// Phase 2, after the certificate import: recheck the build output, the staged
// executable and the CLI, then run `tauri bundle` (never `tauri build`) with the
// same configuration the build used plus the signing thumbprint, and write the
// installers, INSTALL.md, build-info.json and SHA256SUMS to OUTPUT.
export async function bundleWindows(state, env, { output, lockText, run, host = windowsTarget, binding } = {}) {
  assertNoSigningSecrets(env)
  const expected = { version: state.identity.version, commit: state.identity.commit, origin: state.identity.origin }
  const { identity, sha256 } = await checkUnbundledWindows(state.input, state.binary, expected)
  const release = path.join(state.targetDir, ...(host === windowsTarget ? [] : [windowsTarget]), 'release')
  if (sha256 !== state.sha256 || await checksum(path.join(release, state.binary)) !== sha256) throw new Error('The staged executable changed since it was checked')
  if ((await readdir(release)).join() !== state.binary) throw new Error('The bundle directory must hold only the staged executable')
  const cli = await verifyTauriCli(lockText, state.cli, { binding })
  const config = mergeConfig(updateConfiguration(env, windowsTarget, identity.version), windowsSigningConfig(env.WINDOWS_CERTIFICATE_THUMBPRINT))
  const args = ['bundle', '--ci', '--bundles', 'nsis,msi', '--config', 'src-tauri/tauri.test.conf.json',
    '--config', JSON.stringify({ version: identity.version, bundle: { windows: { wix: { version: nativeVersion(identity.version) } } } }), '--config', JSON.stringify(config)]
  if (host !== windowsTarget) args.push('--target', windowsTarget)
  const bundleEnv = { ...env, CI: 'true', CARGO_TARGET_DIR: state.targetDir }
  for (const key of ['CARGO_BUILD_TARGET', 'WINDOWS_CERTIFICATE_THUMBPRINT', 'NAPI_RS_NATIVE_LIBRARY_PATH']) delete bundleEnv[key]
  await run(process.execPath, [cli, ...args], { env: bundleEnv })
  await mkdir(output)
  const collected = []
  for (const [bundle, suffix] of [['nsis', '.exe'], ['msi', '.msi']]) {
    const files = (await filesBelow(path.join(release, 'bundle', bundle))).filter((file) => file.endsWith(suffix))
    if (files.length !== 1) throw new Error(`Expected exactly one ${bundle} installer, found ${files.length}`)
    await copyFile(files[0], path.join(output, path.basename(files[0])), fsConstants.COPYFILE_EXCL)
    collected.push(path.basename(files[0]))
  }
  await cp(path.join(root, 'docs', 'testing-app.md'), path.join(output, 'INSTALL.md'))
  return writeManifest(output, identity, collected, updaterPayloads(collected, 'win32'))
}

async function main() {
  const scratch = process.env.RUNNER_TEMP || tmpdir()
  if (process.argv[2] === 'unpack-macos') {
    // INPUT holds one downloaded artifact directory per macOS target; STATE records
    // what was unpacked for the sign-macos step.
    const [input, state] = process.argv.slice(3)
    if (!input || !state) throw new Error('Usage: node scripts/build-test-desktop.mjs unpack-macos INPUT_DIRECTORY STATE_FILE')
    assertNoSigningSecrets(process.env)
    const unpacked = []
    for (const directory of await artifactDirectories(input)) unpacked.push(await unpackMacos(directory, { scratch }))
    if (!unpacked.length) throw new Error('No macOS artifacts to unpack')
    await writeFile(state, JSON.stringify(unpacked, null, 2) + '\n', { mode: 0o600, flag: 'wx' })
    for (const item of unpacked) console.log(`Checked and unpacked ${item.appName} from ${path.basename(item.directory)}`)
    return
  }
  if (process.argv[2] === 'sign-macos') {
    const state = process.argv[3]
    if (!state) throw new Error('Usage: node scripts/build-test-desktop.mjs sign-macos STATE_FILE')
    const entitlements = path.join(desktop, 'src-tauri', 'Entitlements.plist')
    for (const item of JSON.parse(await readFile(state, 'utf8'))) {
      await signUnpackedMacos(item.directory, item, process.env, { entitlements, scratch })
      console.log('Signed ' + path.basename(item.directory) + (notarizationKey(process.env) ? ' and notarized its DMG' : ' (not notarized: no APPLE_API_* key)'))
    }
    return
  }
  if (process.argv[2] === 'prepare-windows' || process.argv[2] === 'bundle-windows') return windowsSigningStep(process.argv.slice(2), scratch)
  assertNoSigningSecrets(process.env)
  const host = rustHost()
  const options = parseOptions(process.argv.slice(2), host, process.platform)
  const config = await tauriConfig()
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
    ...(options.noBundle ? ['--no-bundle'] : ['--bundles', options.bundles.join(',')])]
  // Native builds reuse Cargo's normal cache; cross-architecture builds have a target subdirectory.
  if (options.target !== host) args.push('--target', options.target)
  if (options.version) {
    const numeric = nativeVersion(options.version)
    const bundle = { windows: { wix: { version: numeric } }, macOS: { bundleVersion: numeric } }
    args.push('--config', JSON.stringify({ version: options.version, bundle }))
  }
  if (options.debug) args.push('--debug')
  const updaterConfig = options.updates ? updateConfiguration(process.env, options.target, identity.version) : undefined
  if (updaterConfig) args.push('--config', JSON.stringify(updaterConfig))
  args.push('--', '--locked')
  const env = buildEnvironment(process.env, options, identity, targetDir, updaterConfig)
  const started = Date.now()
  run(process.execPath, [path.join(desktop, 'node_modules', '@tauri-apps', 'cli', 'tauri.js'), ...args], { env })
  const profileDir = path.join(targetDir, ...(options.target === host ? [] : [options.target]), identity.profile)
  if (options.noBundle) {
    const binary = mainBinaryName(await readFile(path.join(desktop, 'src-tauri', 'Cargo.toml'), 'utf8'), config)
    if ((await stat(path.join(profileDir, binary))).mtimeMs < started - 1000) throw new Error('No newly built ' + binary + '; refusing stale output')
    await copyFile(path.join(profileDir, binary), path.join(output, binary), fsConstants.COPYFILE_EXCL)
    await writeUnbundledManifest(output, identity, binary)
    await githubOutputs({ artifact_directory: output, unbundled: true })
    console.log('Unbundled executable for the Windows signing job: ' + output)
    return
  }
  const bundleDir = path.join(profileDir, 'bundle')
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
  await githubOutputs({ artifact_directory: output, unbundled: false })
  console.log('Test artifacts: ' + output)
}

async function githubOutputs(outputs) {
  if (process.env.GITHUB_ACTIONS === 'true' && process.env.GITHUB_OUTPUT) {
    await appendFile(process.env.GITHUB_OUTPUT, Object.entries(outputs).map(([key, value]) => `${key}=${value}\n`).join(''))
  }
}

const rustHost = () => /^host: (.+)$/m.exec(execFileSync('rustc', ['-vV'], { cwd: root, encoding: 'utf8' }))?.[1]
const tauriConfig = async () => JSON.parse(await readFile(path.join(desktop, 'src-tauri', 'tauri.conf.json'), 'utf8'))

// prepare-windows INPUT STATE: before the certificate import. bundle-windows STATE
// OUTPUT: after it. Both refuse to start with a signing secret in their environment;
// the bundle step gets only the certificate's thumbprint.
async function windowsSigningStep([command, first, second], scratch) {
  assertNoSigningSecrets(process.env)
  if (!first || !second) throw new Error('Usage: node scripts/build-test-desktop.mjs prepare-windows INPUT STATE | bundle-windows STATE OUTPUT')
  const lockText = await readFile(path.join(desktop, 'pnpm-lock.yaml'), 'utf8')
  const host = rustHost()
  if (command === 'prepare-windows') {
    const binary = mainBinaryName(await readFile(path.join(desktop, 'src-tauri', 'Cargo.toml'), 'utf8'), await tauriConfig())
    const commit = process.env.GITHUB_SHA || execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim()
    const expected = { version: validateVersion(process.env.RELEASE_VERSION), commit, origin: process.env.GITHUB_ACTIONS === 'true' ? 'github-actions' : 'local' }
    const state = await prepareWindows(first, { binary, expected, lockText, scratch, host })
    await writeFile(second, JSON.stringify(state, null, 2) + '\n', { mode: 0o600, flag: 'wx' })
    console.log(`Checked ${binary} from ${path.basename(first)} and fetched the locked Tauri CLI`)
    return
  }
  const state = JSON.parse(await readFile(first, 'utf8'))
  if (process.env.RELEASE_VERSION !== state.identity.version) throw new Error('RELEASE_VERSION differs from the checked build')
  const tauriDir = path.join(desktop, 'src-tauri'), configs = {}
  for (const name of await readdir(tauriDir)) {
    if (/^tauri(\.[\w-]+)*\.conf\.(json5?|toml)$/.test(name)) {
      if (!name.endsWith('.json')) throw new Error(`Cannot check ${name} for bundle hooks`)
      configs[name] = JSON.parse(await readFile(path.join(tauriDir, name), 'utf8'))
    }
  }
  assertNoBundleHooks(configs)
  await bundleWindows(state, process.env, { output: second, lockText, host, run })
  console.log('Bundled ' + (process.env.WINDOWS_CERTIFICATE_THUMBPRINT ? 'and signed' : '(unsigned: no WINDOWS_CERTIFICATE)') + ' the Windows installers in ' + second)
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => { console.error(error.message); process.exitCode = 1 })
}
