import assert from 'node:assert/strict'
import test from 'node:test'
import {
  checkEnvironment, compareVersions, crateVersion, planServerRelease, registryState, releaseExists,
  serverVersionFromRef,
} from './release-gate.mjs'

const reviewed = {
  deployment_branch_policy: { protected_branches: false, custom_branch_policies: true },
  protection_rules: [{ type: 'required_reviewers', reviewers: [{ type: 'User', reviewer: { login: 'maintainer' } }] }, { type: 'branch_policy' }],
}
const mainOnly = { branch_policies: [{ name: 'main', type: 'branch' }] }

test('release environments must exist, allow only their ref and require a reviewer', () => {
  checkEnvironment('desktop-release', reviewed, mainOnly)
  checkEnvironment('server-release', reviewed, { branch_policies: [{ name: 'server-v*', type: 'tag' }] })
  checkEnvironment('desktop-signing', { ...reviewed, protection_rules: [] }, mainOnly)
  assert.throws(() => checkEnvironment('desktop-release', null, null), /does not exist/)
  // GitHub creates an unprotected environment the first time a job names it.
  assert.throws(() => checkEnvironment('desktop-release', { deployment_branch_policy: null, protection_rules: [] }, null), /selected branches.*exactly branch:main.*reviewer/)
  assert.throws(() => checkEnvironment('desktop-release', { ...reviewed, protection_rules: [] }, mainOnly), /reviewer/)
  assert.throws(() => checkEnvironment('desktop-release', reviewed, { branch_policies: [...mainOnly.branch_policies, { name: 'task/*', type: 'branch' }] }), /exactly branch:main/)
  assert.throws(() => checkEnvironment('desktop-release', { ...reviewed, deployment_branch_policy: { protected_branches: true, custom_branch_policies: false } }, { branch_policies: [] }), /selected branches/)
  assert.throws(() => checkEnvironment('server-release', reviewed, mainOnly), /exactly tag:server-v\*/)
  assert.throws(() => checkEnvironment('production', reviewed, mainOnly), /Unknown/)
})

test('server version comes only from a server-vX.Y.Z tag and must match the crate', () => {
  assert.equal(serverVersionFromRef('refs/tags/server-v0.2.0'), '0.2.0')
  for (const ref of ['refs/heads/main', 'refs/heads/server-v0.2.0', 'refs/tags/v0.2.0', 'refs/tags/server-v0.2', 'refs/tags/server-v01.2.0', 'refs/tags/server-v0.2.0-rc1', undefined]) {
    assert.throws(() => serverVersionFromRef(ref), /server-vX\.Y\.Z/)
  }
  const workspace = '[workspace]\nmembers = []\n\n[workspace.package]\nedition = "2024"\nversion = "0.1.0"\n\n[workspace.dependencies]\nfoo = { version = "9.9.9" }\n'
  assert.equal(crateVersion(workspace, '[package]\nname = "ss-server"\nversion.workspace = true\n\n[dependencies.bar]\nversion = "7.0.0"\n'), '0.1.0')
  assert.equal(crateVersion(workspace, '[package]\nname = "ss-server"\nversion = "0.3.0"\n'), '0.3.0')
  assert.throws(() => crateVersion('[workspace]\n', '[package]\nversion.workspace = true\n'), /version/)
})

test('versions compare numerically', () => {
  assert.equal(compareVersions('0.10.0', '0.9.9'), 1)
  assert.equal(compareVersions('1.0.0', '1.0.0'), 0)
  assert.equal(compareVersions('0.0.9', '0.1.0'), -1)
})

test('an existing server version is refused unless forced, and latest only moves forward', () => {
  const base = { version: '0.2.0', crateVersion: '0.2.0', versionExists: false, releaseExists: false, latestVersion: '0.1.0', force: false }
  assert.deepEqual(planServerRelease(base), { replace: false, moveLatest: true })
  assert.deepEqual(planServerRelease({ ...base, latestVersion: null }), { replace: false, moveLatest: true })
  // The default 0.0.1 input on a server-v0.2.0 tag was the reported overwrite.
  assert.throws(() => planServerRelease({ ...base, crateVersion: '0.1.0' }), /does not match/)
  assert.throws(() => planServerRelease({ ...base, versionExists: true }), /image tag 0\.2\.0 already exists.*force/)
  assert.throws(() => planServerRelease({ ...base, releaseExists: true }), /release server-v0\.2\.0 already exists/)
  assert.deepEqual(planServerRelease({ ...base, versionExists: true, releaseExists: true, force: true }), { replace: true, moveLatest: true })
  // Forcing an older version replaces that version only.
  assert.deepEqual(planServerRelease({ ...base, version: '0.1.5', crateVersion: '0.1.5', versionExists: true, latestVersion: '0.2.0', force: true }), { replace: true, moveLatest: false })
  assert.deepEqual(planServerRelease({ ...base, latestVersion: '0.2.0' }), { replace: false, moveLatest: true })
})

function fakeRegistry(routes) {
  const seen = []
  const fetch = async (url, init = {}) => {
    seen.push({ url, method: init.method ?? 'GET', headers: init.headers ?? {} })
    const key = `${init.method ?? 'GET'} ${url.replace('https://ghcr.io', '')}`
    const body = routes[key]
    if (body === undefined) return new Response('{}', { status: 404 })
    if (typeof body === 'number') return new Response('{}', { status: body })
    return new Response(JSON.stringify(body), { status: 200 })
  }
  return { fetch, seen }
}

test('registry state reads the version tag and the version label behind latest', async () => {
  const image = 'owner/repo-server'
  const routes = {
    [`GET /token?scope=repository:${image}:pull&service=ghcr.io`]: { token: 'pull-token' },
    [`HEAD /v2/${image}/manifests/0.2.0`]: 404,
    [`GET /v2/${image}/manifests/latest`]: { manifests: [
      { digest: 'sha256:arm', platform: { os: 'linux', architecture: 'arm64' } },
      { digest: 'sha256:amd', platform: { os: 'linux', architecture: 'amd64' } }] },
    [`GET /v2/${image}/manifests/sha256:amd`]: { config: { digest: 'sha256:config' } },
    [`GET /v2/${image}/blobs/sha256:config`]: { config: { Labels: { 'org.opencontainers.image.version': '0.1.0' } } },
  }
  const { fetch, seen } = fakeRegistry(routes)
  assert.deepEqual(await registryState(image, '0.2.0', { fetch, auth: 'secret' }), { versionExists: false, latestVersion: '0.1.0' })
  assert.equal(seen[0].headers.authorization, 'Basic secret')
  assert(seen.slice(1).every(request => request.headers.authorization === 'Bearer pull-token'))

  routes[`HEAD /v2/${image}/manifests/0.2.0`] = {}
  assert.equal((await registryState(image, '0.2.0', fakeRegistry(routes))).versionExists, true)

  routes[`GET /v2/${image}/blobs/sha256:config`] = { config: { Labels: {} } }
  await assert.rejects(registryState(image, '0.2.0', fakeRegistry(routes)), /version label/)

  delete routes[`GET /v2/${image}/manifests/latest`]
  assert.deepEqual(await registryState(image, '0.2.0', fakeRegistry(routes)), { versionExists: true, latestVersion: null })

  // Anything but 200 or 404 is not proof that a tag is absent.
  routes[`HEAD /v2/${image}/manifests/0.2.0`] = 403
  await assert.rejects(registryState(image, '0.2.0', fakeRegistry(routes)), /403/)
})

test('release lookup distinguishes missing from failed', async () => {
  const api = (status) => async () => new Response('{}', { status })
  assert.equal(await releaseExists('owner/repo', 'server-v0.2.0', { fetch: api(200) }), true)
  assert.equal(await releaseExists('owner/repo', 'server-v0.2.0', { fetch: api(404) }), false)
  await assert.rejects(releaseExists('owner/repo', 'server-v0.2.0', { fetch: api(401) }), /401/)
})
