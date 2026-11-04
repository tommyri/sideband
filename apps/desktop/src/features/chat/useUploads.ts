import { useEffect, useRef, useState } from 'react'
import { cancelFile, fileData, MAX_FILE_BYTES, transferFile } from './attachments'
import { errorText } from './api'
export type Upload = {
  key: string
  file: File
  status: 'reading' | 'uploading' | 'ready' | 'error'
  id?: string
  error?: string
}
const drafts = new Map<string, Upload[]>()
export function useUploads(identity: string, server: string, channel: string) {
  const [items, setItems] = useState<Upload[]>(() => drafts.get(identity) ?? [])
  const current = useRef(items)
  const mounted = useRef(true)
  const update = (next: Upload[]) => {
    current.current = next
    drafts.set(identity, next)
    if (mounted.current) setItems(next)
  }
  useEffect(() => {
    mounted.current = true
    return () => {
      mounted.current = false
      const next = current.current.map((item): Upload => {
        if (item.status !== 'reading' && item.status !== 'uploading') return item
        void cancelFile(item.key)
        return { ...item, status: 'error', error: 'Upload interrupted. Retry to continue.' }
      })
      current.current = next
      drafts.set(identity, next)
    }
  }, [identity])
  const upload = async (item: Upload) => {
    const live = () =>
      mounted.current &&
      current.current.some(
        (i) => i.key === item.key && (i.status === 'reading' || i.status === 'uploading'),
      )
    try {
      if (!live()) return
      const data = await fileData(item.file)
      if (!live()) return
      update(current.current.map((i) => (i.key === item.key ? { ...i, status: 'uploading' } : i)))
      const id = await transferFile({
        transfer: item.key,
        server,
        channel,
        filename: item.file.name,
        data,
        save: false,
      })
      if (live())
        update(current.current.map((i) => (i.key === item.key ? { ...i, id, status: 'ready' } : i)))
    } catch (e) {
      if (live())
        update(
          current.current.map((i) =>
            i.key === item.key ? { ...i, status: 'error', error: errorText(e) } : i,
          ),
        )
    }
  }
  const add = (files: File[]) => {
    if (files.length + current.current.length > 10)
      throw new Error('You can attach up to 10 files per message.')
    if (files.some((f) => !f.size || f.size > MAX_FILE_BYTES))
      throw new Error('Files must be between 1 byte and 25 MiB.')
    const next: Upload[] = files.map((file) => ({
      key: crypto.randomUUID(),
      file,
      status: 'reading',
    }))
    update([...current.current, ...next])
    void (async () => {
      for (const item of next) await upload(item)
    })()
  }
  const remove = (key: string) => {
    update(current.current.filter((i) => i.key !== key))
    void cancelFile(key)
  }
  const retry = (key: string) => {
    const old = current.current.find((i) => i.key === key)
    if (!old || old.status !== 'error') return
    const item: Upload = { file: old.file, key: crypto.randomUUID(), status: 'reading' }
    update(current.current.map((i) => (i.key === key ? item : i)))
    void upload(item)
  }
  return {
    items,
    add,
    remove,
    retry,
    clear: () => update([]),
    ready: items.every((i) => i.status === 'ready'),
  }
}
