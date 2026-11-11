// Release steps, each run as its own workflow step:
//   select          prepare job: inputs and platform matrix (sees only whether the key is set)
//   sign INPUT      publish job: the only step given the updater key
//   stage INPUT OUT publish job: verify signatures and checksums, name the assets
//   publish OUT     publish job: upload the release, then the update feeds
import { execFileSync, spawnSync } from 'node:child_process'
import { createHash, createPublicKey, verify } from 'node:crypto'
import { appendFile, copyFile, mkdir, readFile, readdir, rename, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { parseUpdaterPublicKey, validateVersion, updateConfiguration } from './build-test-desktop.mjs'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const feedPattern = /^(aarch64-apple-darwin|x86_64-apple-darwin|x86_64-pc-windows-msvc|x86_64-unknown-linux-gnu)\.json$/

const platforms = {
  windows: { os: 'windows-2025', target: 'x86_64-pc-windows-msvc', label: 'windows-x64', suffixes: ['.exe', '.msi'] },
  macos: { os: 'macos-15', target: 'aarch64-apple-darwin', label: 'macos-arm64', suffixes: ['.dmg', '.app.zip'] },
  macos_intel: { os: 'macos-15-intel', target: 'x86_64-apple-darwin', label: 'macos-x64', suffixes: ['.dmg', '.app.zip'] },
  linux: { os: 'ubuntu-24.04', target: 'x86_64-unknown-linux-gnu', label: 'linux-x64', suffixes: ['.deb', '.AppImage'] },
}

export function selectPlatforms(selection) {
  const include = Object.entries(platforms).filter(([key]) => selection[key] === true)
    .map(([, { os, target }]) => ({ os, target }))
  if (!include.length) throw new Error('Select at least one operating system')
  return { include }
}

export function selectRelease(env) {
  validateVersion(env.RELEASE_VERSION)
  const matrix = selectPlatforms(Object.fromEntries(Object.keys(platforms).map((key) => [key, env[`RELEASE_${key.toUpperCase()}`] === 'true'])))
  if (env.RELEASE_UPDATES === 'true') {
    // Fail before spending runner time; the key itself is given only to the signing step.
    if (env.TAURI_SIGNING_KEY_CONFIGURED !== 'true') throw new Error('Signed updates need the updater signing key (secret TAURI_SIGNING_PRIVATE_KEY in the desktop-release environment)')
    for (const entry of matrix.include) updateConfiguration(env, entry.target)
  }
  return matrix
}

const digest = (bytes) => createHash('sha256').update(bytes).digest('hex')

// Tauri updater signatures are base64 minisign signatures over the BLAKE2b-512 hash of
// the payload; the trusted comment carries the version the updater requires.
export function verifyUpdaterSignature({ bytes, signature, publicKey, version }) {
  const { keyId, key } = parseUpdaterPublicKey(publicKey)
  const lines = Buffer.from(String(signature ?? '').trim(), 'base64').toString('utf8').split('\n')
  const box = Buffer.from(lines[1] ?? '', 'base64'), global = Buffer.from(lines[3] ?? '', 'base64')
  const trusted = lines[2]?.startsWith('trusted comment: ') ? lines[2].slice('trusted comment: '.length) : undefined
  if (box.length !== 74 || box.subarray(0, 2).toString() !== 'ED' || global.length !== 64 || trusted === undefined) {
    throw new Error('Malformed updater signature')
  }
  if (!box.subarray(2, 10).equals(keyId)) throw new Error('Updater signature was made with a different key than TAURI_UPDATER_PUBLIC_KEY')
  const publicKeyObject = createPublicKey({ key: { kty: 'OKP', crv: 'Ed25519', x: key.toString('base64url') }, format: 'jwk' })
  const value = box.subarray(10)
  if (!verify(null, createHash('blake2b512').update(bytes).digest(), publicKeyObject, value) ||
      !verify(null, Buffer.concat([value, Buffer.from(trusted)]), publicKeyObject, global)) {
    throw new Error('Updater signature does not verify against TAURI_UPDATER_PUBLIC_KEY')
  }
  const signed = trusted.split('\t').find((field) => field.startsWith('version:'))?.slice('version:'.length)
  if (!signed) throw new Error('Updater signature has no signed version')
  if (signed !== version) throw new Error(`Updater signature is signed for version ${signed}, not ${version}`)
}

function runChecked(command, args, options) {
  const result = spawnSync(command, args, { stdio: ['ignore', 'ignore', 'inherit'], ...options })
  if (result.error) throw result.error
  if (result.status !== 0) throw new Error(`${path.basename(args[0] ?? command)} failed with status ${result.status}`)
}

// The signer gets the key and nothing else from the job environment.
export async function signUpdaters({ input, version, env, cli, run = runChecked }) {
  validateVersion(version)
  if (!env.TAURI_SIGNING_PRIVATE_KEY) throw new Error('TAURI_SIGNING_PRIVATE_KEY is not set in the desktop-release environment')
  const signerEnv = {
    PATH: env.PATH, CI: 'true', TAURI_SIGNING_PRIVATE_KEY: env.TAURI_SIGNING_PRIVATE_KEY,
    TAURI_SIGNING_PRIVATE_KEY_PASSWORD: env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD ?? '',
  }
  let signed = 0
  for (const entry of await readdir(input, { withFileTypes: true })) {
    if (!entry.isDirectory()) continue
    const directory = path.join(input, entry.name)
    const identity = JSON.parse(await readFile(path.join(directory, 'build-info.json'), 'utf8'))
    if (!identity.updater) continue
    if (identity.version !== version) throw new Error('Artifact version does not match this release')
    for (const { file } of identity.updater) {
      const artifact = identity.artifacts.find((item) => item.file === file)
      if (!artifact || path.basename(file) !== file) throw new Error('Updater payload is not a listed artifact')
      const payload = path.join(directory, file)
      const bytes = await readFile(payload)
      // The build job wrote both the payload and build-info.json: this catches
      // corruption between jobs, not tampering. Whatever the build produced is signed.
      if (bytes.length !== artifact.bytes || digest(bytes) !== artifact.sha256) throw new Error('Updater payload checksum mismatch')
      await run(process.execPath, [cli, 'signer', 'sign', '--app-version', version, payload], { env: signerEnv })
      verifyUpdaterSignature({ bytes, signature: await readFile(payload + '.sig', 'utf8'), publicKey: env.TAURI_UPDATER_PUBLIC_KEY, version })
      signed++
    }
  }
  if (!signed) throw new Error('No updater payloads to sign')
  return signed
}

export async function prepareAssets(input, output, version, commit, repo, notes = `Sideband ${version} development preview.`, publicKey) {
  validateVersion(version)
  if (!/^[a-f0-9]{40}$/.test(commit)) throw new Error('Expected a full source commit')
  await mkdir(output, { recursive: true })
  const files = [], seen = new Set()
  for (const entry of await readdir(input, { withFileTypes: true })) {
    if (!entry.isDirectory()) continue
    const directory = path.join(input, entry.name)
    const identity = JSON.parse(await readFile(path.join(directory, 'build-info.json'), 'utf8'))
    const platform = Object.values(platforms).find((item) => item.target === identity.target)
    if (!platform || seen.has(identity.target) || identity.version !== version || identity.commit !== commit ||
        identity.dirty || identity.profile !== 'release' || identity.origin !== 'github-actions') {
      throw new Error('Artifact identity does not match this release run')
    }
    seen.add(identity.target)
    const prefix = `Sideband-${version}-${platform.label}`
    const artifacts = [], suffixes = new Set()
    for (const artifact of identity.artifacts) {
      if (path.basename(artifact.file) !== artifact.file) throw new Error('Invalid artifact path')
      const suffix = [...platform.suffixes, ...(identity.updater ? ['.app.tar.gz'] : [])].find((item) => artifact.file.endsWith(item))
      if (!suffix || suffixes.has(suffix)) throw new Error('Unexpected or duplicate installer format')
      suffixes.add(suffix)
      const bytes = await readFile(path.join(directory, artifact.file))
      if (bytes.length !== artifact.bytes || digest(bytes) !== artifact.sha256) throw new Error('Installer checksum mismatch')
      const file = prefix + (suffix === '.exe' ? '-setup.exe' : suffix)
      await writeFile(path.join(output, file), bytes)
      artifacts.push({ ...artifact, file })
    }
    if (platform.suffixes.some(suffix => !suffixes.has(suffix))) throw new Error('Missing installer format')
    if (identity.updater) {
      if (!/^[A-Za-z0-9][\w.-]*\/[A-Za-z0-9][\w.-]*$/.test(repo ?? '')) throw new Error('Updater requires a valid repository')
      if (!publicKey) throw new Error('Updater feeds need the updater public key (TAURI_UPDATER_PUBLIC_KEY) to verify signatures')
      const updates = Array.isArray(identity.updater) ? identity.updater : [identity.updater]
      if (updates.length !== (identity.target.includes('windows') ? 2 : 1)) throw new Error('Missing updater format')
      const entries = {}
      for (const update of updates) {
        const original = identity.artifacts.find(a => a.file === update.file)
        const expected = identity.target.includes('apple') ? '.app.tar.gz' : identity.target.includes('windows') ? (update.file.endsWith('.msi') ? '.msi' : '.exe') : '.AppImage'
        if (!original || !original.file.endsWith(expected)) throw new Error('Invalid updater payload')
        let signature
        try { signature = (await readFile(path.join(directory, original.file + '.sig'), 'utf8')).trim() }
        catch { throw new Error(`Missing updater signature for ${original.file}`) }
        // A key mismatch would publish a feed every installed client rejects.
        verifyUpdaterSignature({ bytes: await readFile(path.join(directory, original.file)), signature, publicKey, version })
        update.signature = signature
        // Immutable payload URLs prevent same-version replacement racing a cached feed.
        const immutable = `${prefix}-${commit}-${original.sha256}${expected}`
        await copyFile(path.join(directory, original.file), path.join(output, immutable))
        files.push(path.join(output, immutable))
        const key = identity.target.includes('apple') ? (identity.target.startsWith('aarch64') ? 'darwin-aarch64' : 'darwin-x86_64') : identity.target.includes('windows') ? `windows-x86_64-${expected === '.msi' ? 'msi' : 'nsis'}` : 'linux-x86_64'
        if (entries[key]) throw new Error('Duplicate updater format')
        entries[key] = { url: `https://github.com/${repo}/releases/download/v${version}/${immutable}`, signature }
      }
      if (identity.target.includes('windows') && (!entries['windows-x86_64-msi'] || !entries['windows-x86_64-nsis'])) throw new Error('Both Windows updater formats are required')
      const manifest = { version, notes: `${notes}\n\nSource: ${commit}.`, platforms: entries }
      await writeFile(path.join(output, identity.target + '.json'), JSON.stringify(manifest, null, 2) + '\n')
    }
    const info = prefix + '-build-info.json', install = prefix + '-INSTALL.md'
    await writeFile(path.join(output, info), JSON.stringify({ ...identity, artifacts }, null, 2) + '\n')
    await copyFile(path.join(directory, 'INSTALL.md'), path.join(output, install))
    const names = [...artifacts.map((item) => item.file), info, install].sort()
    const sums = await Promise.all(names.map(async (file) => digest(await readFile(path.join(output, file))) + '  ' + file))
    const checksum = prefix + '-SHA256SUMS'
    await writeFile(path.join(output, checksum), sums.join('\n') + '\n')
    files.push(...names.map((file) => path.join(output, file)), path.join(output, checksum))
  }
  if (!seen.size) throw new Error('No installer artifacts were downloaded')
  return files
}

function gh(args) {
  return execFileSync('gh', args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim()
}

function lookup(endpoint, run) {
  try { return JSON.parse(run(['api', endpoint])) }
  catch (error) {
    if (String(error.stderr).includes('(HTTP 404)')) return null
    throw error
  }
}

export function publishRelease({ repo, commit, version, files, notes, run = gh }) {
  validateVersion(version)
  if (!/^[A-Za-z0-9][\w.-]*\/[A-Za-z0-9][\w.-]*$/.test(repo) || !/^[a-f0-9]{40}$/.test(commit)) throw new Error('Invalid release repository or commit')
  const tag = `v${version}`, base = `repos/${repo}`
  const ref = lookup(`${base}/git/ref/tags/${tag}`, run)
  if (ref) {
    if (ref.object.sha !== commit) run(['api', '--method', 'PATCH', `${base}/git/refs/tags/${tag}`, '-f', `sha=${commit}`, '-F', 'force=true'])
  } else {
    run(['api', '--method', 'POST', `${base}/git/refs`, '-f', `ref=refs/tags/${tag}`, '-f', `sha=${commit}`])
  }
  const release = lookup(`${base}/releases/tags/${tag}`, run)
  run(['release', release ? 'edit' : 'create', tag, '--repo', repo, '--prerelease', '--latest=false',
    '--title', `Sideband ${version} — development preview`, '--notes-file', notes])
  run(['release', 'upload', tag, ...files, '--repo', repo, '--clobber'])
}

export async function publishFeeds({ repo, commit, feeds, run = gh }) {
  if (!feeds.length) return
  const release = lookup(`repos/${repo}/releases/tags/desktop-updates`, run)
  const accepted = []
  for (const file of feeds) {
    const next = JSON.parse(await readFile(file, 'utf8'))
    validateVersion(next.version)
    if (release?.assets?.some(asset => asset.name === path.basename(file))) {
      const previous = JSON.parse(run(['release', 'download', 'desktop-updates', '--repo', repo, '--pattern', path.basename(file), '--output', '-']))
      validateVersion(previous.version)
      const before = previous.version.split('.').map(Number), after = next.version.split('.').map(Number)
      const first = after.findIndex((n, i) => n !== before[i])
      // Publishing an old installer must not roll the update channel backwards.
      if (first !== -1 && after[first] < before[first]) continue
    }
    accepted.push(file)
  }
  if (!accepted.length) return
  if (!release) run(['release', 'create', 'desktop-updates', '--repo', repo, '--target', commit,
    '--prerelease', '--latest=false', '--title', 'Desktop update feeds', '--notes', 'Signed development update manifests.'])
  // Only selected feeds change, after every referenced payload has uploaded.
  run(['release', 'upload', 'desktop-updates', ...accepted, '--repo', repo, '--clobber'])
}

function releaseNotes(repo, commit) {
  return `Development preview for testing. Without optional OS-signing credentials, Windows installers are unsigned and macOS packages are ad-hoc signed, not notarized. Linux packages target Ubuntu 24.04.

This version is mutable during development. Reruns replace downloads for selected operating systems; other platforms retain their previous downloads. The tag points to the latest publication; each platform's build-info.json records its exact source commit. Update-enabled builds can install newer signed versions from App updates. Same-version replacements require re-download and reinstall. OS signing status depends on the configured signing credentials; updater signing is separate.

Latest publication: ${commit}

See the platform INSTALL.md and SHA256SUMS assets for installation instructions and checksums. Every asset has a build provenance attestation:

\`\`\`sh
gh attestation verify FILE --repo ${repo} --signer-workflow ${repo}/.github/workflows/release-desktop.yml --source-ref refs/heads/main
\`\`\`
`
}

async function main() {
  const [command, input, output] = process.argv.slice(2)
  if (command === 'select') {
    await appendFile(process.env.GITHUB_OUTPUT, `matrix=${JSON.stringify(selectRelease(process.env))}\n`)
    return
  }
  const version = validateVersion(process.env.RELEASE_VERSION)
  const commit = process.env.GITHUB_SHA, repo = process.env.GITHUB_REPOSITORY
  if (command === 'sign' && input) {
    const cli = path.join(root, 'apps', 'desktop', 'node_modules', '@tauri-apps', 'cli', 'tauri.js')
    console.log(`Signed ${await signUpdaters({ input, version, env: process.env, cli })} updater payloads`)
  } else if (command === 'stage' && input && output) {
    const assets = path.join(output, 'assets'), feeds = path.join(output, 'feeds')
    await mkdir(feeds, { recursive: true })
    await prepareAssets(input, assets, version, commit, repo, process.env.RELEASE_NOTES || undefined, process.env.TAURI_UPDATER_PUBLIC_KEY || undefined)
    for (const name of await readdir(assets)) if (feedPattern.test(name)) await rename(path.join(assets, name), path.join(feeds, name))
    await writeFile(path.join(output, 'release-notes.md'), releaseNotes(repo, commit))
  } else if (command === 'publish' && input) {
    const assets = path.join(input, 'assets'), feeds = path.join(input, 'feeds')
    const files = (await readdir(assets)).sort().map((name) => path.join(assets, name))
    publishRelease({ repo, commit, version, files, notes: path.join(input, 'release-notes.md') })
    await publishFeeds({ repo, commit, feeds: (await readdir(feeds)).sort().map((name) => path.join(feeds, name)) })
    console.log(`https://github.com/${repo}/releases/tag/v${version}`)
  } else throw new Error('Usage: node scripts/release-desktop.mjs select | sign INPUT | stage INPUT OUTPUT | publish OUTPUT')
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => { console.error(error.message); process.exitCode = 1 })
}
