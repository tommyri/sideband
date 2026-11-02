import { invoke } from '@tauri-apps/api/core'

export const isTauri = () => '__TAURI_INTERNALS__' in window

export async function invokeBridge<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauri()) {
    throw new Error('The native bridge is unavailable in browser preview mode.')
  }

  return invoke<T>(command, args)
}
