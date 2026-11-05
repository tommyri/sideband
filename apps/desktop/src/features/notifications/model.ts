export type Level = 'all' | 'mentions' | 'none'
export type Preferences = {
  desktop: boolean
  previews: boolean
  dnd: boolean
  mutedServers: string[]
  channels: Record<string, Level>
}
export const defaults: Preferences = {
  desktop: false,
  previews: false,
  dnd: false,
  mutedServers: [],
  channels: {},
}
export function notificationLevel(p: Preferences, server: string, channel: string): Level {
  if (p.dnd || p.mutedServers.includes(server)) return 'none'
  return p.channels[`${server}/${channel}`] ?? 'mentions'
}
