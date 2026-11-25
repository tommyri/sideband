import assert from 'node:assert/strict'
import { after, before, test } from 'node:test'
import { chromium } from 'playwright'
import { preview } from 'vite'
import { connectRoom } from './connected-room.mjs'

const roomName = 'Field recording and late-night listening with the whole production crew'
const draft = 'I can bring the recorder.\nMeet you by the north entrance.'
let browser, server
before(async () => {
  server = await preview({ preview: { host: '127.0.0.1', port: 0, open: false } })
  browser = await chromium.launch()
})
after(async () => {
  try {
    await browser?.close()
  } finally {
    await server?.close()
  }
})

// A locator click can scroll a clipped control into view. Check the original viewport first.
async function assertReachable(control) {
  assert.ok(await control.isEnabled(), 'Control must be enabled')
  const result = await control.evaluate((element) => {
    const { left, top, right, bottom, width, height } = element.getBoundingClientRect()
    const hit = document.elementFromPoint(left + width / 2, top + height / 2)
    return {
      label: element.getAttribute('aria-label') || element.textContent,
      inside:
        width > 0 &&
        height > 0 &&
        left >= 0 &&
        top >= 0 &&
        right <= innerWidth &&
        bottom <= innerHeight,
      clickable: element.contains(hit),
    }
  })
  assert.ok(result.inside && result.clickable, JSON.stringify(result))
}

test('sidebar tools keep their spacing and position while servers scroll', async (t) => {
  const page = await browser.newPage({ viewport: { width: 760, height: 600 } })
  t.after(() => page.close())
  await page.addInitScript(connectRoom, { roomName, extraServers: 29 })
  await page.goto(`http://127.0.0.1:${server.httpServer.address().port}`)
  await page.getByRole('button', { name: 'Channels', exact: true }).click()
  const rail = page.getByRole('navigation', { name: 'Servers' })
  const tools = rail.getByRole('group', { name: 'App tools' })
  const notifications = tools.getByRole('button', { name: 'Notification settings' })
  const add = rail.getByRole('button', { name: 'Add server' })
  for (const viewport of [
    { width: 760, height: 600 },
    { width: 1440, height: 900 },
  ]) {
    await page.setViewportSize(viewport)
    await rail.getByRole('button', { name: 'Night Shift', exact: true }).focus()
    const top = (await notifications.boundingBox()).y
    let bottom
    for (const tile of await tools.getByRole('button').all()) {
      await assertReachable(tile)
      const rect = await tile.boundingBox()
      if (bottom !== undefined) assert.ok(rect.y - bottom >= 8, 'Separate adjacent app tools')
      assert.equal(rect.height, 42)
      bottom = rect.y + rect.height
    }
    await add.focus()
    await assertReachable(add)
    const bounds = await add.boundingBox()
    assert.ok(top - bounds.y - bounds.height >= 12, 'Separate servers from app tools')
    await page.keyboard.press('Tab')
    assert.ok(await notifications.evaluate((el) => document.activeElement === el))
    assert.equal((await notifications.boundingBox()).y, top, 'Only the server list scrolls')
  }
})

async function startReply(t) {
  const page = await browser.newPage({ viewport: { width: 760, height: 600 } })
  t.after(() => page.close())
  page.setDefaultTimeout(5000)
  await page.addInitScript(connectRoom, roomName)
  await page.goto(`http://127.0.0.1:${server.httpServer.address().port}`)
  const button = (name, root = page) => root.getByRole('button', { name, exact: true })
  const dock = page.getByRole('region', { name: 'Voice controls' })
  await dock.getByText(roomName, { exact: true }).waitFor()
  const message = page.getByRole('listitem').filter({ hasText: 'Who can bring a recorder?' })
  await message.hover()
  await button('Reply', message).click()
  const target = page.getByText('Replying to Mina', { exact: true })
  await target.waitFor()
  const input = page.getByRole('textbox', { name: 'Message', exact: true })
  const [first, second] = draft.split('\n')
  await input.fill(first)
  await input.press('Shift+Enter')
  await page.keyboard.insertText(second)
  assert.equal(await input.inputValue(), draft)
  const send = button('Send')
  const leave = button('Leave voice', dock)
  await assertReachable(send)
  await assertReachable(leave)
  return { page, dock, input, target, send, leave, audio: button('Audio settings', dock) }
}

test('reach account linking, sync, and recovery backup in compact personal settings', async (t) => {
  const { page, dock, input } = await startReply(t)
  await page.evaluate(() => {
    const bridge = window.__TAURI_INTERNALS__
    const invoke = bridge.invoke
    bridge.invoke = async (command, args) => {
      if (command === 'get_local_profile') return 'Jules'
      if (command === 'review_profile_account')
        return { status: 'eligible', userId: 'jules', username: 'jules', role: 'Admin' }
      if (command === 'sync_server_list') return { available: [], chosen: null, unchecked: 0 }
      return invoke(command, args)
    }
  })
  await dock.getByRole('button', { name: 'Audio settings', exact: true }).click()
  const entry = page.getByRole('button', { name: 'Profile and devices', exact: true })
  await assertReachable(entry)
  await entry.click()
  const settings = page.getByRole('dialog', { name: 'Profile and devices', exact: true })
  const account = settings.getByRole('button', { name: /^Link existing accounts/ })
  await assertReachable(account)
  await account.click()
  await settings.getByText('Ready to link').waitFor()
  await settings.getByRole('button', { name: 'Back to profile and devices' }).click()
  assert.ok(await account.evaluate((el) => document.activeElement === el))
  await settings.getByRole('button', { name: /^Sync servers/ }).click()
  await settings.getByRole('button', { name: 'Manage recovery backup' }).click()
  await assertReachable(settings.getByRole('button', { name: 'Save backup file' }))
  await page.keyboard.press('Escape')
  await settings.waitFor({ state: 'hidden' })
  assert.ok(await entry.evaluate((el) => document.activeElement === el))
  await page.keyboard.press('Escape')
  assert.equal(await input.inputValue(), draft)
})

test('pairing invitation, matching phrase and approval fit the compact Mac window', async (t) => {
  const { page, dock } = await startReply(t)
  await page.evaluate(() => {
    const bridge = window.__TAURI_INTERNALS__
    const original = bridge.invoke
    bridge.invoke = async (command, args) => {
      if (command === 'get_local_profile') return 'Jules'
      if (command === 'pair_start')
        return {
          kind: 'invite',
          code: 'ABCDE-FGHIJ-KLMNO-PQRST-UVWXY-Z',
          target: { host: 'relay.example', port: 443, httpsPort: 8443 },
        }
      if (command === 'pair_update' && args.action === 'check')
        return {
          kind: 'review',
          device: 'Gaming PC',
          memberships: 10,
          phrase: 'maple · lantern · river',
        }
      if (command === 'pair_update' && args.action === 'approve') return { kind: 'allowed' }
      return original(command, args)
    }
  })
  await dock.getByRole('button', { name: 'Audio settings', exact: true }).click()
  await page.getByRole('button', { name: 'Profile and devices', exact: true }).click()
  const dialog = page.getByRole('dialog', { name: 'Profile and devices', exact: true })
  await dialog.getByRole('button', { name: /^Link a device/ }).click()
  await dialog.getByRole('button', { name: 'Create one-time code' }).click()
  await assertReachable(dialog.getByRole('button', { name: 'Copy pairing link' }))
  assert.equal(
    await dialog.getByLabel('Sync server address').inputValue(),
    'relay.example:443?https=8443',
  )
  await dialog.getByText('maple · lantern · river').waitFor()
  const allow = dialog.getByRole('button', { name: 'Allow Gaming PC' })
  assert.ok(await allow.isDisabled())
  await assertReachable(dialog.getByRole('button', { name: 'Deny request' }))
  await dialog.getByRole('checkbox').check()
  await assertReachable(allow)
  await allow.click()
  await dialog.getByText('Access approved').waitFor()
})

test('a pairing link opens on first launch and the compact Windows view waits for consent', async (t) => {
  const page = await browser.newPage({
    viewport: { width: 760, height: 600 },
    userAgent:
      'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/142.0 Safari/537.36',
  })
  t.after(() => page.close())
  page.setDefaultTimeout(5000)
  await page.addInitScript(() => {
    let linked = false,
      callback = 0
    window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} }
    window.__TAURI_INTERNALS__ = {
      transformCallback: () => ++callback,
      invoke: async (command, args) => {
        if (command === 'current_pair_link')
          return 'sideband://pair?relay=relay.example%3A443%3Fhttps%3D8443#ABCD'
        if (command === 'get_snapshot') return { servers: [] }
        if (command === 'get_local_profile') return linked ? 'Jules' : null
        if (command === 'pair_join')
          return { kind: 'compare', device: args.name, phrase: 'maple · lantern · river' }
        if (command === 'pair_update' && args.action === 'check') return { kind: 'ready' }
        if (command === 'pair_update' && args.action === 'install') {
          linked = true
          return { kind: 'linked', name: 'Jules' }
        }
        if (command === 'plugin:event|listen') return args.handler
        return null
      },
    }
  })
  await page.goto('http://127.0.0.1:' + server.httpServer.address().port)
  const dialog = page.getByRole('dialog', { name: 'Profile and devices' })
  assert.equal(await dialog.getByLabel('Name of this computer').inputValue(), 'Windows PC')
  await assertReachable(dialog.getByRole('button', { name: 'Request access' }))
  await dialog.getByRole('button', { name: 'Request access' }).click()
  await dialog.getByText('maple · lantern · river').waitFor()
  await assertReachable(dialog.getByRole('button', { name: 'Cancel linking' }))
  const finish = dialog.getByRole('button', { name: 'Finish linking' })
  await finish.waitFor()
  await assertReachable(finish)
  await finish.click()
  await dialog.getByRole('button', { name: 'Continue' }).click()
  await dialog.waitFor({ state: 'hidden' })
})

test('restore from first launch offline, then sign in with the restored membership', async (t) => {
  const page = await browser.newPage({ viewport: { width: 760, height: 600 } })
  t.after(() => page.close())
  page.setDefaultTimeout(5000)
  await page.addInitScript(() => {
    let restored = false
    let callback = 0
    const server = {
      id: '[games.example]:443',
      label: 'Friday Games',
      host: 'games.example',
      port: 443,
      httpsPort: 8443,
      profileUsername: 'jules',
      status: { kind: 'disconnected' },
      unread: 0,
    }
    window.profileCalls = []
    window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} }
    window.__TAURI_INTERNALS__ = {
      transformCallback: () => ++callback,
      invoke: async (command, args) => {
        window.profileCalls.push(command)
        switch (command) {
          case 'get_snapshot':
            return { servers: restored ? [server] : [] }
          case 'get_local_profile':
            return restored ? 'Jules' : null
          case 'choose_profile_backup':
            return { id: 'example-file', filename: 'My profile.sideband-backup' }
          case 'restore_profile_backup':
            restored = true
            return 'Jules'
          case 'probe_server':
            return {
              name: server.label,
              capabilities: ['profileSignIn'],
              hasPublicIcon: false,
              registrationMode: 'invite',
              trusted: true,
              publicCertificate: true,
              previousFingerprint: null,
              fingerprint: 'ab'.repeat(32),
            }
          case 'connect':
            window.profileSignIn = args.input
            throw { code: 'unreachable', message: 'Server is offline.' }
          case 'plugin:event|listen':
            return args.handler
          default:
            return null
        }
      },
    }
  })
  await page.goto(`http://127.0.0.1:${server.httpServer.address().port}`)
  await page.getByRole('button', { name: 'Restore from backup' }).click()
  await page.getByRole('button', { name: 'Choose backup file' }).click()
  await page.getByLabel('Recovery key').fill('synthetic test words')
  await page.getByRole('button', { name: 'Restore profile', exact: true }).click()
  await page.getByRole('heading', { name: 'Profile restored' }).waitFor()
  await page.getByRole('button', { name: 'Continue', exact: true }).click()
  assert.equal(await page.evaluate(() => window.profileCalls.includes('probe_server')), false)
  await page.getByRole('button', { name: 'Reconnect / log in' }).click()
  await page.getByRole('button', { name: 'Find server' }).click()
  const signIn = page.getByRole('button', { name: 'Sign in with profile' })
  await assertReachable(signIn)
  assert.equal(await page.getByLabel('Password', { exact: true }).count(), 0)
  assert.equal(await page.getByLabel('Invite code', { exact: true }).count(), 0)
  await signIn.click()
  await page.getByRole('alert').filter({ hasText: 'Cannot reach' }).waitFor()
  assert.deepEqual(await page.evaluate(() => window.profileSignIn), {
    host: 'games.example',
    port: 443,
    httpsPort: 8443,
    mode: 'profile',
    username: 'jules',
    password: '',
    inviteCode: '',
  })
})

test('open dock Audio by keyboard, return to the reply, and send it in the call', async (t) => {
  const { page, input, target, send, leave, audio } = await startReply(t)
  const hasFocus = () => audio.evaluate((el) => document.activeElement === el)
  for (let tabs = 0; tabs < 25 && !(await hasFocus()); tabs++) await page.keyboard.press('Tab')
  assert.ok(await hasFocus(), 'Tab from the composer should reach dock Audio')
  await page.keyboard.press('Enter')
  const dialog = page.getByRole('dialog', { name: 'Audio', exact: true })
  await dialog.waitFor()
  await page.keyboard.press('Escape')
  await dialog.waitFor({ state: 'hidden' })
  assert.ok(await hasFocus(), 'Escape should return focus to dock Audio')
  assert.equal(await input.inputValue(), draft)
  assert.ok(await target.isVisible())
  await assertReachable(send)
  await assertReachable(leave)
  await send.click()
  await target.waitFor({ state: 'hidden' })
  await page.getByRole('listitem').filter({ hasText: 'Meet you by the north entrance.' }).waitFor()
  const { messagesSent, leaves } = await page.evaluate(() => window.compactCall)
  assert.equal(messagesSent.length, 1)
  const { serverId, channelId, content, replyTo } = messagesSent[0]
  assert.deepEqual(
    { serverId, channelId, content, replyTo },
    { serverId: 'night', channelId: 'general', content: draft, replyTo: '01' },
  )
  assert.equal(await input.inputValue(), '')
  assert.equal(leaves, 0)
  await assertReachable(leave)
})

test('leave the call while keeping the two-line reply ready to send', async (t) => {
  const { page, dock, input, target, send, leave } = await startReply(t)
  await leave.click()
  await dock.waitFor({ state: 'hidden' })
  assert.deepEqual(await page.evaluate(() => window.compactCall), { messagesSent: [], leaves: 1 })
  assert.ok(await target.isVisible())
  assert.equal(await input.inputValue(), draft)
  await assertReachable(send)
})

for (const outcome of ['automatic', 'manual', 'leave while offline']) {
  test(`keep the reply through a dropped call: ${outcome}`, async (t) => {
    const { page, dock, input, target, send, leave } = await startReply(t)
    if (outcome === 'manual') {
      await dock.getByRole('button', { name: 'Mute', exact: true }).click()
      await dock.getByRole('button', { name: 'Unmute', exact: true }).waitFor()
    }
    await page.evaluate(() => window.roomConnection('reconnecting', 'waiting'))
    await dock.getByText('Your microphone is off.').waitFor()
    assert.ok(await send.isDisabled())
    assert.ok(await input.isEditable())
    await input.fill(draft + ' Bring headphones.')
    await input.press('Enter')
    assert.deepEqual(await page.evaluate(() => window.compactCall.messagesSent), [])
    if (outcome === 'leave while offline') {
      await leave.click()
      await dock.waitFor({ state: 'hidden' })
      await page.evaluate(() => window.roomConnection('connected', 'ended'))
    } else {
      await page.evaluate(() => window.roomConnection('restoring', 'waiting'))
      await dock.getByText(/Rejoining voice/).waitFor()
      await assertReachable(send)
      if (outcome === 'automatic') {
        await page.evaluate(() => window.roomConnection('connected', 'seated'))
      } else {
        await page.evaluate(() => window.roomConnection('connected', 'ended'))
        const rejoin = dock.getByRole('button', { name: 'Rejoin voice' })
        await rejoin.waitFor()
        assert.equal(await page.evaluate(() => window.rejoinRequest), undefined)
        await assertReachable(rejoin)
        await rejoin.click()
        await dock.getByRole('button', { name: 'Unmute', exact: true }).waitFor()
        assert.deepEqual(await page.evaluate(() => window.rejoinRequest), {
          serverId: 'night',
          channelId: 'voice',
          startMuted: true,
          startDeafened: false,
        })
      }
      await dock.getByText('In voice', { exact: true }).waitFor()
    }
    assert.ok(await target.isVisible())
    assert.equal(await input.inputValue(), draft + ' Bring headphones.')
    await assertReachable(send)
    await send.click()
    await target.waitFor({ state: 'hidden' })
    const sent = await page.evaluate(() => window.compactCall.messagesSent)
    assert.equal(sent.length, 1)
    assert.equal(sent[0].replyTo, '01')
    assert.equal(sent[0].content, draft + ' Bring headphones.')
    if (outcome === 'leave while offline') assert.ok(await dock.isHidden())
  })
}

test('create and copy an invitation from the compact server menu', async (t) => {
  const { page, target, input } = await startReply(t)
  await page.context().grantPermissions(['clipboard-read', 'clipboard-write'])
  await page.getByRole('button', { name: 'Channels', exact: true }).click()
  await page.getByRole('button', { name: 'Server menu for Night Shift' }).click()
  await page.getByRole('menuitem', { name: 'Invites', exact: true }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByRole('button', { name: 'Create invite' }).click()
  const copy = dialog.getByRole('button', { name: 'Copy invitation' })
  await copy.waitFor()
  await assertReachable(copy)
  await copy.click()
  await dialog.getByText('Invitation copied.').waitFor()
  const clipboard = await page.evaluate(() => navigator.clipboard.readText())
  assert.match(clipboard, /Join Night Shift on Sideband/)
  assert.match(clipboard, /Server: nightshift.example/)
  assert.match(clipboard, /Invite code: night-shift-test/)
  assert.match(clipboard, /Expires: .+2026/)
  await dialog.getByRole('button', { name: 'Back to invites' }).click()
  assert.ok(await dialog.getByText('night-shift-test').isHidden())
  await dialog.getByRole('button', { name: 'Close', exact: true }).click()
  assert.equal(await input.inputValue(), draft)
  assert.ok(await target.isVisible())
})
