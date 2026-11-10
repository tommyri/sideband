import { execFileSync, spawnSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { createReadStream } from 'node:fs'
import { appendFile, cp, mkdir, mkdtemp, readFile, readdir, stat, writeFile } from 'node:fs/promises'
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
// macros. The updater key is used only by the release workflow's signing step and
// notarization credentials only by the separate notarize step.
export const signingSecrets = [
  'TAURI_SIGNING_PRIVATE_KEY', 'TAURI_SIGNING_PRIVATE_KEY_PASSWORD', 'TAURI_SIGNING_PRIVATE_KEY_PATH',
  'TAURI_PRIVATE_KEY', 'TAURI_PRIVATE_KEY_PASSWORD', 'TAURI_PRIVATE_KEY_PATH',
  'APPLE_ID', 'APPLE_PASSWORD', 'APPLE_TEAM_ID', 'APPLE_API_ISSUER', 'APPLE_API_KEY', 'APPLE_API_KEY_PATH',
  'APPLE_CERTIFICATE', 'APPLE_CERTIFICATE_PASSWORD', 'WINDOWS_CERTIFICATE', 'WINDOWS_CERTIFICATE_PASSWORD',
]

export function assertNoSigningSecrets(env) {
  const present = signingSecrets.filter((key) => env[key])
  if (present.length) {
    throw new Error(`Refusing to build with ${present.join(', ')} in the environment; signing is a separate step (docs/release.md)`)
  }
}

export function buildEnvironment(env, options, identity, targetDir, updaterConfig) {
  const result = { ...env, CI: 'true', CARGO_TARGET_DIR: targetDir, SS_BUILD_IDENTITY: JSON.stringify(identity) }
  for (const key of [...signingSecrets, 'CARGO_BUILD_TARGET', 'SS_UPDATE_PUBLIC_KEY', 'SS_UPDATE_ENDPOINT']) delete result[key]
  // Manual test installers stay ad-hoc signed; --updates opts in to the OS signing identity.
  if (!options.updates || !env.APPLE_SIGNING_IDENTITY) delete result.APPLE_SIGNING_IDENTITY
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
    bundle: { createUpdaterArtifacts: false, macOS: { signingIdentity: env.APPLE_SIGNING_IDENTITY || '-' }, ...windows },
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
export function createUpdaterArchive(app, destination) {
  execFileSync('tar', ['--no-mac-metadata', '--no-xattrs', '--no-acls', '-czf', destination,
    '-C', path.dirname(app), path.basename(app)], { env: { ...process.env, COPYFILE_DISABLE: '1' }, stdio: 'inherit' })
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

const capture = async (command, args) => execFileSync(command, args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'] })

// Runs as its own workflow step after the build, so the Apple password never reaches
// `tauri build`. The DMG contains the signed app, so one submission notarizes the app
// in the ZIP and updater archive too; only the DMG carries a stapled ticket.
export async function notarize(directory, env, run = capture) {
  const missing = ['APPLE_ID', 'APPLE_PASSWORD', 'APPLE_TEAM_ID'].filter((key) => !env[key])
  if (missing.length) throw new Error(`Notarization requires ${missing.join(', ')}`)
  const { artifacts, updater, ...identity } = JSON.parse(await readFile(path.join(directory, 'build-info.json'), 'utf8'))
  const dmgs = artifacts.filter((artifact) => artifact.file.endsWith('.dmg'))
  if (dmgs.length !== 1) throw new Error('Expected exactly one DMG to notarize')
  const dmg = path.join(directory, dmgs[0].file)
  const result = JSON.parse(await run('xcrun', ['notarytool', 'submit', dmg, '--apple-id', env.APPLE_ID,
    '--team-id', env.APPLE_TEAM_ID, '--password', env.APPLE_PASSWORD, '--wait', '--output-format', 'json']))
  if (result.status !== 'Accepted') throw new Error(`Notarization ${result.id ?? ''} finished as ${result.status}; see xcrun notarytool log`)
  await run('xcrun', ['stapler', 'staple', dmg])
  return writeManifest(directory, identity, artifacts.map((artifact) => artifact.file), updater?.map((item) => item.file))
}

async function main() {
  if (process.argv[2] === 'notarize') {
    if (!process.argv[3]) throw new Error('Usage: node scripts/build-test-desktop.mjs notarize ARTIFACT_DIRECTORY')
    await notarize(process.argv[3], process.env)
    console.log('Notarized and stapled the DMG in ' + process.argv[3])
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
      // ditto preserves app symlinks, permissions and extended attributes in the zip.
      const name = config.productName + '-test-' + identity.version + '.app.zip'
      run('ditto', ['-c', '-k', '--sequesterRsrc', '--keepParent', app, path.join(output, name)])
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
