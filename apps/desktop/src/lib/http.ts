import type { BridgeMessage, HistoryRequest } from './bindings'
import { invokeBridge } from './tauri'

/** HTTPS history stays behind the Rust QUIC-pin and bearer-token boundary. */
export function loadHistory(request: HistoryRequest): Promise<BridgeMessage[]> {
  return invokeBridge<BridgeMessage[]>('load_history', { request })
}
