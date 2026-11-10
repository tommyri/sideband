import { invokeBridge } from '../../lib/tauri'
import type { Snapshot } from './model'

export const diagnosticsSnapshot = (serverId: string | null) =>
  invokeBridge<Snapshot>('diagnostics_snapshot', { serverId })
