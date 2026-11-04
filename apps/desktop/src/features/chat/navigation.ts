import { create } from 'zustand'
import { useAppStore } from '../../store/app-store'
import { useVoiceStore } from '../voice/store'
import { parseMessageLink } from './rich-model'
export const useChatNavigation = create<{
  request: { server: string; channel: string; message: string; sequence: number } | null
}>(() => ({ request: null }))
export function navigateMessage(value: string) {
  const target = parseMessageLink(value)
  if (!target) throw new Error('Invalid message link.')
  if (!useAppStore.getState().servers.some((s) => s.id === target.server))
    throw new Error('Connect to this message’s server first.')
  useAppStore.getState().selectServer(target.server)
  useVoiceStore.getState().select(target.server, target.channel)
  useChatNavigation.setState((s) => ({
    request: { ...target, sequence: (s.request?.sequence ?? 0) + 1 },
  }))
}
// Session-only drafts, isolated by server, account and conversation. Nothing is
// written to disk; leaving a channel temporarily does not discard typed work.
const drafts = new Map<string, string>()
export const draftKey = (server: string, user: string, channel: string) =>
  JSON.stringify([server, user, channel])
export function getDraft(key: string) {
  return drafts.get(key) ?? ''
}
export function saveDraft(key: string, value: string) {
  if (value) drafts.set(key, value)
  else drafts.delete(key)
}

const attempts = new Map<string, { identity: string; nonce: string }>()
export function draftNonce(key: string, identity: string) {
  if (attempts.get(key)?.identity !== identity)
    attempts.set(key, { identity, nonce: crypto.randomUUID() })
  return attempts.get(key)!.nonce
}
export function clearDraftNonce(key: string) {
  attempts.delete(key)
}
