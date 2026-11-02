import { invokeBridge } from '../../lib/tauri'
import type { MemberAction } from '../../lib/generated/MemberAction'
import type { ServerView } from '../../lib/generated/ServerView'

export const getServerView = (serverId: string) =>
  invokeBridge<ServerView>('get_server_view', { serverId })
export const joinVoice = (serverId: string, channelId: string) =>
  invokeBridge<void>('join_voice', { serverId, channelId })
export const leaveVoice = () => invokeBridge<void>('leave_voice')
export const voiceControl = (action: 'mute' | 'deafen' | 'ptt', enabled: boolean) =>
  invokeBridge<void>('voice_control', { action, enabled })
export const memberAction = (input: MemberAction) => invokeBridge<void>('member_action', { input })
