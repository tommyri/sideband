import type { ServerSettingsFields } from '../../lib/generated/ServerSettingsFields'
import type { ServerSettingsView } from '../../lib/generated/ServerSettingsView'
import { invokeBridge } from '../../lib/tauri'

export const serverSettings = (serverId: string) =>
  invokeBridge<ServerSettingsView>('get_server_settings', { serverId })
export const uploadServerIcon = (serverId: string, data: string) =>
  invokeBridge<string>('upload_server_icon', { serverId, data })
export const saveServerSettings = (serverId: string, fields: ServerSettingsFields) =>
  invokeBridge<ServerSettingsView>('save_server_settings', { serverId, fields })
