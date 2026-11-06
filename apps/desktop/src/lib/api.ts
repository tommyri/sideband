import type {
  AppSnapshot,
  ApproveFingerprintInput,
  ConnectInput,
  ConnectResult,
  CreateChannelInput,
  SendMessageInput,
  ServerSummary,
  ProbeInput,
  ServerProbe,
} from './bindings'
import { invokeBridge, isTauri } from './tauri'

const previewServer: ServerSummary = {
  id: 'preview.local:5781',
  label: 'Product Studio',
  host: 'preview.local',
  port: 5781,
  httpsPort: 5781,
  unread: 0,
  status: { kind: 'connected' },
}

export async function getSnapshot(): Promise<AppSnapshot> {
  if (!isTauri()) return { servers: [previewServer] }
  return invokeBridge<AppSnapshot>('get_snapshot')
}

export async function connect(input: ConnectInput): Promise<ConnectResult> {
  if (!isTauri()) {
    throw new Error('Open the installed desktop app to connect to a real server.')
  }
  return invokeBridge<ConnectResult>('connect', { input })
}

export async function approveFingerprint(input: ApproveFingerprintInput): Promise<void> {
  return invokeBridge<void>('approve_fingerprint', { input })
}

export function probeServer(input: ProbeInput): Promise<ServerProbe> {
  return invokeBridge('probe_server', { input })
}
export function enablePublicCertificate(input: ProbeInput): Promise<ServerProbe> {
  return invokeBridge('use_public_certificate', { input })
}
export function removeServer(serverId: string): Promise<void> {
  return invokeBridge('remove_server', { serverId })
}
export function reorderServers(serverIds: string[]): Promise<void> {
  return invokeBridge('reorder_servers', { serverIds })
}

export async function disconnect(serverId: string): Promise<void> {
  if (!isTauri()) return
  return invokeBridge<void>('disconnect', { serverId })
}

export function sendMessage(input: SendMessageInput): Promise<string> {
  return invokeBridge<string>('send_message', { input })
}

export function createChannel(input: CreateChannelInput): Promise<string> {
  return invokeBridge<string>('create_channel', { input })
}
