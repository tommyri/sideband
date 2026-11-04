import { invokeBridge } from '../../lib/tauri'
import type { AuditEntryView } from '../../lib/generated/AuditEntryView'
import type { AuditQuery } from '../../lib/generated/AuditQuery'
export const auditPage = (query: AuditQuery) =>
  invokeBridge<AuditEntryView[]>('audit_page', { query })
export type AuditFilters = Pick<AuditQuery, 'actor' | 'action' | 'since' | 'until'>
export const emptyFilters: AuditFilters = { actor: null, action: null, since: null, until: null }
export function auditFilters(
  actor: string,
  action: string,
  since: string,
  until: string,
): AuditFilters {
  actor = actor.trim()
  action = action.trim()
  if (actor && !/^[0-7][0-9A-HJKMNP-TV-Z]{25}$/i.test(actor))
    throw new Error('Actor must be a valid user ID.')
  if (new TextEncoder().encode(action).length > 128)
    throw new Error('Action must be at most 128 bytes.')
  const stamp = (value: string) => {
    if (!value) return null
    const number = new Date(value).getTime()
    if (!Number.isFinite(number)) throw new Error('Choose a valid date and time.')
    return number
  }
  const from = stamp(since)
  const to = stamp(until)
  if (from !== null && to !== null && from >= to)
    throw new Error('From must be earlier than Before.')
  return { actor: actor || null, action: action || null, since: from, until: to }
}
