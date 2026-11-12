import type { BridgeMessage } from '../../lib/generated/BridgeMessage'
export function day(timestamp: number) {
  return new Date(timestamp).toLocaleDateString()
}
export function grouped(previous: BridgeMessage | undefined, message: BridgeMessage) {
  return Boolean(
    previous &&
    !previous.deleted &&
    !message.deleted &&
    message.authorId &&
    previous.authorId === message.authorId &&
    !message.replyTo &&
    day(previous.createdAt) === day(message.createdAt) &&
    message.createdAt - previous.createdAt >= 0 &&
    message.createdAt - previous.createdAt < 7 * 60_000,
  )
}
export function canEdit(message: BridgeMessage, own: string | null, now = Date.now()) {
  return (
    !message.deleted &&
    own !== null &&
    message.authorId === own &&
    now - message.createdAt < 7 * 86400_000
  )
}
export function atBottom(element: HTMLElement) {
  return element.scrollHeight - element.scrollTop - element.clientHeight <= 8
}

/**
 * Where the newest run scrolls when it lost its oldest messages under the
 * reader. A resync over its page budget drops older cached messages (T-089,
 * T-107). The virtualizer keeps a row that is still loaded in place, but when
 * the row being read went too it leaves the offset as it was, over rows the
 * reader never saw. Then stay at the end if the reader was there, or else
 * show the oldest message still loaded, with "Load older" above it.
 */
export function afterTrim(
  previous: readonly BridgeMessage[],
  next: readonly BridgeMessage[],
  reading: string | null,
  atEnd: boolean,
): 'end' | 'start' | null {
  const first = previous[0]
  if (!first || !next.length || reading === null) return null
  const kept = new Set(next.map((m) => m.id))
  if (kept.has(reading) || kept.has(first.id)) return null
  return atEnd ? 'end' : 'start'
}
