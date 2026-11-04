import { useCallback, useEffect, useRef, useState } from 'react'
import type { ChatSnapshot } from '../../lib/generated/ChatSnapshot'
import { chatSnapshot, errorText, loadHistory, onChatEvent } from './api'

// The native cache owns ordering, identity and event reconciliation. Fetch a
// fresh projection, never merge stale HTTP pages over newer live events.
export function useChat(server: string, channel: string) {
  const [view, setView] = useState<ChatSnapshot>()
  const [error, setError] = useState('')
  const [loading, setLoading] = useState(true)
  const [hasOlder, setHasOlder] = useState(true)
  const refresh = useRef<() => Promise<void>>(async () => {})
  const page = useRef<(before: string | null) => Promise<void>>(async () => {})
  useEffect(() => {
    let alive = true
    let running: Promise<void> | undefined
    let dirty = false
    let paging = false
    const update = (): Promise<void> => {
      dirty = true
      if (running) return running
      running = (async () => {
        while (dirty && alive) {
          dirty = false
          try {
            const next = await chatSnapshot(server, channel)
            if (alive) setView(next)
          } catch (e) {
            if (alive) {
              setError(errorText(e))
              setView(undefined)
            }
          }
        }
      })().finally(() => {
        running = undefined
      })
      return running
    }
    refresh.current = update
    page.current = async (before) => {
      if (paging || !alive) return
      paging = true
      setLoading(true)
      setError('')
      try {
        const result = await loadHistory(server, channel, before)
        if (alive) setHasOlder(result.length === 50)
        await update()
      } catch (e) {
        if (alive) setError(errorText(e))
      } finally {
        paging = false
        if (alive) setLoading(false)
      }
    }
    const stop = onChatEvent(({ serverId, event }) => {
      if (serverId !== server) return
      if (
        (event.kind === 'messagesChanged' && event.channel === channel) ||
        ['resynced', 'readStatesChanged', 'channelsChanged', 'connection'].includes(event.kind)
      ) {
        void update()
      }
    })
    void page.current(null)
    return () => {
      alive = false
      stop()
    }
  }, [server, channel])
  return {
    view,
    error,
    setError,
    loading,
    hasOlder,
    refresh: useCallback(() => refresh.current(), []),
    load: useCallback((before: string | null) => page.current(before), []),
  }
}
