import { invokeBridge } from '../../lib/tauri'

export type UpdateStatus = { enabled: boolean; version: string | null; notes: string | null }
export const checkUpdate = () => invokeBridge<UpdateStatus>('check_update')
export const installUpdate = () => invokeBridge<void>('install_update')
