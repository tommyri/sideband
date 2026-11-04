import { invokeBridge } from '../../lib/tauri'
import type { BridgeMessage } from '../../lib/generated/BridgeMessage'
import type { ChatSnapshot } from '../../lib/generated/ChatSnapshot'
import type { ChatAction } from '../../lib/generated/ChatAction'
import type { AppEvent } from '../../lib/bindings'

export const chatSnapshot = (serverId: string, channelId: string) =>
  invokeBridge<ChatSnapshot>('chat_snapshot', { serverId, channelId })
export const loadHistory = (serverId: string, channelId: string, before: string | null) =>
  invokeBridge<BridgeMessage[]>('load_history', { request: { serverId, channelId, before } })
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
export function errorText(error: unknown): string {
  return error && typeof error === 'object' && 'message' in error
    ? String(error.message)
    : 'The request failed. Please try again.'
}
