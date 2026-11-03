// Rebuild the desktop icon family from the adopted offset-bands vector master.
import { execFileSync } from 'node:child_process'
import { copyFileSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const brand = path.join(root, 'apps/desktop/public/brand')
const icons = path.join(root, 'apps/desktop/src-tauri/icons')
const cli = path.join(root, 'apps/desktop/node_modules/@tauri-apps/cli/tauri.js')
const mark = readFileSync(path.join(brand, 'sideband-mark.svg'), 'utf8')
const shapes = mark.slice(mark.indexOf('  <g'), mark.lastIndexOf('</svg>'))
const app = `<svg xmlns="http://www.w3.org/2000/svg" width="512" height="512" viewBox="0 0 512 512">
  <rect x="16" y="16" width="480" height="480" rx="112" fill="#24282b"/>
  <g transform="translate(64 64) scale(.75)">
${shapes.replace('#00b79c', '#faf7f0')}  </g>
</svg>
`
writeFileSync(path.join(brand, 'sideband-app.svg'), app)
const output = mkdtempSync(path.join(tmpdir(), 'sideband-icons-'))
execFileSync(process.execPath, [cli, 'icon', path.join(brand, 'sideband-app.svg'), '-o', output], { stdio: 'inherit' })
for (const name of ['icon.png', 'icon.icns', 'icon.ico']) {
  copyFileSync(path.join(output, name), path.join(icons, name))
}
const tray = path.join(output, 'tray.svg')
writeFileSync(tray, mark.replace('#00b79c', '#000000'))
const trayOutput = path.join(output, 'tray')
execFileSync(process.execPath, [cli, 'icon', tray, '-o', trayOutput, '--png', '32'], { stdio: 'inherit' })
copyFileSync(path.join(trayOutput, '32x32.png'), path.join(icons, 'tray-icon.png'))
