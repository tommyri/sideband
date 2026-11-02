import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import type { AppEvent } from './bindings'
import { useAppStore } from '../store/app-store'
import { isTauri } from './tauri'

let subscription: Promise<UnlistenFn> | undefined

export function subscribeToClientEvents(): Promise<UnlistenFn> {
  if (!subscription) {
    subscription = isTauri()
      ? listen<AppEvent>('ss://event', ({ payload }) =>
          useAppStore.getState().receiveEvent(payload),
        )
      : Promise.resolve(() => undefined)
  }
  return subscription
}
