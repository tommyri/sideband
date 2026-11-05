import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdtemp, mkdir, readFile, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { validateVersion } from './build-test-desktop.mjs'
import { prepareAssets, publishRelease, selectPlatforms, publishFeeds } from './release-desktop.mjs'

const commit = 'a'.repeat(40)
test('selection schedules only requested operating systems and rejects empty selection', () => {
  assert.deepEqual(selectPlatforms({ windows: true }).include.map(x => x.target), ['x86_64-pc-windows-msvc'])
  assert.equal(selectPlatforms({ windows: true, macos: true, linux: true }).include.length, 3)
  assert.throws(() => selectPlatforms({}))
  assert.equal(validateVersion('0.0.1'), '0.0.1')
  for (const value of ['v0.0.1', '0.01.1', '1.2', '../1.0.0', '1.2.65536', '256.0.0', '1.256.0', '1.2.3;echo bad', undefined]) assert.throws(() => validateVersion(value))
})

test('assets have stable platform-specific names and reject corrupt or mismatched builds', async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'sideband-release-test-'))
  const input = path.join(root, 'input'), pkg = path.join(input, 'windows'), output = path.join(root, 'output')
  await mkdir(pkg, { recursive: true })
  const bytes = Buffer.from('fixture installer'), sha256 = createHash('sha256').update(bytes).digest('hex')
  const identity = { version: '0.0.1', commit, target: 'x86_64-pc-windows-msvc', dirty: false, profile: 'release', origin: 'github-actions', artifacts: ['installer.exe', 'installer.msi'].map(file => ({ file, bytes: bytes.length, sha256 })) }
  await writeFile(path.join(pkg, 'installer.exe'), bytes)
  await writeFile(path.join(pkg, 'installer.msi'), bytes)
  await writeFile(path.join(pkg, 'INSTALL.md'), 'Instructions')
  await writeFile(path.join(pkg, 'build-info.json'), JSON.stringify(identity))
  const files = await prepareAssets(input, output, '0.0.1', commit)
  assert(files.some(file => file.endsWith('Sideband-0.0.1-windows-x64-setup.exe')))
  assert.match(await readFile(path.join(output, 'Sideband-0.0.1-windows-x64-SHA256SUMS'), 'utf8'), new RegExp(sha256))
  identity.updater = ['installer.exe', 'installer.msi'].map(file => ({ file, signature: 'signature-content' }))
  await writeFile(path.join(pkg, 'build-info.json'), JSON.stringify(identity))
  await prepareAssets(input, output, '0.0.1', commit, 'owner/repo')
  const feed = JSON.parse(await readFile(path.join(output, identity.target + '.json'), 'utf8'))
  assert.equal(feed.platforms['windows-x86_64-nsis'].signature, 'signature-content')
  assert(feed.platforms['windows-x86_64-nsis'].url.includes(commit))
  assert.equal(Object.keys(feed.platforms).length, 2)
  await assert.rejects(prepareAssets(input, output, '0.0.2', commit), /identity/)
  await writeFile(path.join(pkg, 'installer.exe'), 'corrupt')
  await assert.rejects(prepareAssets(input, output, '0.0.1', commit), /checksum/)
})

test('same-version publication moves the tag, edits the prerelease and clobbers only supplied assets', () => {
  const calls = []
  const run = args => {
    calls.push(args)
    if (args[0] === 'api' && args[1].includes('/git/ref/')) return JSON.stringify({ object: { sha: 'b'.repeat(40) } })
    if (args[0] === 'api' && args[1].includes('/releases/tags/')) return '{"id":1}'
    return ''
  }
  publishRelease({ repo: 'owner/sideband', commit, version: '0.0.1', files: ['windows.exe'], notes: 'notes.md', run })
  assert(calls.some(args => args.includes('force=true')))
  assert(calls.some(args => args[0] === 'release' && args[1] === 'edit'))
  assert.deepEqual(calls.at(-1), ['release', 'upload', 'v0.0.1', 'windows.exe', '--repo', 'owner/sideband', '--clobber'])
})

test('first publication creates a tag and prerelease; authentication errors abort', () => {
  const calls = []
  const run = args => {
    calls.push(args)
    if (args[0] === 'api' && args[1].startsWith('repos/')) {
      const error = new Error('Not found'); error.stderr = 'gh: Not Found (HTTP 404)'; throw error
    }
    return ''
  }
  publishRelease({ repo: 'owner/sideband', commit, version: '0.0.1', files: ['windows.exe'], notes: 'notes.md', run })
  assert(calls.some(args => args.includes('POST') && args.includes('ref=refs/tags/v0.0.1')))
  assert(calls.some(args => args[0] === 'release' && args[1] === 'create'))
  assert.throws(() => publishRelease({ repo: 'owner/sideband', commit, version: '0.0.1', files: [], notes: 'notes.md', run: () => { throw new Error('Unauthorized') } }), /Unauthorized/)
})


test('platform feeds never regress and other platforms are not overwritten', async () => {
  const dir = await mkdtemp(path.join(tmpdir(), 'sideband-feed-test-'))
  const file = path.join(dir, 'x86_64-pc-windows-msvc.json')
  const calls = []
  const run = args => {
    calls.push(args)
    if (args[0] === 'api') return JSON.stringify({ assets: [{ name: path.basename(file) }] })
    if (args[1] === 'download') return JSON.stringify({ version: '0.0.2' })
    return ''
  }
  await writeFile(file, JSON.stringify({ version: '0.0.1' }))
  await publishFeeds({ repo: 'owner/repo', commit, feeds: [file], run })
  assert(!calls.some(args => args[1] === 'upload'))
  await writeFile(file, JSON.stringify({ version: '0.0.2' }))
  await publishFeeds({ repo: 'owner/repo', commit, feeds: [file], run })
  assert.deepEqual(calls.at(-1), ['release', 'upload', 'desktop-updates', file, '--repo', 'owner/repo', '--clobber'])
})
