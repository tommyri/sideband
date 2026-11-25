import { invokeBridge } from '../../../lib/tauri'

export type SavedBackup = { filename: string; words: string }
export type SelectedBackup = { id: string; filename: string }

export const saveBackup = () => invokeBridge<SavedBackup | null>('save_profile_backup')
export const selectBackup = () => invokeBridge<SelectedBackup | null>('choose_profile_backup')
export const restoreBackup = (id: string, words: string) =>
  invokeBridge<string>('restore_profile_backup', { id, words })
