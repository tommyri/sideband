import { invokeBridge } from '../../lib/tauri'
import type { BridgeMessage } from '../../lib/generated/BridgeMessage'
import type { ChatSnapshot } from '../../lib/generated/ChatSnapshot'
import type { ChatAction } from '../../lib/generated/ChatAction'
import type { AppEvent } from '../../lib/bindings'

// `around` picks the run of loaded messages holding that message; without it
// the newest run is shown.
export const chatSnapshot = (serverId: string, channelId: string, around: string | null = null) =>
  invokeBridge<ChatSnapshot>('chat_snapshot', { serverId, channelId, around })
export const loadHistory = (serverId: string, channelId: string, before: string | null) =>
  invokeBridge<BridgeMessage[]>('load_history', { request: { serverId, channelId, before } })
export const loadNewer = (serverId: string, channelId: string, messageId: string) =>
  invokeBridge<BridgeMessage[]>('load_newer', { serverId, channelId, messageId })
export const chatAction = (serverId: string, action: ChatAction) =>
  invokeBridge<void>('chat_action', { serverId, action })

const listeners = new Set<(event: AppEvent) => void>()
export function receiveChatEvent(event: AppEvent) {
  for (const listener of listeners) listener(event)
}
export function onChatEvent(listener: (event: AppEvent) => void) {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}
export { errorText } from '../../lib/errors'

export const messageContext = (serverId: string, channelId: string, messageId: string) =>
  invokeBridge<void>('message_context', { serverId, channelId, messageId })
export const openChatLink = (url: string) => invokeBridge<void>('open_chat_link', { url })

const typingSent = new Map<string, number>()
export function notifyTyping(server: string, channel: string) {
  const key = JSON.stringify([server, channel])
  const now = Date.now()
  if (now - (typingSent.get(key) ?? -Infinity) < 5000) return
  typingSent.set(key, now)
  void chatAction(server, { kind: 'typing', channel }).catch(() => {})
}
