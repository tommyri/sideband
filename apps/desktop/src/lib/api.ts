import type {
  AppSnapshot,
  ApproveFingerprintInput,
  ConnectInput,
  ConnectResult,
  CreateChannelInput,
  SendMessageInput,
  ServerSummary,
} from './bindings'
import { invokeBridge, isTauri } from './tauri'

const previewServer: ServerSummary = {
  id: 'preview.local:5781',
  label: 'Product Studio',
  host: 'preview.local',
  status: { kind: 'connected' },
}

export async function getSnapshot(): Promise<AppSnapshot> {
  if (!isTauri()) return { servers: [previewServer] }
  return invokeBridge<AppSnapshot>('get_snapshot')
}

export async function connect(input: ConnectInput): Promise<ConnectResult> {
  if (!isTauri()) {
    await new Promise((resolve) => window.setTimeout(resolve, 420))
    return {
      serverId: `${input.host}:${input.port}`,
      label: input.host === '127.0.0.1' ? 'Local SocialSpeak' : input.host,
      status: { kind: 'connected' },
    }
  }
  return invokeBridge<ConnectResult>('connect', { input })
}

export async function approveFingerprint(input: ApproveFingerprintInput): Promise<void> {
  return invokeBridge<void>('approve_fingerprint', { input })
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
