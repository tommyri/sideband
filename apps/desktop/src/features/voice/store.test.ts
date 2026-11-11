import { beforeEach, expect, it, vi } from 'vitest'
import { refreshVoice, useVoiceStore } from './store'
import { invokeBridge } from '../../lib/tauri'
import type { ActiveVoiceView } from '../../lib/generated/ActiveVoiceView'

vi.mock('../../lib/tauri', () => ({ invokeBridge: vi.fn(), isTauri: () => true }))
const active = { serverId: 'a', channelId: 'voice' } as ActiveVoiceView
beforeEach(() => {
  useVoiceStore.setState({ active, views: {} })
})

it('keeps the voice dock when a view refresh fails while the session runs', async () => {
  vi.mocked(invokeBridge).mockRejectedValue({ code: 'client', message: 'Invalid identifier.' })
  await refreshVoice('a')
  expect(useVoiceStore.getState().active).toBe(active)
})

it('drops the dock once the server is gone, which ends the session first', async () => {
  vi.mocked(invokeBridge).mockRejectedValue({
    code: 'disconnected',
    message: 'That server is not connected.',
  })
  await refreshVoice('a')
  expect(useVoiceStore.getState().active).toBeNull()
})
