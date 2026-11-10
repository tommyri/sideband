import type { ModerationAction } from '../../lib/generated/ModerationAction'
import type { ModerationPage } from '../../lib/generated/ModerationPage'
import { invokeBridge } from '../../lib/tauri'

export interface ModerationPageRequest {
  serverId: string
  bans: boolean
  before: string | null | undefined
  query: string
}
export const moderationPage = (request: ModerationPageRequest) =>
  invokeBridge<ModerationPage>('get_moderation_page', { ...request })
export const moderateMember = (serverId: string, action: ModerationAction) =>
  invokeBridge<void>('moderate_member', { serverId, action })
