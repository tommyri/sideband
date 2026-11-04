import { invokeBridge } from '../../lib/tauri'
import type { RoleSnapshot } from '../../lib/generated/RoleSnapshot'
import type { RoleAction } from '../../lib/generated/RoleAction'
export type Override = { roleId: string; allow: string; deny: string }
export const roleSnapshot = (serverId: string) =>
  invokeBridge<RoleSnapshot>('role_admin_snapshot', { serverId })
export const roleAction = (serverId: string, action: RoleAction) =>
  invokeBridge<void>('role_admin_action', { serverId, action })
export const roleOverrides = (serverId: string, channel: string) =>
  invokeBridge<Override[]>('role_overrides', { serverId, channel })
export const permissions = [
  ['Administrator', 'Server', 'Bypasses all permissions, channel overrides and role hierarchy.'],
  ['Manage server', 'Server', 'Manage server settings and webhooks.'],
  ['Manage channels', 'Server', 'Create, edit and delete channels and their overrides.'],
  ['Manage roles', 'Server', 'Edit and assign roles below your highest role.'],
  ['Manage messages', 'Moderation', 'Moderate messages in this channel.'],
  ['Kick members', 'Moderation', 'Remove lower-ranked members.'],
  ['Ban members', 'Moderation', 'Ban lower-ranked members.'],
  ['Mute members', 'Moderation', 'Server-mute lower-ranked members.'],
  ['Move members', 'Moderation', 'Move lower-ranked members between voice channels.'],
  ['View channel', 'Text and voice', 'See this channel.'],
  ['Send messages', 'Text and voice', 'Post messages; also requires View channel.'],
  [
    'Attach files',
    'Text and voice',
    'Attach uploads; also requires View channel and Send messages.',
  ],
  ['Mention everyone', 'Text and voice', 'Notify everyone; also requires Send messages.'],
  ['Connect', 'Text and voice', 'Join voice; also requires View channel.'],
  ['Speak', 'Text and voice', 'Send voice; also requires Connect and View channel.'],
  ['Stream', 'Text and voice', 'Share video; also requires Connect and View channel.'],
  ['Create invites', 'Server', 'Create server invitations.'],
  ['View audit log', 'Server', 'Read administrative activity.'],
  ['Change nickname', 'Server', 'Edit your own profile.'],
] as const
export const channelBits = [4, 9, 10, 11, 12, 13, 14, 15] as const
export const has = (mask: string, bit: number) => (BigInt(mask) & (1n << BigInt(bit))) !== 0n
export const toggle = (mask: string, bit: number, on: boolean) => {
  const flag = 1n << BigInt(bit)
  return (on ? BigInt(mask) | flag : BigInt(mask) & ~flag).toString()
}
export function overrideValue(value: Override, bit: number) {
  return has(value.allow, bit) ? 'allow' : has(value.deny, bit) ? 'deny' : 'inherit'
}
export function setOverride(value: Override, bit: number, mode: string): Override {
  return {
    ...value,
    allow: toggle(value.allow, bit, mode === 'allow'),
    deny: toggle(value.deny, bit, mode === 'deny'),
  }
}
