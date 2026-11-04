import { useEffect, useState } from 'react'
import { onChatEvent } from './api'
export function useTyping(server: string, channel: string, own: string | null) {
  const [users, setUsers] = useState<Record<string, number>>({})
  useEffect(() => {
    const stop = onChatEvent(({ serverId, event }) => {
      if (serverId !== server) return
      if (event.kind === 'connection' || event.kind === 'resynced') setUsers({})
      if (event.kind === 'typing' && event.channel === channel && event.user !== own)
        setUsers((old) => ({ ...old, [event.user]: Date.now() + 6000 }))
    })
    const timer = setInterval(
      () =>
        setUsers((old) => {
          const next = Object.fromEntries(
            Object.entries(old).filter(([, expiry]) => expiry > Date.now()),
          )
          return Object.keys(next).length === Object.keys(old).length ? old : next
        }),
      1000,
    )
    return () => {
      stop()
      clearInterval(timer)
    }
  }, [server, channel, own])
  return Object.keys(users)
}
