#!/usr/bin/env node
// Fails before `pnpm check` when Node or pnpm differ from apps/desktop/package.json.
// Another pnpm rewrites pnpm-lock.yaml in its own format, and another Node is not
// what CI runs. `just check-desktop` calls this first.
//
// Node must satisfy `engines.node` (only the `>=A.B.C <D` form is used here).
// pnpm must equal `packageManager` exactly. Inside a pnpm script the running
// pnpm is read from npm_config_user_agent; otherwise `pnpm --version` runs in
// apps/desktop, where Corepack or pnpm's own version switching apply.

import { execFileSync } from 'node:child_process'
import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const parse = (version) => version.replace(/^v/, '').split('.').map(Number)

const compare = (a, b) => {
  for (let i = 0; i < 3; i += 1) {
    if (a[i] !== b[i]) return a[i] < b[i] ? -1 : 1
  }
  return 0
}

/** Problems with the given versions, as sentences; empty when both match. */
export function toolchainProblems(manifest, { node, pnpm }) {
  const pinned = /^pnpm@(\d+\.\d+\.\d+)$/.exec(manifest.packageManager ?? '')
  if (!pinned) throw new Error('apps/desktop/package.json has no exact packageManager pnpm@X.Y.Z')
  const range = /^>=(\d+\.\d+\.\d+) <(\d+)$/.exec(manifest.engines?.node ?? '')
  if (!range) throw new Error('apps/desktop/package.json engines.node is not of the form >=A.B.C <D')

  const problems = []
  const current = parse(node)
  if (compare(current, parse(range[1])) < 0 || current[0] >= Number(range[2])) {
    problems.push(`Node ${node.replace(/^v/, '')} is outside engines.node ${manifest.engines.node}; CI uses ${range[1]}.`)
  }
  if (pnpm === null) {
    problems.push(`pnpm was not found; install pnpm ${pinned[1]} (packageManager).`)
  } else if (pnpm !== pinned[1]) {
    problems.push(`pnpm ${pnpm} is not the pinned ${pinned[1]}; another pnpm rewrites pnpm-lock.yaml in its own format.`)
  }
  return problems
}

function runningPnpm(desktop) {
  const agent = /^pnpm\/(\S+)/.exec(process.env.npm_config_user_agent ?? '')
  if (agent) return agent[1]
  try {
    return execFileSync('pnpm', ['--version'], { cwd: desktop, encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }).trim()
  } catch {
    return null
  }
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const desktop = join(dirname(fileURLToPath(import.meta.url)), '..', 'apps', 'desktop')
  const manifest = JSON.parse(readFileSync(join(desktop, 'package.json'), 'utf8'))
  // Node is checked first so a wrong Node never starts pnpm.
  let problems = toolchainProblems(manifest, { node: process.version, pnpm: manifest.packageManager.slice('pnpm@'.length) })
  if (problems.length === 0) problems = toolchainProblems(manifest, { node: process.version, pnpm: runningPnpm(desktop) })
  if (problems.length > 0) {
    for (const problem of problems) console.error(`check-desktop: ${problem}`)
    console.error('check-desktop: use the versions in apps/desktop/package.json (see README "Develop").')
    process.exit(1)
  }
}
