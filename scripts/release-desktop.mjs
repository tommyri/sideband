import { execFileSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { appendFile, copyFile, mkdir, mkdtemp, readFile, readdir, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { validateVersion, updateConfiguration } from './build-test-desktop.mjs'

const platforms = {
  windows: { os: 'windows-latest', target: 'x86_64-pc-windows-msvc', label: 'windows-x64', suffixes: ['.exe', '.msi'] },
  macos: { os: 'macos-14', target: 'aarch64-apple-darwin', label: 'macos-arm64', suffixes: ['.dmg', '.app.zip'] },
  macos_intel: { os: 'macos-14', target: 'x86_64-apple-darwin', label: 'macos-x64', suffixes: ['.dmg', '.app.zip'] },
  linux: { os: 'ubuntu-24.04', target: 'x86_64-unknown-linux-gnu', label: 'linux-x64', suffixes: ['.deb', '.AppImage'] },
}

export function selectPlatforms(selection) {
  const include = Object.entries(platforms).filter(([key]) => selection[key] === true)
    .map(([, { os, target }]) => ({ os, target }))
  if (!include.length) throw new Error('Select at least one operating system')
  return { include }
}

const digest = (bytes) => createHash('sha256').update(bytes).digest('hex')

export async function prepareAssets(input, output, version, commit, repo, notes = `Sideband ${version} development preview.`) {
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
      const updates = Array.isArray(identity.updater) ? identity.updater : [identity.updater]
      if (updates.length !== (identity.target.includes('windows') ? 2 : 1)) throw new Error('Missing updater format')
      const entries = {}
      for (const update of updates) {
        const original = identity.artifacts.find(a => a.file === update.file)
        const expected = identity.target.includes('apple') ? '.app.tar.gz' : identity.target.includes('windows') ? (update.file.endsWith('.msi') ? '.msi' : '.exe') : '.AppImage'
        if (!original || !original.file.endsWith(expected) || typeof update.signature !== 'string' || !update.signature.trim()) throw new Error('Invalid updater payload or signature')
        // Immutable payload URLs prevent same-version replacement racing a cached feed.
        const immutable = `${prefix}-${commit}-${original.sha256}${expected}`
        await copyFile(path.join(directory, original.file), path.join(output, immutable))
        files.push(path.join(output, immutable))
        const key = identity.target.includes('apple') ? (identity.target.startsWith('aarch64') ? 'darwin-aarch64' : 'darwin-x86_64') : identity.target.includes('windows') ? `windows-x86_64-${expected === '.msi' ? 'msi' : 'nsis'}` : 'linux-x86_64'
        if (entries[key]) throw new Error('Duplicate updater format')
        entries[key] = { url: `https://github.com/${repo}/releases/download/v${version}/${immutable}`, signature: update.signature }
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

async function main() {
  const version = validateVersion(process.env.RELEASE_VERSION)
  if (process.argv[2] === 'select') {
    const matrix = selectPlatforms(Object.fromEntries(Object.keys(platforms).map((key) => [key, process.env[`RELEASE_${key.toUpperCase()}`] === 'true'])))
    if (process.env.RELEASE_UPDATES === 'true') for (const entry of matrix.include) updateConfiguration(process.env, entry.target)
    await appendFile(process.env.GITHUB_OUTPUT, `matrix=${JSON.stringify(matrix)}\n`)
  } else if (process.argv[2] === 'publish') {
    const output = await mkdtemp(path.join(tmpdir(), 'sideband-release-'))
    const commit = process.env.GITHUB_SHA, repo = process.env.GITHUB_REPOSITORY
    const files = await prepareAssets(process.argv[3], output, version, commit, repo, process.env.RELEASE_NOTES || undefined)
    const notes = path.join(output, 'release-notes.md')
    await writeFile(notes, `Development preview for testing. Without optional OS-signing credentials, Windows installers are unsigned and macOS packages are ad-hoc signed, not notarized. Linux packages target Ubuntu 24.04.\n\nThis version is mutable during development. Reruns replace downloads for selected operating systems; other platforms retain their previous downloads. The tag points to the latest publication; each platform's build-info.json records its exact source commit. Update-enabled builds can install newer signed versions from App updates. Same-version replacements require re-download and reinstall. OS signing status depends on the configured signing credentials; updater signing is separate.\n\nLatest publication: ${commit}\n\nSee the platform INSTALL.md and SHA256SUMS assets for installation instructions and checksums.\n`)
    publishRelease({ repo, commit, version, files, notes })
    const feeds = (await readdir(output)).filter(name => /^(aarch64-apple-darwin|x86_64-apple-darwin|x86_64-pc-windows-msvc|x86_64-unknown-linux-gnu)\.json$/.test(name))
    await publishFeeds({ repo, commit, feeds: feeds.map(name => path.join(output, name)) })
    console.log(`https://github.com/${repo}/releases/tag/v${version}`)
  } else throw new Error('Usage: node scripts/release-desktop.mjs select|publish INPUT_DIRECTORY')
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => { console.error(error.message); process.exitCode = 1 })
}
