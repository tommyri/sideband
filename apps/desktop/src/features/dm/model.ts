import type { DmView } from '../../lib/generated/DmView'
import type { MemberView } from '../../lib/generated/MemberView'

export function dmName(dm: DmView, members: Map<string, MemberView>, own: string) {
  if (dm.kind === 5 && dm.name.trim()) return dm.name
  return (
    dm.memberIds
      .filter((id) => id !== own)
      .map(
        (id) =>
          members.get(id)?.displayName ||
          members.get(id)?.username ||
          `Former member (${id.slice(-6)})`,
      )
      .join(', ') || 'Group conversation'
  )
}
export function sortedDms(dms: DmView[]) {
  return [...dms].sort((a, b) => b.lastActivity - a.lastActivity || b.id.localeCompare(a.id))
}
export function dmOnline(dm: DmView, members: Map<string, MemberView>, own: string) {
  return dm.memberIds.some(
    (id) =>
      id !== own && ['online', 'idle', 'do not disturb'].includes(members.get(id)?.presence ?? ''),
  )
}
export function canAddMembers(dm: DmView, own: string) {
  return (
    dm.kind === 5 && dm.creatorId === own && dm.memberIds.includes(own) && dm.memberIds.length < 10
  )
}
