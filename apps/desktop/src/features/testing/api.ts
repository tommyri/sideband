import { invokeBridge } from '../../lib/tauri'
import type { Json } from './report'

export interface ExportFile {
  name: string
  bytes: number[]
}
export const buildIdentity = () => invokeBridge<Json>('test_build_identity')
/** Writes a ZIP to the Downloads folder and returns its path. */
export const exportTestReport = (
  files: ExportFile[],
  consent: boolean,
  includeRecording: boolean,
) => invokeBridge<string>('export_test_report', { files, consent, includeRecording })
