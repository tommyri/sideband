import { create } from 'zustand'
import type { AppEvent } from './bindings'

// Why something happened on a server, from events the shell forwards:
// a kick or ban with its reason, server and client notices (a failed voice
// rejoin, a deferred resync) and unsolicited server errors. Kept per server,
// newest last, until dismissed or the server is connected again.
export interface ServerNotice {
  id: number
  tone: 'alert' | 'status'
  text: string
}
const KEEP = 5
let next = 0
export const useServerNotices = create<{
  notices: Record<string, ServerNotice[]>
  dismiss: (server: string, id: number) => void
}>((set) => ({
  notices: {},
  dismiss: (server, id) =>
    set((s) => ({
      notices: { ...s.notices, [server]: (s.notices[server] ?? []).filter((n) => n.id !== id) },
    })),
}))

function text(event: AppEvent['event']): Omit<ServerNotice, 'id'> | null {
  switch (event.kind) {
    case 'removed': {
      const what = event.banned
        ? 'You were banned from this server'
        : 'You were removed from this server'
      return { tone: 'alert', text: event.reason ? `${what}: ${event.reason}` : `${what}.` }
    }
    case 'notice':
      return { tone: 'status', text: event.message }
    case 'serverError': {
      const wait =
        event.retryAfterMs !== null && event.retryAfterMs > 0
          ? ` Try again in ${Math.ceil(event.retryAfterMs / 1000)} s.`
          : ''
      const message = event.message || `The server reported error ${event.code}`
      return { tone: 'status', text: `${message.replace(/\.$/, '')}.${wait}` }
    }
    default:
      return null
  }
}

export function receiveNotice({ serverId, event }: AppEvent) {
  // A new session: a removal no longer applies. Other notices stay, since a
  // failed voice rejoin is reported just before the reconnect completes.
  if (event.kind === 'connection' && event.status.kind === 'connected') {
    useServerNotices.setState((s) => ({
      notices: {
        ...s.notices,
        [serverId]: (s.notices[serverId] ?? []).filter((n) => n.tone !== 'alert'),
      },
    }))
    return
  }
  const notice = text(event)
  if (!notice) return
  useServerNotices.setState((s) => ({
    notices: {
      ...s.notices,
      [serverId]: [
        ...(s.notices[serverId] ?? []).filter((n) => n.text !== notice.text),
        { ...notice, id: ++next },
      ].slice(-KEEP),
    },
  }))
}
