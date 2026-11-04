import { invokeBridge } from '../../lib/tauri'
import type { ChannelAdminSnapshot } from '../../lib/generated/ChannelAdminSnapshot'
import type { ChannelAdminAction } from '../../lib/generated/ChannelAdminAction'
import type { ChannelFields } from '../../lib/generated/ChannelFields'
import type { ChannelProfile } from '../../lib/generated/ChannelProfile'
export const channelSnapshot = (serverId: string) =>
  invokeBridge<ChannelAdminSnapshot>('channel_admin_snapshot', { serverId })
export const channelAction = (serverId: string, action: ChannelAdminAction) =>
  invokeBridge<void>('channel_admin_action', { serverId, action })
export const channelKind = (kind: number) =>
  kind === 1 ? 'Category' : kind === 3 ? 'Voice' : 'Text'
export function validFields(
  fields: ChannelFields,
  kind: number,
  profiles: ChannelProfile[],
): ChannelFields {
  const name = fields.name.trim()
  if (!name || [...name].length > 64) throw new Error('Name must be 1 to 64 characters.')
  if ([...fields.topic].length > 1024) throw new Error('Topic must be at most 1024 characters.')
  if (kind === 3) {
    const profile = profiles.find((p) => p.id === fields.profile)
    if (!profile) throw new Error('Choose a voice profile.')
    if (
      fields.bitrate !== null &&
      (!Number.isInteger(fields.bitrate) ||
        fields.bitrate < profile.minBitrate ||
        fields.bitrate > profile.maxBitrate)
    )
      throw new Error('Bitrate is outside the profile range.')
    if (
      fields.userLimit !== null &&
      (!Number.isInteger(fields.userLimit) || fields.userLimit < 0 || fields.userLimit > 99)
    )
      throw new Error('User limit must be 0 to 99, or blank for unlimited.')
  }
  return { ...fields, name }
}
