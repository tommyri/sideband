import { invokeBridge } from '../../lib/tauri'

export const getLocalProfile = () => invokeBridge<string | null>('get_local_profile')
export const createLocalProfile = (name: string) =>
  invokeBridge<string>('create_local_profile', { name })

export function suggestedUsername(name: string | null | undefined): string {
  const candidate = name?.trim().toLowerCase() ?? ''
  return /^[a-z0-9_.]{3,32}$/.test(candidate) ? candidate : ''
}
