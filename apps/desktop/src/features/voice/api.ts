import { record } from '../testing/report'
import { invokeBridge } from '../../lib/tauri'
import type { MemberAction } from '../../lib/generated/MemberAction'
import type { ServerView } from '../../lib/generated/ServerView'

export const getServerView = (serverId: string) =>
  invokeBridge<ServerView>('get_server_view', { serverId })
export const joinVoice = (serverId: string, channelId: string) =>
  invokeBridge<void>('join_voice', { serverId, channelId })
export const leaveVoice = () => invokeBridge<void>('leave_voice')
// Each invoke runs as its own native task, so a fast tap could apply release
// before press and leave the gate open. Send each PTT change only after the
// previous one settled.
let pttQueue: Promise<unknown> = Promise.resolve()
export const voiceControl = (action: 'mute' | 'deafen' | 'ptt', enabled: boolean) => {
  record('local-voice-control', { action, enabled })
  if (action !== 'ptt') return invokeBridge<void>('voice_control', { action, enabled })
  const next = pttQueue.then(() => invokeBridge<void>('voice_control', { action, enabled }))
  pttQueue = next.catch(() => {})
  return next
}
export const memberAction = (input: MemberAction) => invokeBridge<void>('member_action', { input })
