import { invokeBridge } from '../../lib/tauri'
export const MAX_FILE_BYTES = 25 * 1024 * 1024
export type FileRequest = {
  transfer: string
  server: string
  channel?: string
  attachment?: string
  filename: string
  data?: string
  preview?: string
  save: boolean
}
export const transferFile = (request: FileRequest) =>
  invokeBridge<string>('attachment_transfer', { request })
export const cancelFile = (transfer: string) =>
  invokeBridge<void>('cancel_attachment', { transfer }).catch(() => {})
export async function fileData(file: File) {
  const bytes = new Uint8Array(await file.arrayBuffer())
  let binary = ''
  for (let start = 0; start < bytes.length; start += 8192)
    binary += String.fromCharCode(...bytes.subarray(start, start + 8192))
  return btoa(binary)
}
export function mediaKind(mime: string) {
  if (['image/png', 'image/jpeg', 'image/gif', 'image/webp'].includes(mime)) return 'image'
  if (['video/mp4', 'video/webm'].includes(mime)) return 'video'
  if (['audio/ogg', 'audio/mpeg', 'audio/wav', 'audio/webm', 'audio/mp4'].includes(mime))
    return 'audio'
  return null
}
export function fileSize(size: number) {
  return size < 1024
    ? `${size} B`
    : size < 1024 * 1024
      ? `${(size / 1024).toFixed(1)} KiB`
      : `${(size / 1024 / 1024).toFixed(1)} MiB`
}
export function mediaBlob(data: string, mime: string) {
  if (!mediaKind(mime)) throw new Error('This file type cannot be previewed.')
  const bytes = Uint8Array.from(atob(data), (c) => c.charCodeAt(0))
  return URL.createObjectURL(new Blob([bytes], { type: mime }))
}
