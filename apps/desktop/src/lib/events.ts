import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import type { AppEvent } from './bindings'
import { useAppStore } from '../store/app-store'
import { isTauri } from './tauri'
import { getSnapshot } from './api'

let subscription: Promise<UnlistenFn> | undefined

export function subscribeToClientEvents(): Promise<UnlistenFn> {
  if (!subscription) {
    subscription = isTauri()
      ? listen<AppEvent>('ss://event', ({ payload }) => {
          useAppStore.getState().receiveEvent(payload)
          if (
            ['connection', 'readStatesChanged', 'messagesChanged', 'resynced'].includes(
              payload.event.kind,
            )
          ) {
            void getSnapshot()
              .then((snapshot) => useAppStore.getState().hydrate(snapshot.servers))
              .catch(() => undefined)
          }
        })
      : Promise.resolve(() => undefined)
  }
  return subscription
}
