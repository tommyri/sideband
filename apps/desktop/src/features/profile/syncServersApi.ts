import { invokeBridge } from '../../lib/tauri'
import type { SyncOptions } from '../../lib/generated/SyncOptions'

export const listSyncServers = () => invokeBridge<SyncOptions>('sync_server_list')
export const chooseSyncServer = (serverId: string) =>
  invokeBridge<void>('choose_sync_server', { serverId })
export const verifySyncCopy = () => invokeBridge<void>('check_sync_copy')
