import type { BridgeMessage } from '../../lib/generated/BridgeMessage'
export function day(timestamp: number) {
  return new Date(timestamp).toLocaleDateString()
}
export function grouped(previous: BridgeMessage | undefined, message: BridgeMessage) {
  return Boolean(
    previous &&
    message.authorId &&
    previous.authorId === message.authorId &&
    !message.replyTo &&
    day(previous.createdAt) === day(message.createdAt) &&
    message.createdAt - previous.createdAt >= 0 &&
    message.createdAt - previous.createdAt < 7 * 60_000,
  )
}
export function canEdit(message: BridgeMessage, own: string | null, now = Date.now()) {
  return own !== null && message.authorId === own && now - message.createdAt < 7 * 86400_000
}
export function atBottom(element: HTMLElement) {
  return element.scrollHeight - element.scrollTop - element.clientHeight <= 8
}
