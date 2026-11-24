import { invokeBridge } from '../../lib/tauri'
import { loadDmPage } from '../../lib/api'
import type { DmAction } from '../../lib/generated/DmAction'
import type { DmResult } from '../../lib/generated/DmResult'
import { refreshVoice, useVoiceStore } from '../voice/store'

export const dmAction = (serverId: string, action: DmAction) =>
  invokeBridge<DmResult>('dm_action', { serverId, action })
export async function openDm(server: string, user: string) {
  const result = await dmAction(server, { kind: 'create', userIds: [user], name: '' })
  await refreshVoice(server)
  useVoiceStore.getState().select(server, result.channelId)
}
// Load the next page of older conversations, then reread the server view,
// which lists them. Resolves to whether more remain.
export async function loadOlderDms(server: string) {
  const more = await loadDmPage(server)
  await refreshVoice(server)
  return more
}
