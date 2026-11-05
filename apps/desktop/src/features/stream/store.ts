import { useEffect } from 'react'
import { create } from 'zustand'
import { isTauri } from '../../lib/tauri'
import { snapshot, type Snapshot } from './api'
export const useStreams = create<{ views: Record<string, Snapshot> }>(() => ({ views: {} }))
const observers = new Map<string, { count: number; stop: () => void }>()
export function useStreamPolling(server?: string) {
  useEffect(() => {
    if (!server || !isTauri()) return
    let observer = observers.get(server)
    if (!observer) {
      let live = true
      let timer: ReturnType<typeof setTimeout> | undefined
      const poll = async () => {
        try {
          const view = await snapshot(server)
          if (live)
            useStreams.setState((s) =>
              JSON.stringify(s.views[server]) === JSON.stringify(view)
                ? s
                : { views: { ...s.views, [server]: view } },
            )
        } catch {
          if (live)
            useStreams.setState((s) => {
              const views = { ...s.views }
              delete views[server]
              return { views }
            })
        }
        if (live) timer = setTimeout(() => void poll(), 500)
      }
      observer = {
        count: 0,
        stop: () => {
          live = false
          clearTimeout(timer)
        },
      }
      observers.set(server, observer)
      void poll()
    }
    observer.count++
    return () => {
      if (--observer.count === 0) {
        observer.stop()
        observers.delete(server)
      }
    }
  }, [server])
}
