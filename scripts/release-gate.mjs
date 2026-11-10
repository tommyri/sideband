// Checks that run before a release job touches a secret or a registry.
//
//   node scripts/release-gate.mjs environment NAME   environment protection is in place
//   node scripts/release-gate.mjs server-plan        tag, version and overwrite decision
//
// The workflow files can be edited on any branch, so these checks are not the
// security boundary: the GitHub Environment's branch/tag policy is. They make the
// trusted workflow on main fail closed until that policy has been configured.
import { appendFile, readFile } from 'node:fs/promises'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const versionPattern = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/

// What each release environment must enforce before a job may use its secrets.
export const environments = {
  'desktop-release': { reviewers: true, policies: [{ type: 'branch', name: 'main' }] },
  'desktop-signing': { reviewers: false, policies: [{ type: 'branch', name: 'main' }] },
  'server-release': { reviewers: true, policies: [{ type: 'tag', name: 'server-v*' }] },
}

export function checkEnvironment(name, environment, policies) {
  const expected = environments[name]
  if (!expected) throw new Error(`Unknown release environment ${name}`)
  if (!environment) throw new Error(`Environment ${name} does not exist; create it as described in docs/release.md`)
  const problems = []
  const policy = environment.deployment_branch_policy
  if (!policy || policy.custom_branch_policies !== true || policy.protected_branches) {
    problems.push('must allow only selected branches and tags')
  }
  const actual = (policies?.branch_policies ?? []).map(({ type, name }) => `${type ?? 'branch'}:${name}`).sort()
  const wanted = expected.policies.map(({ type, name }) => `${type}:${name}`).sort()
  if (actual.join() !== wanted.join()) problems.push(`must allow exactly ${wanted.join(', ')} (has ${actual.join(', ') || 'none'})`)
  if (expected.reviewers && !environment.protection_rules?.find(rule => rule.type === 'required_reviewers')?.reviewers?.length) {
    problems.push('must require a reviewer')
  }
  if (problems.length) throw new Error(`Environment ${name} ${problems.join('; ')}. See docs/release.md.`)
}

export function compareVersions(a, b) {
  const left = a.split('.').map(Number), right = b.split('.').map(Number)
  const index = left.findIndex((part, i) => part !== right[i])
  return index === -1 ? 0 : Math.sign(left[index] - right[index])
}

export function serverVersionFromRef(ref) {
  const version = /^refs\/tags\/server-v(.+)$/.exec(ref ?? '')?.[1]
  if (!versionPattern.test(version ?? '')) {
    throw new Error(`Server releases run only on a server-vX.Y.Z tag (got ${ref}); select the tag when dispatching`)
  }
  return version
}

function section(toml, name) {
  const lines = toml.split('\n')
  const start = lines.findIndex(line => line.trim() === `[${name}]`)
  if (start === -1) return ''
  const end = lines.findIndex((line, i) => i > start && /^\s*\[/.test(line))
  return lines.slice(start + 1, end === -1 ? undefined : end).join('\n')
}

// The binary reports its Cargo version, so the tag must name that version.
export function crateVersion(workspaceToml, crateToml) {
  const own = section(crateToml, 'package')
  const inherited = /^version\.workspace\s*=\s*true\s*$/m.test(own)
  const version = (inherited ? section(workspaceToml, 'workspace.package') : own).match(/^version\s*=\s*"([^"]+)"/m)?.[1]
  if (!version) throw new Error('Cannot read the ss-server version from Cargo.toml')
  return version
}

export function planServerRelease({ version, crateVersion, versionExists, releaseExists, latestVersion, force }) {
  if (version !== crateVersion) {
    throw new Error(`Tag server-v${version} does not match the ss-server version ${crateVersion}; bump the workspace version and tag that commit`)
  }
  const existing = [versionExists && `image tag ${version}`, releaseExists && `GitHub release server-v${version}`].filter(Boolean)
  if (existing.length && !force) throw new Error(`${existing.join(' and ')} already exists; rerun with force to replace it`)
  // latest never moves to an older version, even when an old version is forced.
  return { replace: existing.length > 0, moveLatest: latestVersion === null || compareVersions(version, latestVersion) >= 0 }
}

const manifestTypes = [
  'application/vnd.oci.image.index.v1+json', 'application/vnd.docker.distribution.manifest.list.v2+json',
  'application/vnd.oci.image.manifest.v1+json', 'application/vnd.docker.distribution.manifest.v2+json',
].join(', ')

async function ok(response, what) {
  if (!response.ok) throw new Error(`${what} failed with HTTP ${response.status}`)
  return response.json()
}

// image is OWNER/NAME on ghcr.io. auth is base64 "user:token" for private packages.
export async function registryState(image, version, { fetch = globalThis.fetch, auth } = {}) {
  const { token } = await ok(await fetch(`https://ghcr.io/token?scope=repository:${image}:pull&service=ghcr.io`,
    { headers: auth ? { authorization: `Basic ${auth}` } : {} }), 'GHCR token request')
  const request = (resource, method = 'GET') => fetch(`https://ghcr.io/v2/${image}/${resource}`,
    { method, headers: { authorization: `Bearer ${token}`, accept: manifestTypes } })
  const head = await request(`manifests/${version}`, 'HEAD')
  if (head.status !== 200 && head.status !== 404) throw new Error(`Checking image tag ${version} failed with HTTP ${head.status}`)
  const latest = await request('manifests/latest')
  if (latest.status === 404) return { versionExists: head.status === 200, latestVersion: null }
  let manifest = await ok(latest, 'Reading latest')
  if (manifest.manifests) {
    const entry = manifest.manifests.find(item => item.platform?.os === 'linux' && item.platform?.architecture === 'amd64')
    if (!entry) throw new Error('latest has no linux/amd64 image')
    manifest = await ok(await request(`manifests/${entry.digest}`), 'Reading latest linux/amd64')
  }
  const config = await ok(await request(`blobs/${manifest.config.digest}`), 'Reading latest image config')
  const latestVersion = config.config?.Labels?.['org.opencontainers.image.version']
  if (!versionPattern.test(latestVersion ?? '')) throw new Error('latest has no valid version label; cannot tell whether moving it goes forward')
  return { versionExists: head.status === 200, latestVersion }
}

export async function releaseExists(repo, tag, { fetch = globalThis.fetch, token, api = 'https://api.github.com' } = {}) {
  const response = await fetch(`${api}/repos/${repo}/releases/tags/${tag}`,
    { headers: { accept: 'application/vnd.github+json', ...(token ? { authorization: `Bearer ${token}` } : {}) } })
  if (response.status === 404) return false
  if (!response.ok) throw new Error(`Checking release ${tag} failed with HTTP ${response.status}`)
  return true
}

async function github(resource) {
  const response = await fetch(`${process.env.GITHUB_API_URL ?? 'https://api.github.com'}/repos/${process.env.GITHUB_REPOSITORY}/${resource}`, {
    headers: { accept: 'application/vnd.github+json', ...(process.env.GH_TOKEN ? { authorization: `Bearer ${process.env.GH_TOKEN}` } : {}) },
  })
  if (response.status === 404) return null
  return ok(response, `GET ${resource}`)
}

async function main() {
  const [command, name] = process.argv.slice(2)
  if (command === 'environment') {
    const environment = await github(`environments/${encodeURIComponent(name)}`)
    const policies = environment && await github(`environments/${encodeURIComponent(name)}/deployment-branch-policies`)
    checkEnvironment(name, environment, policies)
    console.log(`Environment ${name} is protected`)
  } else if (command === 'server-plan') {
    const version = serverVersionFromRef(process.env.GITHUB_REF)
    const repo = process.env.GITHUB_REPOSITORY
    const image = `${repo.toLowerCase()}-server`
    const auth = process.env.GH_TOKEN && Buffer.from(`${process.env.GITHUB_ACTOR}:${process.env.GH_TOKEN}`).toString('base64')
    const plan = planServerRelease({
      version,
      crateVersion: crateVersion(await readFile(path.join(root, 'Cargo.toml'), 'utf8'),
        await readFile(path.join(root, 'crates', 'ss-server', 'Cargo.toml'), 'utf8')),
      ...await registryState(image, version, { auth }),
      releaseExists: await releaseExists(repo, `server-v${version}`, { token: process.env.GH_TOKEN }),
      force: process.env.RELEASE_FORCE === 'true',
    })
    const outputs = { version, image: `ghcr.io/${image}`, replace: plan.replace, move_latest: plan.moveLatest }
    console.log(JSON.stringify(outputs))
    if (process.env.GITHUB_OUTPUT) {
      await appendFile(process.env.GITHUB_OUTPUT, Object.entries(outputs).map(([key, value]) => `${key}=${value}\n`).join(''))
    }
  } else throw new Error('Usage: node scripts/release-gate.mjs environment NAME | server-plan')
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => { console.error(error.message); process.exitCode = 1 })
}
