import type { ServerSettingsFields } from '../../lib/generated/ServerSettingsFields'
export function validServerSettings(fields: ServerSettingsFields) {
  if (!fields.name.trim() || [...fields.name.trim()].length > 100)
    return 'Name must be 1 to 100 characters.'
  if ([...fields.description].length > 2048) return 'Description must be at most 2048 characters.'
  if (!['invite', 'closed'].includes(fields.registrationMode)) return 'Choose a registration mode.'
  return ''
}
