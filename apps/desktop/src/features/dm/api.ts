import { invokeBridge } from '../../lib/tauri'
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
