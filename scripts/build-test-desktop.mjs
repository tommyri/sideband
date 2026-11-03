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
  'x86_64-pc-windows-msvc': { os: 'win32', bundles: ['nsis'] },
  'x86_64-unknown-linux-gnu': { os: 'linux', bundles: ['deb', 'appimage'] },
}

export function parseOptions(args, host, os) {
  let target = host
  let bundles
  let debug = false
  let version
  for (let i = 0; i < args.length; i++) {
    switch (args[i]) {
      case '--target': target = args[++i]; break
      case '--bundles':
        if (!args[i + 1]) throw new Error('--bundles requires a format list')
        bundles = args[++i].split(',')
        break
      case '--version': version = validateVersion(args[++i]); break
      case '--debug': debug = true; break
      default: throw new Error('Usage: node scripts/build-test-desktop.mjs [--target TRIPLE] [--bundles app,dmg|nsis|deb,appimage] [--debug] [--version X.Y.Z]')
    }
  }
  const platform = platforms[target]
  if (!platform || platform.os !== os) throw new Error('Build on the target operating system; unsupported target')
  bundles ??= platform.bundles
  if (!bundles.length || bundles.some((bundle) => !platform.bundles.includes(bundle))) {
    throw new Error('Unsupported bundle format for target')
  }
  return { target, bundles: [...new Set(bundles)], debug, ...(version ? { version } : {}) }
}

export function validateVersion(version) {
  if (typeof version !== 'string' || !/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(version) ||
      version.split('.').some((part) => Number(part) > 65535)) {
    throw new Error('Version must be X.Y.Z with components between 0 and 65535')
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

async function main() {
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
  args.push('--', '--locked')
  const env = { ...process.env, CI: 'true', CARGO_TARGET_DIR: targetDir, SS_BUILD_IDENTITY: JSON.stringify(identity) }
  delete env.CARGO_BUILD_TARGET
  // This test channel never notarizes or consumes production signing credentials.
  for (const key of ['APPLE_SIGNING_IDENTITY', 'APPLE_ID', 'APPLE_PASSWORD', 'APPLE_TEAM_ID',
    'APPLE_API_ISSUER', 'APPLE_API_KEY', 'APPLE_API_KEY_PATH', 'TAURI_SIGNING_PRIVATE_KEY',
    'TAURI_SIGNING_PRIVATE_KEY_PASSWORD']) delete env[key]
  const started = Date.now()
  run(process.execPath, [path.join(desktop, 'node_modules', '@tauri-apps', 'cli', 'tauri.js'), ...args], { env })
  const bundleDir = path.join(targetDir, ...(options.target === host ? [] : [options.target]),
    identity.profile, 'bundle')
  const collected = []
  for (const bundle of options.bundles) {
    if (bundle === 'app') {
      const app = path.join(bundleDir, 'macos', config.productName + '.app')
      // ditto preserves app symlinks, permissions and extended attributes in the zip.
      const name = config.productName + '-test-' + identity.version + '.app.zip'
      run('ditto', ['-c', '-k', '--sequesterRsrc', '--keepParent', app, path.join(output, name)])
      collected.push(name)
      continue
    }
    const suffix = { dmg: '.dmg', nsis: '.exe', deb: '.deb', appimage: '.AppImage' }[bundle]
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
  const artifacts = await Promise.all(collected.sort().map(async (file) => ({
    file, bytes: (await stat(path.join(output, file))).size,
    sha256: await checksum(path.join(output, file)),
  })))
  await writeFile(path.join(output, 'build-info.json'), JSON.stringify({ ...identity, artifacts }, null, 2) + '\n')
  await cp(path.join(root, 'docs', 'testing-app.md'), path.join(output, 'INSTALL.md'))
  const checksummed = [...collected, 'build-info.json', 'INSTALL.md'].sort()
  const lines = await Promise.all(checksummed.map(async (file) => await checksum(path.join(output, file)) + '  ' + file))
  await writeFile(path.join(output, 'SHA256SUMS'), lines.join('\n') + '\n')
  if (process.env.GITHUB_ACTIONS === 'true' && process.env.GITHUB_OUTPUT) {
    await appendFile(process.env.GITHUB_OUTPUT, 'artifact_directory=' + output + '\n')
  }
  console.log('Test artifacts: ' + output)
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => { console.error(error.message); process.exitCode = 1 })
}
