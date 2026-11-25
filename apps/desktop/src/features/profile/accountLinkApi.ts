import { invokeBridge } from '../../lib/tauri'
import type { AccountLinkReview } from '../../lib/generated/AccountLinkReview'

export const reviewServerAccount = (serverId: string) =>
  invokeBridge<AccountLinkReview>('review_profile_account', { serverId })

export const linkServerAccount = (serverId: string, userId: string, role: string) =>
  invokeBridge<void>('link_profile_account', { serverId, userId, role })
