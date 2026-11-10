import { invokeBridge } from '../../lib/tauri'
import type { Preferences } from './model'

export const notificationPreferences = () => invokeBridge<Preferences>('notification_preferences')
export const saveNotificationPreferences = (preferences: Preferences) =>
  invokeBridge<void>('save_notification_preferences', { preferences })
export const testNotification = () => invokeBridge<void>('test_notification')
