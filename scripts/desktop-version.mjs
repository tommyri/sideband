// Keep the semantic version in the app and signed feed. MSI compares only the
// first three numeric fields, so reserve slots for alpha 1..99 before each final.
const versionPattern = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-alpha\.([1-9]\d?))?$/

export function validateVersion(version) {
  const match = typeof version === 'string' && version.match(versionPattern)
  if (!match || +match[1] > 255 || +match[2] > 255 || +match[3] > 654) {
    throw new Error('Expected X.Y.Z or X.Y.Z-alpha.N (major/minor 0–255, patch 0–654, alpha 1–99)')
  }
  return version
}

export function nativeVersion(version) {
  const [, major, minor, patch, alpha] = validateVersion(version).match(versionPattern)
  if (!alpha && +major === 0 && +minor <= 1 && (+minor === 0 || +patch < 4)) return version
  return [major, minor, +patch * 100 + (alpha ? +alpha - 1 : 99)].join('.')
}

export function feedTag(version) {
  return version?.includes('-alpha.') ? 'desktop-alpha-updates' : 'desktop-updates'
}
