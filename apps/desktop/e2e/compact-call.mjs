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
