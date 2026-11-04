import { invokeBridge } from '../../lib/tauri'
import type { InviteView } from '../../lib/generated/InviteView'
import type { CreatedInvite } from '../../lib/generated/CreatedInvite'
import type { CreateInviteInput } from '../../lib/generated/CreateInviteInput'
export const invitePage = (serverId: string, before: string | null = null) =>
  invokeBridge<InviteView[]>('invite_page', { serverId, before })
export const createInvite = (input: CreateInviteInput) =>
  invokeBridge<CreatedInvite>('create_server_invite', { input })
export const revokeInvite = (serverId: string, inviteId: string) =>
  invokeBridge<void>('revoke_server_invite', { serverId, inviteId })
export function inviteStatus(invite: InviteView, now: number) {
  if (invite.revokedAt !== null) return 'Revoked'
  if (invite.expiresAt !== null && invite.expiresAt <= now) return 'Expired'
  if (invite.maxUses !== null && invite.uses >= invite.maxUses) return 'Used up'
  return 'Active'
}
export function canRevoke(invite: InviteView, own: string, create: boolean, manage: boolean) {
  return invite.revokedAt === null && (invite.createdBy === own ? create : manage)
}
export function parseUses(value: string): number | null {
  if (!value.trim()) return null
  if (!/^\d+$/.test(value) || Number(value) < 1 || Number(value) > 4294967295)
    throw new Error(
      'Maximum uses must be a whole number between 1 and 4,294,967,295, or blank for unlimited.',
    )
  return Number(value)
}
