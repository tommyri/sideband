import { useCallback, useEffect, useRef, useState } from 'react'
import type { ChatSnapshot } from '../../lib/generated/ChatSnapshot'
import { chatSnapshot, errorText, loadHistory, loadNewer, onChatEvent } from './api'

// The native cache owns ordering, identity and event reconciliation. Fetch a
// fresh projection, never merge stale HTTP pages over newer live events.
//
// The projection is one run of loaded messages with nothing missing inside:
// the newest run, or after a jump the run around the jumped-to message
// (`show`). `loadNewer` extends that run until it reaches the newest.
export function useChat(server: string, channel: string) {
  const [view, setView] = useState<ChatSnapshot>()
  const [error, setError] = useState('')
  const [loading, setLoading] = useState(true)
  const refresh = useRef<() => Promise<void>>(async () => {})
  const page = useRef<(before: string | null) => Promise<void>>(async () => {})
  const newer = useRef<(after: string) => Promise<void>>(async () => {})
  const show = useRef<(around: string | null) => Promise<void>>(async () => {})
  useEffect(() => {
    let alive = true
    let running: Promise<void> | undefined
    let dirty = false
    let paging = false
    let around: string | null = null
    // Only an error from reading the projection is cleared by the next
    // successful read; a failed page stays reported until the next attempt.
    let snapshotFailed = false
    const update = (): Promise<void> => {
      dirty = true
      if (running) return running
      running = (async () => {
        while (dirty && alive) {
          dirty = false
          try {
            const next = await chatSnapshot(server, channel, around)
            if (!alive) break
            // Once the jumped-to run reaches the newest, it is the newest run.
            if (next.latest) around = null
            setView(next)
            if (snapshotFailed) {
              snapshotFailed = false
              setError('')
            }
          } catch (e) {
            if (alive) {
              snapshotFailed = true
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
    const run = async (request: () => Promise<unknown>) => {
      if (paging || !alive) return
      paging = true
      setLoading(true)
      setError('')
      snapshotFailed = false
      try {
        await request()
      } catch (e) {
        if (alive) setError(errorText(e))
      } finally {
        paging = false
        // Show what is cached even when the page failed.
        await update()
        if (alive) setLoading(false)
      }
    }
    refresh.current = update
    page.current = (before) => run(() => loadHistory(server, channel, before))
    newer.current = (after) => run(() => loadNewer(server, channel, after))
    show.current = (target) => {
      around = target
      return update()
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
    hasOlder: !view?.start,
    refresh: useCallback(() => refresh.current(), []),
    load: useCallback((before: string | null) => page.current(before), []),
    loadNewer: useCallback((after: string) => newer.current(after), []),
    show: useCallback((around: string | null) => show.current(around), []),
  }
}
