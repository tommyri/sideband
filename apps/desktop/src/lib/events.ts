import { navigateMessage } from '../features/chat/navigation'
import { messageLink } from '../features/chat/rich-model'
import { receiveChatEvent } from '../features/chat/api'
import { observe, record } from '../features/testing/report'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import type { AppEvent } from './bindings'
import { useAppStore } from '../store/app-store'
import { isTauri } from './tauri'
import { getSnapshot } from './api'
import { scheduleVoiceRefresh, useVoiceStore } from '../features/voice/store'
import type { SpeakingView } from './generated/SpeakingView'

let subscription: Promise<UnlistenFn> | undefined

export function subscribeToClientEvents(): Promise<UnlistenFn> {
  if (!subscription) {
    subscription = isTauri()
      ? Promise.all([
          listen<{ server: string; channel: string; message: string }>(
            'ss://notification-activate',
            ({ payload }) => {
              try {
                navigateMessage(messageLink(payload.server, payload.channel, payload.message))
              } catch {
                /* Server was disconnected while activating. */
              }
            },
          ),
          listen<AppEvent>('ss://event', ({ payload }) => {
            receiveChatEvent(payload)
            observe(payload, useVoiceStore.getState().active?.serverId)
            useAppStore.getState().receiveEvent(payload)
            if (payload.event.kind === 'voiceStats') {
              const stats = payload.event.stats
              useVoiceStore.setState((s) => ({ stats: { ...s.stats, [payload.serverId]: stats } }))
            }
            if (payload.event.kind === 'voiceDevice') {
              const device = payload.event.state
              const message =
                device.kind === 'permissionDenied'
                  ? 'Microphone access was denied. Allow it in system settings and rejoin voice.'
                  : device.kind === 'unavailable'
                    ? device.message
                    : device.kind === 'silentInput'
                      ? 'The microphone is sending silence. Check permission and your input device.'
                      : device.kind === 'lowQualityInput'
                        ? 'The microphone opened in low-quality hands-free mode.'
                        : ''
              if (message) useVoiceStore.setState({ deviceNotice: message })
            }
            if (
              [
                'connection',
                'channelsChanged',
                'membersChanged',
                'voiceChanged',
                'resynced',
                'readStatesChanged',
                'messagesChanged',
              ].includes(payload.event.kind)
            )
              scheduleVoiceRefresh(payload.serverId)
            if (
              ['connection', 'readStatesChanged', 'messagesChanged', 'resynced'].includes(
                payload.event.kind,
              )
            ) {
              void getSnapshot()
                .then((snapshot) => useAppStore.getState().hydrate(snapshot.servers))
                .catch(() => undefined)
            }
          }),
          listen<string>('ss://test-shortcut', ({ payload }) =>
            record('configured-shortcut', { action: payload }),
          ),
          listen<SpeakingView>('ss://speaking', ({ payload }) => {
            const voice = useVoiceStore.getState()
            const own = voice.views[payload.serverId]?.ownUserId
            if (voice.active?.serverId === payload.serverId && own) {
              const before = voice.speaking[payload.serverId]?.includes(own) ?? false
              const after = payload.userIds.includes(own)
              if (before !== after) record('local-transmit-indicator', { active: after })
            }
            useVoiceStore.setState((s) => ({
              speaking: { ...s.speaking, [payload.serverId]: payload.userIds },
            }))
          }),
          listen('ss://voice-session', () => {
            for (const server of useAppStore.getState().servers) scheduleVoiceRefresh(server.id)
          }),
          listen<string>('ss://voice-error', ({ payload }) =>
            useVoiceStore.setState({ error: payload }),
          ),
        ]).then((listeners) => () => listeners.forEach((stop) => stop()))
      : Promise.resolve(() => undefined)
  }
  return subscription
}
