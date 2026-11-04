import { invokeBridge } from '../../lib/tauri'
import type { WebhookView } from '../../lib/generated/WebhookView'
import type { CreatedWebhook } from '../../lib/generated/CreatedWebhook'
export const webhookPage = (serverId: string, before: string | null = null) =>
  invokeBridge<WebhookView[]>('webhook_page', { serverId, before })
export const createWebhook = (serverId: string, channelId: string, name: string) =>
  invokeBridge<CreatedWebhook>('create_server_webhook', { serverId, channelId, name })
export const revokeWebhook = (serverId: string, webhookId: string) =>
  invokeBridge<void>('revoke_server_webhook', { serverId, webhookId })
export function webhookName(value: string) {
  const name = value.trim()
  if (!name || [...name].length > 80) throw new Error('Name must be 1 to 80 characters.')
  return name
}
